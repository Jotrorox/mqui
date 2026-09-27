use super::*;
use mqtt_ep::transport::{TransportError, TransportOps};
use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, IoSlice};
use std::pin::Pin;
use tokio::sync::{Notify, oneshot};

enum ConnectWrite {
    Success,
    Fail,
    Hold(oneshot::Receiver<()>),
}

// Drive the real endpoint and protocol task, with deterministic transport errors
// and paused time instead of TCP reset races or wall-clock acknowledgement waits.
struct TestTransport {
    incoming: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    buffered: VecDeque<u8>,
    outgoing: tokio_mpsc::UnboundedSender<Vec<u8>>,
    connect_write: ConnectWrite,
    closed: Arc<AtomicBool>,
}

impl TransportOps for TestTransport {
    fn send<'a>(
        &'a mut self,
        buffers: &'a [IoSlice<'a>],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            self.outgoing
                .send(
                    buffers
                        .iter()
                        .flat_map(|buffer| buffer.iter().copied())
                        .collect(),
                )
                .unwrap();
            match std::mem::replace(&mut self.connect_write, ConnectWrite::Success) {
                ConnectWrite::Success => Ok(()),
                ConnectWrite::Fail => Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected CONNECT write failure",
                )
                .into()),
                ConnectWrite::Hold(release) => {
                    release.await.unwrap();
                    Ok(())
                }
            }
        })
    }

    fn recv<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            if self.buffered.is_empty() {
                let Some(bytes) = self.incoming.recv().await else {
                    return Ok(0);
                };
                self.buffered.extend(bytes);
            }
            let count = buffer.len().min(self.buffered.len());
            for byte in &mut buffer[..count] {
                *byte = self.buffered.pop_front().unwrap();
            }
            Ok(count)
        })
    }

    fn shutdown<'a>(
        &'a mut self,
        _timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.closed.store(true, Ordering::Release);
        })
    }
}

struct Harness {
    client: ClientHandle,
    incoming: tokio_mpsc::UnboundedSender<Vec<u8>>,
    outgoing: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    closed: Arc<AtomicBool>,
    event_ready: Arc<Notify>,
}

impl Harness {
    fn new(runtime: &Runtime, connect_write: ConnectWrite) -> Self {
        let (incoming, incoming_rx) = tokio_mpsc::unbounded_channel();
        let (outgoing_tx, outgoing) = tokio_mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let transport = TestTransport {
            incoming: incoming_rx,
            buffered: VecDeque::new(),
            outgoing: outgoing_tx,
            connect_write,
            closed: closed.clone(),
        };
        let event_ready = Arc::new(Notify::new());
        let notify = event_ready.clone();
        let client = spawn_client_with_connector(
            runtime,
            1,
            MqttLoginData::default(),
            Some(Arc::new(move || notify.notify_one())),
            |_, _| async move {
                Ok((
                    Box::new(transport) as Box<dyn TransportOps + Send>,
                    "test".into(),
                ))
            },
        );
        Self {
            client,
            incoming,
            outgoing,
            closed,
            event_ready,
        }
    }

    async fn packet(&mut self, packet_type: u8) -> Vec<u8> {
        let packet = timeout(Duration::from_secs(1), self.outgoing.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet[0] >> 4, packet_type);
        packet
    }

    async fn connected(&mut self) {
        self.packet(1).await;
        self.incoming.send(vec![0x20, 3, 0, 0, 0]).unwrap();
        timeout(Duration::from_secs(1), async {
            loop {
                while let Ok(event) = self.client.try_recv() {
                    if matches!(event, ClientEvent::Connected) {
                        return;
                    }
                }
                self.event_ready.notified().await;
            }
        })
        .await
        .unwrap();
    }

