use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

fn read_packet(stream: &mut TcpStream) -> Vec<u8> {
    let mut byte = [0];
    stream.read_exact(&mut byte).unwrap();
    let mut packet = vec![byte[0]];
    let mut length = 0;
    let mut shift = 0;
    loop {
        stream.read_exact(&mut byte).unwrap();
        packet.push(byte[0]);
        length += usize::from(byte[0] & 127) << shift;
        if byte[0] & 128 == 0 {
            break;
        }
        shift += 7;
        assert!(shift <= 21);
    }
    let header = packet.len();
    packet.resize(header + length, 0);
    stream.read_exact(&mut packet[header..]).unwrap();
    packet
}

fn broker(
    test: impl FnOnce(&mut TcpStream) + Send + 'static,
) -> (MqttLoginData, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(read_packet(&mut stream)[0], 0x10);
        stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
        test(&mut stream);
    });
    (
        MqttLoginData {
            broker: "127.0.0.1".into(),
            port: port.to_string(),
            automatic_reconnect: false,
            ..Default::default()
        },
        thread,
    )
}

fn wait_for_disconnect(client: &ClientHandle) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match client
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            ClientEvent::Disconnected(reason) => return reason,
            ClientEvent::ControlOverflow => panic!("unexpected control overflow"),
            _ => {}
        }
    }
}

#[test]
fn oversized_header_disconnects_without_waiting_for_packet_body() {
    let (login, server) = broker(|stream| {
        // A 256 KiB body already exceeds the total packet limit. Send no body.
        stream.write_all(&[0x30, 0x80, 0x80, 0x10]).unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let runtime = Runtime::new().unwrap();
    let client = spawn_headless_client(&runtime, 1, login);
    let reason = wait_for_disconnect(&client);
    assert!(reason.contains("size limit"), "{reason}");
    server.join().unwrap();
}

#[test]
fn pending_qos2_payloads_exhaust_byte_budget_before_message_limit() {
    let (login, server) = broker(|stream| {
        for id in 1..100 {
            let packet = mqtt_ep::packet::v5_0::Publish::builder()
                .topic_name("budget")
                .unwrap()
                .qos(mqtt_ep::packet::Qos::ExactlyOnce)
                .packet_id(id)
                .payload(vec![0; MAX_PACKET_BYTES / 2])
                .build()
                .unwrap()
                .to_continuous_buffer();
            stream.write_all(&packet).unwrap();
            let mut byte = [0];
            let size = stream.read(&mut byte).unwrap();
            if size == 0 {
                return;
            }
            assert_eq!(byte[0], 0x50, "expected PUBREC");
            // PUBREC generated without properties/reason is four bytes total.
            let mut rest = [0; 3];
            stream.read_exact(&mut rest).unwrap();
            assert_eq!(rest[0], 2);
        }
        panic!("client failed to enforce its byte budget");
    });
    let runtime = Runtime::new().unwrap();
    let client = spawn_headless_client(&runtime, 2, login);
    assert!(wait_for_disconnect(&client).contains("QoS 2 byte or message budget"));
    server.join().unwrap();
}
