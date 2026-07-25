use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mqui::{
    ClientCommand, ClientEvent, MqttLoginData, TlsVerificationMode, TransportKind,
    spawn_headless_client,
};
use tokio::runtime::Runtime;

const EVENT_TIMEOUT: Duration = Duration::from_secs(8);

fn login(port: u16, suffix: &str) -> MqttLoginData {
    MqttLoginData {
        broker: "localhost".into(),
        port: port.to_string(),
        client_id: format!("mqui-it-{suffix}"),
        automatic_reconnect: false,
        ..MqttLoginData::default()
    }
}

fn wait_for(
    client: &mqui::ClientHandle,
    description: &str,
    predicate: impl Fn(&ClientEvent) -> bool,
) -> ClientEvent {
    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    let mut observed = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for {description}; observed events: {observed:#?}"
        );
        match client.recv_timeout(remaining) {
            Ok(event) if predicate(&event) => return event,
            Ok(ClientEvent::Error(error)) => {
                panic!(
                    "client error while waiting for {description}: {error}; \
                     earlier events: {observed:#?}"
                )
            }
            Ok(event) => observed.push(event),
            Err(error) => {
                panic!("failed waiting for {description}: {error}; observed events: {observed:#?}")
            }
        }
    }
}

fn connect(runtime: &Runtime, key: u64, settings: MqttLoginData) -> mqui::ClientHandle {
    let client = spawn_headless_client(runtime, key, settings);
    wait_for(&client, "successful connection", |event| {
        matches!(event, ClientEvent::Connected)
    });
    client
}

fn send(client: &mqui::ClientHandle, command: ClientCommand) {
    client.try_send(command).expect("send client command");
}

fn exercise_round_trip(client: &mqui::ClientHandle, topic: &str, qos: u8) {
    send(
        client,
        ClientCommand::Subscribe {
            topic: topic.into(),
            qos,
        },
    );
    wait_for(
        client,
        "SUBACK",
        |event| matches!(event, ClientEvent::Subscribed { topic: actual, .. } if actual == topic),
    );

    let payload = format!("qos-{qos}").into_bytes();
    send(
        client,
        ClientCommand::Publish {
            topic: topic.into(),
            payload: payload.clone(),
            qos,
            retain: false,
        },
    );
    // Mosquitto may deliver the looped-back PUBLISH before or after its
    // acknowledgement, so retain both observations instead of assuming order.
    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    let mut published = false;
    let mut received = false;
    let mut observed = Vec::new();
    while !published || !received {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for QoS {qos} round trip on {topic}; observed: {observed:#?}"
        );
        let event = client
            .recv_timeout(remaining)
            .unwrap_or_else(|error| panic!("QoS {qos} round trip failed on {topic}: {error}"));
        match &event {
            ClientEvent::Published { topic: actual, .. } if actual == topic => published = true,
            ClientEvent::MessageReceived {
                topic: actual,
                qos: actual_qos,
                payload: actual_payload,
                ..
            } if actual == topic && *actual_qos == qos && actual_payload == &payload => {
                received = true;
            }
            ClientEvent::Error(error) => {
                panic!("client error during QoS {qos} round trip on {topic}: {error}")
            }
            _ => observed.push(event),
        }
    }
}