    async fn finish(&mut self, limit: Duration) -> Vec<ClientEvent> {
        timeout(limit, &mut self.client.join_handle)
            .await
            .expect("protocol task did not exit")
            .unwrap();
        assert!(
            self.closed.load(Ordering::Acquire),
            "transport was not closed"
        );
        let mut events = Vec::new();
        while let Ok(event) = self.client.try_recv() {
            events.push(event);
        }
        assert!(matches!(
            self.client.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(self.client.try_send(ClientCommand::Disconnect).is_err());
        assert!(
            self.client
                .command_tx
                .budget
                .reserve(CLIENT_BUFFER_BYTES - CONTROL_BUFFER_BYTES)
                .is_some(),
            "protocol task retained command or message bytes"
        );
        events
    }
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

fn assert_disconnected(events: &[ClientEvent], expected: &str) {
    let reasons: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Disconnected(reason) => Some(reason.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "{events:?}");
    assert!(reasons[0].contains(expected), "{reasons:?}");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            ClientEvent::Published { .. }
                | ClientEvent::Subscribed { .. }
                | ClientEvent::Unsubscribed { .. }
        )),
        "unexpected acknowledgement: {events:?}"
    );
}

#[test]
fn connect_send_propagates_endpoint_error() {
    let runtime = runtime();
    runtime.block_on(async {
        let endpoint =
            mqtt_ep::endpoint::Endpoint::<mqtt_ep::role::Client>::new(mqtt_ep::Version::V5_0);
        let packet = mqtt_ep::packet::v5_0::Connect::builder()
            .client_id("test")
            .unwrap()
            .build()
            .unwrap();
        // Sending before attach returns the endpoint's inner error immediately.
        let result = timeout(
            MQTT_HANDSHAKE_TIMEOUT,
            checked_connect_send(endpoint.send(packet), &Mutex::new(None)),
        )
        .await
        .unwrap();
        assert!(result.is_err(), "endpoint error was ignored");
    });
}

#[test]
fn failed_connect_write_disconnects_immediately() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Fail);
        let events = test.finish(Duration::from_secs(1)).await;
        assert_disconnected(&events, "CONNECT send failed:");
        assert_disconnected(&events, "injected CONNECT write failure");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ClientEvent::Connected))
        );
    });
}

#[test]
fn connect_write_timeout_disconnects() {
    let runtime = runtime();
    runtime.block_on(async {
        let (release, held) = oneshot::channel();
        let mut test = Harness::new(&runtime, ConnectWrite::Hold(held));
        test.packet(1).await;
        tokio::time::advance(MQTT_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;
        // The endpoint serializes transport I/O and close requests. Let its
        // pending write return after the protocol deadline has expired.
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        let events = test.finish(Duration::from_secs(1)).await;
        assert_disconnected(&events, "CONNECT send timed out");
    });
}

#[test]
fn cancellation_during_connect_write_closes_transport() {
    let runtime = runtime();
    runtime.block_on(async {
        let (release, held) = oneshot::channel();
        let mut test = Harness::new(&runtime, ConnectWrite::Hold(held));
        test.packet(1).await;
        test.client.cancel();
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        let events = test.finish(Duration::from_secs(1)).await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ClientEvent::Connected))
        );
    });
}

#[test]
fn cancellation_while_waiting_for_connack_closes_transport() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.packet(1).await;
        test.client.cancel();
        let events = test.finish(Duration::from_secs(1)).await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ClientEvent::Connected))
        );
    });
}

#[test]
fn cancellation_with_pending_publish_releases_resources() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.connected().await;
        test.client.try_send(publish(1)).unwrap();
        test.packet(3).await;
        test.client.cancel();
        test.finish(Duration::from_secs(1)).await;
    });
}

#[test]
fn closed_command_channel_exits_and_closes_transport() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.connected().await;
        let (replacement, receiver) = tokio_mpsc::channel(1);
        drop(receiver);
        drop(std::mem::replace(
            &mut test.client.command_tx.sender,
            replacement,
        ));
        let events = test.finish(Duration::from_secs(1)).await;
        assert_disconnected(&events, "Command channel closed");
    });
}

#[test]
fn missing_connack_disconnects_at_handshake_deadline() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.packet(1).await;
        let start = tokio::time::Instant::now();
        let events = test
            .finish(MQTT_HANDSHAKE_TIMEOUT + Duration::from_secs(1))
            .await;
        assert!(start.elapsed() >= MQTT_HANDSHAKE_TIMEOUT);
        assert_disconnected(&events, "Waiting for CONNACK timed out");
    });
}

fn publish(qos: u8) -> ClientCommand {
    ClientCommand::Publish {
        topic: "test".into(),
        payload: vec![42; 64],
        qos,
        retain: false,
    }
}

#[test]
fn missing_command_acknowledgements_disconnect_and_release_resources() {
    let runtime = runtime();
    runtime.block_on(async {
        for (command, packet_type) in [
            (
                ClientCommand::Subscribe {
                    topic: "test".into(),
                    qos: 1,
                },
                8,
            ),
            (
                ClientCommand::Unsubscribe {
                    topic: "test".into(),
                },
                10,
            ),
            (publish(1), 3),
            (publish(2), 3),
        ] {
            let mut test = Harness::new(&runtime, ConnectWrite::Success);
            test.connected().await;
            test.client.try_send(command).unwrap();
            test.packet(packet_type).await;
            let start = tokio::time::Instant::now();
            let events = test.finish(ACK_TIMEOUT + Duration::from_secs(2)).await;
            assert!(start.elapsed() >= ACK_TIMEOUT);
            assert_disconnected(&events, "Acknowledgement timed out");
        }
    });
}

#[test]
fn missing_pubcomp_disconnects_and_releases_resources() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.connected().await;
        test.client.try_send(publish(2)).unwrap();
        let packet = test.packet(3).await;
        // One-byte Remaining Length, followed by the two-byte topic length and "test".
        let id = [packet[8], packet[9]];
        test.incoming.send(vec![0x50, 2, id[0], id[1]]).unwrap();
        assert_eq!(test.packet(6).await, vec![0x62, 2, id[0], id[1]]);
        let events = test.finish(ACK_TIMEOUT + Duration::from_secs(2)).await;
        assert_disconnected(&events, "Acknowledgement timed out");
    });
}

#[test]
fn missing_incoming_pubrel_disconnects_without_delivering_message() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.connected().await;
        let packet = mqtt_ep::packet::v5_0::Publish::builder()
            .topic_name("test")
            .unwrap()
            .qos(mqtt_ep::packet::Qos::ExactlyOnce)
            .packet_id(7)
            .payload(vec![42; 64])
            .build()
            .unwrap()
            .to_continuous_buffer();
        test.incoming.send(packet).unwrap();
        assert_eq!(test.packet(5).await, vec![0x50, 2, 0, 7]);
        let events = test.finish(ACK_TIMEOUT + Duration::from_secs(2)).await;
        assert_disconnected(&events, "Acknowledgement timed out");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ClientEvent::MessageReceived { .. }))
        );
    });
}

#[test]
fn saturated_control_queue_stops_protocol_and_reports_terminal_overflow() {
    let runtime = runtime();
    runtime.block_on(async {
        let mut test = Harness::new(&runtime, ConnectWrite::Success);
        test.connected().await;
        // Invalid commands produce control errors without creating MQTT traffic.
        for _ in 0..=EVENT_CAPACITY {
            if test.client.cancellation.is_cancelled() {
                break;
            }
            test.client
                .try_send(ClientCommand::Subscribe {
                    topic: "test".into(),
                    qos: 3,
                })
                .unwrap();
            tokio::task::yield_now().await;
        }
        let events = test.finish(Duration::from_secs(1)).await;
        assert!(test.client.cancellation.is_cancelled());
        assert_eq!(events.len(), EVENT_CAPACITY + 1);
        assert!(matches!(events.last(), Some(ClientEvent::ControlOverflow)));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ClientEvent::ControlOverflow))
                .count(),
            1
        );
    });
}