#[test]
fn mosquitto_protocol_and_transport_suite() {
    if std::env::var_os("MQUI_INTEGRATION_TESTS").is_none() {
        eprintln!(
            "skipping Mosquitto suite; set MQUI_INTEGRATION_TESTS=1 after starting test-broker"
        );
        return;
    }

    let runtime = Runtime::new().expect("create Tokio runtime");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let root = format!("mqui/integration/{nonce}");

    let tcp = connect(&runtime, 1, login(18883, &format!("{nonce}-tcp")));
    for qos in 0..=2 {
        exercise_round_trip(&tcp, &format!("{root}/qos/{qos}"), qos);
    }

    let unsubscribe_topic = format!("{root}/unsubscribe");
    send(
        &tcp,
        ClientCommand::Subscribe {
            topic: unsubscribe_topic.clone(),
            qos: 1,
        },
    );
    wait_for(
        &tcp,
        "subscription before unsubscribe",
        |event| matches!(event, ClientEvent::Subscribed { topic, .. } if topic == &unsubscribe_topic),
    );
    send(
        &tcp,
        ClientCommand::Unsubscribe {
            topic: unsubscribe_topic.clone(),
        },
    );
    wait_for(
        &tcp,
        "UNSUBACK",
        |event| matches!(event, ClientEvent::Unsubscribed { topic, .. } if topic == &unsubscribe_topic),
    );
    send(
        &tcp,
        ClientCommand::Publish {
            topic: unsubscribe_topic.clone(),
            payload: b"must-not-arrive".to_vec(),
            qos: 1,
            retain: false,
        },
    );
    wait_for(
        &tcp,
        "publish after unsubscribe",
        |event| matches!(event, ClientEvent::Published { topic, .. } if topic == &unsubscribe_topic),
    );
    let quiet_deadline = std::time::Instant::now() + Duration::from_millis(500);
    while let Ok(event) =
        tcp.recv_timeout(quiet_deadline.saturating_duration_since(std::time::Instant::now()))
    {
        assert!(
            !matches!(
                event,
                ClientEvent::MessageReceived { ref topic, .. } if topic == &unsubscribe_topic
            ),
            "received a message after unsubscribe: {event:?}"
        );
        if std::time::Instant::now() >= quiet_deadline {
            break;
        }
    }

    let retained_topic = format!("{root}/retained");
    send(
        &tcp,
        ClientCommand::Publish {
            topic: retained_topic.clone(),
            payload: b"retained-value".to_vec(),
            qos: 1,
            retain: true,
        },
    );
    wait_for(
        &tcp,
        "retained publish acknowledgement",
        |event| matches!(event, ClientEvent::Published { topic, .. } if topic == &retained_topic),
    );
    let retained_subscriber = connect(&runtime, 2, login(18883, &format!("{nonce}-retained")));
    send(
        &retained_subscriber,
        ClientCommand::Subscribe {
            topic: retained_topic.clone(),
            qos: 1,
        },
    );
    wait_for(&retained_subscriber, "retained message", |event| {
        matches!(
            event,
            ClientEvent::MessageReceived { topic, retain: true, payload, .. }
                if topic == &retained_topic && payload == b"retained-value"
        )
    });

    let mut authenticated = login(18884, &format!("{nonce}-auth-ok"));
    authenticated.username = "mqui-test".into();
    authenticated.password = "correct-password".into();
    let auth_client = connect(&runtime, 3, authenticated);

    let mut rejected = login(18884, &format!("{nonce}-auth-bad"));
    rejected.username = "mqui-test".into();
    rejected.password = "wrong-password".into();
    let rejected_client = spawn_headless_client(&runtime, 4, rejected);
    wait_for(&rejected_client, "authentication rejection", |event| {
        matches!(
            event,
            ClientEvent::Disconnected(reason)
                if reason.contains("username or password") || reason.contains("authorization")
        )
    });

    let mut websocket_login = login(19001, &format!("{nonce}-ws"));
    websocket_login.transport = TransportKind::Ws;
    websocket_login.ws_path = "/mqtt".into();
    let websocket = connect(&runtime, 5, websocket_login);
    exercise_round_trip(&websocket, &format!("{root}/websocket"), 1);

    let ca_path = std::env::var_os("MQUI_TEST_CA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("test-broker/generated/ca.crt"));
    assert!(
        ca_path.is_file(),
        "test CA does not exist at {}; did the broker finish starting?",
        ca_path.display()
    );
    let mut tls_login = login(18885, &format!("{nonce}-tls"));
    tls_login.transport = TransportKind::Tls;
    tls_login.tls_verification = TlsVerificationMode::CustomCa;
    tls_login.tls_ca_cert_path = ca_path.display().to_string();
    let tls = connect(&runtime, 6, tls_login);
    exercise_round_trip(&tls, &format!("{root}/tls"), 1);

    let duplicate_id = format!("mqui-it-{nonce}-duplicate");
    let original = connect(
        &runtime,
        7,
        MqttLoginData {
            client_id: duplicate_id.clone(),
            ..login(18883, "original")
        },
    );
    let replacement = connect(
        &runtime,
        8,
        MqttLoginData {
            client_id: duplicate_id,
            ..login(18883, "replacement")
        },
    );
    wait_for(
        &original,
        "broker-initiated duplicate-client disconnect",
        |event| matches!(event, ClientEvent::Disconnected(_)),
    );

    for client in [
        &tcp,
        &retained_subscriber,
        &auth_client,
        &rejected_client,
        &websocket,
        &tls,
        &original,
        &replacement,
    ] {
        client.cancel();
    }
}
