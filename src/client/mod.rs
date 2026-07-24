use mqtt_endpoint_tokio::mqtt_ep;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, Once};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::Request;

use crate::models::client::ClientHandle;
use crate::models::ipc::{ClientCommand, ClientEvent};
use crate::models::mqtt::{MqttLoginData, TlsVerificationMode, TransportKind};
use crate::utils::qos::qos_to_u8;

static RUSTLS_PROVIDER_INIT: Once = Once::new();

#[derive(Debug, PartialEq, Eq)]
enum OutgoingPublishState {
    Puback { topic: String },
    Pubrec { topic: String },
    Pubcomp { topic: String },
}

impl OutgoingPublishState {
    fn after_successful_pubrec(self) -> Result<Self, Self> {
        match self {
            Self::Pubrec { topic } => Ok(Self::Pubcomp { topic }),
            other => Err(other),
        }
    }

    fn complete_with_puback(self) -> Result<String, Self> {
        match self {
            Self::Puback { topic } => Ok(topic),
            other => Err(other),
        }
    }

    fn complete_with_pubcomp(self) -> Result<String, Self> {
        match self {
            Self::Pubcomp { topic } => Ok(topic),
            other => Err(other),
        }
    }
}

#[derive(Debug)]
struct IncomingQos2Message {
    topic: String,
    qos: u8,
    retain: bool,
    payload: Vec<u8>,
}

fn connack_error(code: mqtt_ep::result_code::ConnectReasonCode) -> Option<String> {
    use mqtt_ep::result_code::ConnectReasonCode;

    match code {
        ConnectReasonCode::Success => None,
        ConnectReasonCode::BadUserNameOrPassword => {
            Some("broker rejected the username or password".to_string())
        }
        ConnectReasonCode::NotAuthorized => {
            Some("broker denied authorization for this connection".to_string())
        }
        other => Some(format!("broker rejected the connection: {other}")),
    }
}

fn suback_result(codes: &[mqtt_ep::result_code::SubackReasonCode]) -> Result<(u8, String), String> {
    use mqtt_ep::result_code::SubackReasonCode;

    let [code] = codes else {
        return Err(format!(
            "expected exactly one reason code, received {}",
            codes.len()
        ));
    };
    let qos = match code {
        SubackReasonCode::GrantedQos0 => 0,
        SubackReasonCode::GrantedQos1 => 1,
        SubackReasonCode::GrantedQos2 => 2,
        other => return Err(format!("{other}")),
    };
    Ok((qos, code.to_string()))
}

fn unsuback_result(codes: &[mqtt_ep::result_code::UnsubackReasonCode]) -> Result<String, String> {
    let [code] = codes else {
        return Err(format!(
            "expected exactly one reason code, received {}",
            codes.len()
        ));
    };
    if code.is_success() {
        Ok(code.to_string())
    } else {
        Err(code.to_string())
    }
}

fn optional_reason_is_success<T>(code: Option<T>, is_success: impl FnOnce(&T) -> bool) -> bool {
    code.as_ref().is_none_or(is_success)
}

#[derive(Debug)]
struct InsecureServerCertVerifier;

impl ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

fn ensure_rustls_crypto_provider() {
    RUSTLS_PROVIDER_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn build_tls_config(
    login: &MqttLoginData,
    domain: &str,
) -> Result<Option<Arc<ClientConfig>>, String> {
    if domain.trim().is_empty() {
        return Err("TLS transport requires a non-empty server name".to_string());
    }

    ensure_rustls_crypto_provider();

    match login.tls_verification {
        TlsVerificationMode::SystemRoots => Ok(None),
        TlsVerificationMode::CustomCa => {
            let path = login.tls_ca_cert_path.trim();
            if path.is_empty() {
                return Err("Custom CA verification requires a CA PEM file path".to_string());
            }

            let mut root_store = RootCertStore::empty();
            let cert_result = rustls_native_certs::load_native_certs();
            for cert in cert_result.certs {
                let _ = root_store.add(cert);
            }

            let certs = CertificateDer::pem_file_iter(path)
                .map_err(|err| format!("Failed to open CA PEM file '{path}': {err}"))?;
            let mut found_cert = false;
            for cert in certs {
                let cert =
                    cert.map_err(|err| format!("Failed to read CA PEM file '{path}': {err}"))?;
                root_store.add(cert).map_err(|err| {
                    format!("Failed to add certificate from CA PEM file '{path}': {err}")
                })?;
                found_cert = true;
            }

            if !found_cert {
                return Err(format!(
                    "CA PEM file '{path}' did not contain any certificates"
                ));
            }

            Ok(Some(Arc::new(
                ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth(),
            )))
        }
        TlsVerificationMode::InsecureSkipVerify => Ok(Some(Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(InsecureServerCertVerifier))
                .with_no_client_auth(),
        ))),
    }
}

fn build_websocket_request(addr: &str, path: &str) -> Result<Request<()>, String> {
    let url = format!("ws://{addr}{path}");
    Request::builder()
        .uri(&url)
        .header("Host", addr)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Protocol", "mqtt")
        .body(())
        .map_err(|err| format!("Failed to build WebSocket request: {err}"))
}

async fn connect_transport(
    login: &MqttLoginData,
) -> Result<(Box<dyn mqtt_ep::transport::TransportOps + Send>, String), String> {
    let resolved = login.resolve_connection()?;
    let transport: Box<dyn mqtt_ep::transport::TransportOps + Send> = match resolved.transport {
        TransportKind::Tcp => {
            let stream = mqtt_ep::transport::connect_helper::connect_tcp(&resolved.addr, None)
                .await
                .map_err(|err| format!("TCP connect failed: {err}"))?;
            Box::new(mqtt_ep::transport::TcpTransport::from_stream(stream))
        }
        TransportKind::Tls => {
            let domain = resolved
                .tls_domain
                .as_deref()
                .ok_or_else(|| "TLS transport requires a server name".to_string())?;
            let tls_config = build_tls_config(login, domain)?;
            let stream = mqtt_ep::transport::connect_helper::connect_tcp_tls(
                &resolved.addr,
                domain,
                tls_config,
                None,
            )
            .await
            .map_err(|err| format!("TLS connect failed: {err}"))?;
            Box::new(mqtt_ep::transport::TlsTransport::from_stream(stream))
        }
        TransportKind::Ws => {
            let path = resolved
                .ws_path
                .as_deref()
                .ok_or_else(|| "WebSocket transport requires a path".to_string())?;
            let tcp_stream = mqtt_ep::transport::connect_helper::connect_tcp(&resolved.addr, None)
                .await
                .map_err(|err| format!("WebSocket TCP connect failed: {err}"))?;
            let request = build_websocket_request(&resolved.addr, path)?;
            let (stream, _response) = client_async(request, tcp_stream)
                .await
                .map_err(|err| format!("WebSocket connect failed: {err}"))?;
            Box::new(mqtt_ep::transport::WebSocketTransport::from_tcp_client_stream(stream))
        }
        TransportKind::Wss => {
            let domain = resolved
                .tls_domain
                .as_deref()
                .ok_or_else(|| "Secure WebSocket transport requires a server name".to_string())?;
            let path = resolved
                .ws_path
                .as_deref()
                .ok_or_else(|| "Secure WebSocket transport requires a path".to_string())?;
            let tls_config = build_tls_config(login, domain)?;
            let stream = mqtt_ep::transport::connect_helper::connect_tcp_tls_ws(
                &resolved.addr,
                domain,
                path,
                tls_config,
                None,
                None,
            )
            .await
            .map_err(|err| format!("Secure WebSocket connect failed: {err}"))?;
            Box::new(mqtt_ep::transport::WebSocketTransport::from_tls_client_stream(stream))
        }
    };

    Ok((transport, resolved.display_label))
}

pub(crate) fn spawn_client(runtime: &Runtime, tab_id: u64, login: MqttLoginData) -> ClientHandle {
    let (event_tx, event_rx) = mpsc::channel();
    let (command_tx, mut command_rx) = tokio_mpsc::unbounded_channel::<ClientCommand>();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let client_id = login.effective_client_id(tab_id);
    let keep_alive_secs = login.effective_keep_alive_secs();

    let join_handle = runtime.spawn(async move {
        let resolved = match login.resolve_connection() {
            Ok(resolved) => resolved,
            Err(err) => {
                let _ = event_tx.send(ClientEvent::Disconnected(format!(
                    "Invalid connection settings: {err}"
                )));
                return;
            }
        };
        let _ = event_tx.send(ClientEvent::Status(format!(
            "Connecting via {} to {}",
            resolved.transport.label(),
            resolved.display_label
        )));

        let endpoint = mqtt_ep::endpoint::Endpoint::<mqtt_ep::role::Client>::new(mqtt_ep::Version::V5_0);
        let (transport, display_label) = match connect_transport(&login).await {
            Ok(transport) => transport,
            Err(err) => {
                let _ = event_tx.send(ClientEvent::Disconnected(err));
                return;
            }
        };
        if let Err(err) = endpoint
            .attach(transport, mqtt_ep::endpoint::Mode::Client)
            .await
        {
            let _ = event_tx.send(ClientEvent::Disconnected(format!("Attach failed: {err}")));
            return;
        }

        let mut connect_builder = match mqtt_ep::packet::v5_0::Connect::builder().client_id(&client_id) {
            Ok(builder) => builder.keep_alive(keep_alive_secs).clean_start(true),
            Err(err) => {
                let _ = event_tx.send(ClientEvent::Disconnected(format!("Client ID setup failed: {err}")));
                let _ = endpoint.close().await;
                return;
            }
        };

        if let Some(username) = login.username_opt() {
            connect_builder = match connect_builder.user_name(username) {
                Ok(builder) => builder,
                Err(err) => {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!("Username setup failed: {err}")));
                    let _ = endpoint.close().await;
                    return;
                }
            };

            if let Some(password) = login.password_opt() {
                connect_builder = match connect_builder.password(password.as_bytes().to_vec()) {
                    Ok(builder) => builder,
                    Err(err) => {
                        let _ = event_tx.send(ClientEvent::Disconnected(format!("Password setup failed: {err}")));
                        let _ = endpoint.close().await;
                        return;
                    }
                };
            }
        }

        if let Some(testament) = login.testament_and_last_will_opt() {
            let will_topic = login
                .testament_topic_opt()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("mqui/{client_id}/last-will"));
            let will_qos = match mqtt_ep::packet::Qos::try_from(login.testament_qos) {
                Ok(qos) => qos,
                Err(_) => mqtt_ep::packet::Qos::AtMostOnce,
            };
            connect_builder = match connect_builder.will_message(
                &will_topic,
                testament.as_bytes().to_vec(),
                will_qos,
                login.testament_retain,
            ) {
                Ok(builder) => builder,
                Err(err) => {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!(
                        "Last Will setup failed: {err}"
                    )));
                    let _ = endpoint.close().await;
                    return;
                }
            };
        }

        let connect_packet = match connect_builder.build() {
            Ok(packet) => packet,
            Err(err) => {
                let _ = event_tx.send(ClientEvent::Disconnected(format!("CONNECT build failed: {err}")));
                let _ = endpoint.close().await;
                return;
            }
        };

        if let Err(err) = endpoint.send(connect_packet).await {
            let _ = event_tx.send(ClientEvent::Disconnected(format!("CONNECT send failed: {err}")));
            let _ = endpoint.close().await;
            return;
        }

        let connack = match endpoint.recv().await {
            Ok(packet) => packet,
            Err(err) => {
                let _ = event_tx.send(ClientEvent::Disconnected(format!("CONNACK recv failed: {err}")));
                let _ = endpoint.close().await;
                return;
            }
        };

        match connack {
            mqtt_ep::packet::Packet::V5_0Connack(connack) => {
                if let Some(err) = connack_error(connack.reason_code()) {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!(
                        "CONNACK rejected: {err}"
                    )));
                    let _ = endpoint.close().await;
                    return;
                }
                let _ = event_tx.send(ClientEvent::Connected);
                let _ =
                    event_tx.send(ClientEvent::Status(format!("Connected to {display_label}")));
            }
            other => {
                let _ = event_tx.send(ClientEvent::Disconnected(format!(
                    "Expected CONNACK, got {:?}",
                    other.packet_type()
                )));
                let _ = endpoint.close().await;
                return;
            }
        }

        let mut pending_subscribe: HashMap<u16, (String, u8)> = HashMap::new();
        let mut pending_unsubscribe: HashMap<u16, String> = HashMap::new();
        let mut pending_publish: HashMap<u16, OutgoingPublishState> = HashMap::new();
        let mut incoming_qos2: HashMap<u16, IncomingQos2Message> = HashMap::new();
        let mut completed_incoming_qos2: HashSet<u16> = HashSet::new();

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    let _ = endpoint.close().await;
                    let _ = event_tx.send(ClientEvent::Status("Closed".to_string()));
                    break;
                }
                maybe_command = command_rx.recv() => {
                    let Some(command) = maybe_command else {
                        continue;
                    };

                    match command {
                        ClientCommand::Disconnect => {
                            let disconnect_packet = mqtt_ep::packet::v5_0::Disconnect::builder()
                                .build();

                            if let Ok(packet) = disconnect_packet {
                                let _ = endpoint.send(packet).await;
                            }

                            let _ = endpoint.close().await;
                            let _ = event_tx.send(ClientEvent::Disconnected(
                                "Disconnected by user".to_string(),
                            ));
                            break;
                        }
                        ClientCommand::ForceDisconnect => {
                            let _ = endpoint.close().await;
                            let _ = event_tx.send(ClientEvent::Disconnected(
                                "Force disconnected by user".to_string(),
                            ));
                            break;
                        }
                        ClientCommand::Subscribe { topic, qos } => {
                            let qos_level = match mqtt_ep::packet::Qos::try_from(qos) {
                                Ok(level) => level,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Invalid subscribe QoS {qos}: {err}")));
                                    continue;
                                }
                            };

                            let packet_id = match endpoint.acquire_packet_id().await {
                                Ok(id) => id,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to acquire packet id: {err}")));
                                    continue;
                                }
                            };

                            let sub_opts = mqtt_ep::packet::SubOpts::new().set_qos(qos_level);
                            let sub_entry = match mqtt_ep::packet::SubEntry::new(&topic, sub_opts) {
                                Ok(entry) => entry,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Invalid subscription topic '{topic}': {err}")));
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                    continue;
                                }
                            };

                            let subscribe_packet = match mqtt_ep::packet::v5_0::Subscribe::builder()
                                .packet_id(packet_id)
                                .entries(vec![sub_entry])
                                .build()
                            {
                                Ok(packet) => packet,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to build SUBSCRIBE: {err}")));
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                    continue;
                                }
                            };

                            if let Err(err) = endpoint.send(subscribe_packet).await {
                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to send SUBSCRIBE: {err}")));
                                let _ = endpoint.release_packet_id(packet_id).await;
                                continue;
                            }

                            pending_subscribe.insert(packet_id, (topic, qos));
                        }
                        ClientCommand::Unsubscribe { topic } => {
                            let packet_id = match endpoint.acquire_packet_id().await {
                                Ok(id) => id,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to acquire packet id: {err}")));
                                    continue;
                                }
                            };

                            let unsubscribe_packet = match mqtt_ep::packet::v5_0::Unsubscribe::builder()
                                .packet_id(packet_id)
                                .entries(vec![topic.as_str()])
                                .and_then(|builder| builder.build())
                            {
                                Ok(packet) => packet,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to build UNSUBSCRIBE: {err}")));
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                    continue;
                                }
                            };

                            if let Err(err) = endpoint.send(unsubscribe_packet).await {
                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to send UNSUBSCRIBE: {err}")));
                                let _ = endpoint.release_packet_id(packet_id).await;
                                continue;
                            }

                            pending_unsubscribe.insert(packet_id, topic);
                        }
                        ClientCommand::Publish {
                            topic,
                            payload,
                            qos,
                            retain,
                        } => {
                            let qos_level = match mqtt_ep::packet::Qos::try_from(qos) {
                                Ok(level) => level,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Invalid publish QoS {qos}: {err}")));
                                    continue;
                                }
                            };

                            let mut builder = match mqtt_ep::packet::v5_0::Publish::builder()
                                .topic_name(&topic)
                            {
                                Ok(builder) => builder,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Invalid publish topic '{topic}': {err}")));
                                    continue;
                                }
                            }
                            .qos(qos_level)
                            .retain(retain)
                            .payload(payload);

                            let mut packet_id = None;
                            if qos_level != mqtt_ep::packet::Qos::AtMostOnce {
                                let id = match endpoint.acquire_packet_id().await {
                                    Ok(id) => id,
                                    Err(err) => {
                                        let _ = event_tx.send(ClientEvent::Error(format!("Failed to acquire packet id: {err}")));
                                        continue;
                                    }
                                };
                                builder = builder.packet_id(id);
                                packet_id = Some(id);
                            }

                            let publish_packet = match builder.build() {
                                Ok(packet) => packet,
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to build PUBLISH: {err}")));
                                    if let Some(id) = packet_id {
                                        let _ = endpoint.release_packet_id(id).await;
                                    }
                                    continue;
                                }
                            };

                            if let Err(err) = endpoint.send(publish_packet).await {
                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to send PUBLISH: {err}")));
                                if let Some(id) = packet_id {
                                    let _ = endpoint.release_packet_id(id).await;
                                }
                                continue;
                            }

                            if let Some(id) = packet_id {
                                let state = if qos_level == mqtt_ep::packet::Qos::ExactlyOnce {
                                    OutgoingPublishState::Pubrec {
                                        topic: topic.clone(),
                                    }
                                } else {
                                    OutgoingPublishState::Puback {
                                        topic: topic.clone(),
                                    }
                                };
                                pending_publish.insert(id, state);
                            } else {
                                let _ = event_tx.send(ClientEvent::Published { topic, packet_id: None });
                            }
                        }
                    }
                }
                recv_result = endpoint.recv() => {
                    let packet = match recv_result {
                        Ok(packet) => packet,
                        Err(err) => {
                            let _ = event_tx.send(ClientEvent::Disconnected(format!("Receive loop failed: {err}")));
                            let _ = endpoint.close().await;
                            break;
                        }
                    };

                    match packet {
                        mqtt_ep::packet::Packet::V5_0Publish(publish) => {
                            let payload = publish.payload().as_slice().to_vec();
                            let topic = publish.topic_name().to_string();
                            let qos_level = publish.qos();
                            let retain = publish.retain();
                            let duplicate = publish.dup();

                            match qos_level {
                                mqtt_ep::packet::Qos::AtMostOnce => {
                                    let _ = event_tx.send(ClientEvent::MessageReceived {
                                        topic,
                                        qos: qos_to_u8(qos_level),
                                        retain,
                                        payload,
                                    });
                                }
                                mqtt_ep::packet::Qos::AtLeastOnce => {
                                    if let Some(packet_id) = publish.packet_id() {
                                        let _ = event_tx.send(ClientEvent::MessageReceived {
                                            topic,
                                            qos: qos_to_u8(qos_level),
                                            retain,
                                            payload,
                                        });
                                        let puback = match mqtt_ep::packet::v5_0::Puback::builder()
                                            .packet_id(packet_id)
                                            .build()
                                        {
                                            Ok(packet) => packet,
                                            Err(err) => {
                                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to build PUBACK: {err}")));
                                                continue;
                                            }
                                        };

                                        if let Err(err) = endpoint.send(puback).await {
                                            let _ = event_tx.send(ClientEvent::Error(format!("Failed to send PUBACK: {err}")));
                                        }
                                    } else {
                                        let _ = event_tx.send(ClientEvent::Error(
                                            "Received QoS 1 PUBLISH without a packet id".to_string(),
                                        ));
                                    }
                                }
                                mqtt_ep::packet::Qos::ExactlyOnce => {
                                    if let Some(packet_id) = publish.packet_id() {
                                        if !duplicate {
                                            completed_incoming_qos2.remove(&packet_id);
                                        }
                                        if !completed_incoming_qos2.contains(&packet_id) {
                                            incoming_qos2.entry(packet_id).or_insert(
                                                IncomingQos2Message {
                                                    topic,
                                                    qos: qos_to_u8(qos_level),
                                                    retain,
                                                    payload,
                                                },
                                            );
                                        }
                                        let pubrec = match mqtt_ep::packet::v5_0::Pubrec::builder()
                                            .packet_id(packet_id)
                                            .build()
                                        {
                                            Ok(packet) => packet,
                                            Err(err) => {
                                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to build PUBREC: {err}")));
                                                continue;
                                            }
                                        };

                                        if let Err(err) = endpoint.send(pubrec).await {
                                            let _ = event_tx.send(ClientEvent::Error(format!("Failed to send PUBREC: {err}")));
                                        }
                                    } else {
                                        let _ = event_tx.send(ClientEvent::Error(
                                            "Received QoS 2 PUBLISH without a packet id".to_string(),
                                        ));
                                    }
                                }
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Suback(suback) => {
                            let packet_id = suback.packet_id();
                            if let Some((topic, qos)) = pending_subscribe.remove(&packet_id) {
                                let codes = suback.reason_codes();
                                match suback_result(&codes) {
                                    Ok((granted_qos, details)) => {
                                        let _ = event_tx.send(ClientEvent::Subscribed {
                                            topic,
                                            qos: granted_qos,
                                            details,
                                        });
                                    }
                                    Err(reason) => {
                                        let _ = event_tx.send(ClientEvent::Error(format!(
                                            "SUBACK rejected subscription to '{topic}' (packet id {packet_id}, requested QoS {qos}): {reason}"
                                        )));
                                    }
                                }
                                let _ = endpoint.release_packet_id(packet_id).await;
                            } else {
                                let _ = event_tx.send(ClientEvent::Error(format!(
                                    "Unexpected SUBACK for unknown packet id {packet_id}"
                                )));
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Unsuback(unsuback) => {
                            let packet_id = unsuback.packet_id();
                            if let Some(topic) = pending_unsubscribe.remove(&packet_id) {
                                let codes = unsuback.reason_codes();
                                match unsuback_result(&codes) {
                                    Ok(details) => {
                                        let _ = event_tx.send(ClientEvent::Unsubscribed {
                                            topic,
                                            details,
                                        });
                                    }
                                    Err(reason) => {
                                        let _ = event_tx.send(ClientEvent::Error(format!(
                                            "UNSUBACK rejected unsubscribe from '{topic}' (packet id {packet_id}): {reason}"
                                        )));
                                    }
                                }
                                let _ = endpoint.release_packet_id(packet_id).await;
                            } else {
                                let _ = event_tx.send(ClientEvent::Error(format!(
                                    "Unexpected UNSUBACK for unknown packet id {packet_id}"
                                )));
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Puback(puback) => {
                            let packet_id = puback.packet_id();
                            match pending_publish
                                .remove(&packet_id)
                                .map(OutgoingPublishState::complete_with_puback)
                            {
                                Some(Ok(topic)) => {
                                    let reason = puback.reason_code();
                                    if optional_reason_is_success(reason, |code| code.is_success()) {
                                        let _ = event_tx.send(ClientEvent::Published {
                                            topic,
                                            packet_id: Some(packet_id),
                                        });
                                    } else {
                                        let _ = event_tx.send(ClientEvent::Error(format!(
                                            "PUBACK rejected publish to '{topic}' (packet id {packet_id}): {}",
                                            reason.expect("failure reason exists")
                                        )));
                                    }
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                }
                                Some(Err(state)) => {
                                    pending_publish.insert(packet_id, state);
                                    let _ = event_tx.send(ClientEvent::Error(format!(
                                        "Unexpected PUBACK for packet id {packet_id}: publish is not awaiting PUBACK"
                                    )));
                                }
                                None => {
                                    let _ = event_tx.send(ClientEvent::Error(format!(
                                        "Unexpected PUBACK for unknown packet id {packet_id}"
                                    )));
                                }
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Pubrec(pubrec) => {
                            let packet_id = pubrec.packet_id();
                            let reason = pubrec.reason_code();
                            let transition = pending_publish
                                .remove(&packet_id)
                                .map(OutgoingPublishState::after_successful_pubrec);
                            if let Some(Ok(OutgoingPublishState::Pubcomp { topic })) = transition
                            {
                                if !optional_reason_is_success(reason, |code| code.is_success()) {
                                    let _ = event_tx.send(ClientEvent::Error(format!(
                                        "PUBREC rejected publish to '{topic}' (packet id {packet_id}): {}",
                                        reason.expect("failure reason exists")
                                    )));
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                    continue;
                                }
                                let pubrel = match mqtt_ep::packet::v5_0::Pubrel::builder()
                                    .packet_id(packet_id)
                                    .build()
                                {
                                    Ok(packet) => packet,
                                    Err(err) => {
                                        let _ = event_tx.send(ClientEvent::Error(format!("Failed to build PUBREL: {err}")));
                                        let _ = endpoint.release_packet_id(packet_id).await;
                                        continue;
                                    }
                                };

                                if let Err(err) = endpoint.send(pubrel).await {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to send PUBREL: {err}")));
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                } else {
                                    pending_publish.insert(
                                        packet_id,
                                        OutgoingPublishState::Pubcomp { topic },
                                    );
                                }
                            } else if let Some(Err(state @ OutgoingPublishState::Pubcomp { .. })) =
                                transition
                            {
                                pending_publish.insert(packet_id, state);
                                let pubrel = mqtt_ep::packet::v5_0::Pubrel::builder()
                                    .packet_id(packet_id)
                                    .build();
                                match pubrel {
                                    Ok(packet) => {
                                        if let Err(err) = endpoint.send(packet).await {
                                            let _ = event_tx.send(ClientEvent::Error(format!(
                                                "Failed to resend PUBREL for duplicate PUBREC (packet id {packet_id}): {err}"
                                            )));
                                        }
                                    }
                                    Err(err) => {
                                        let _ = event_tx.send(ClientEvent::Error(format!(
                                            "Failed to rebuild PUBREL for duplicate PUBREC (packet id {packet_id}): {err}"
                                        )));
                                    }
                                }
                            } else {
                                let context = match transition {
                                    Some(Err(state)) => {
                                        pending_publish.insert(packet_id, state);
                                        "publish is not awaiting PUBREC"
                                    }
                                    _ => "unknown packet id",
                                };
                                let _ = event_tx.send(ClientEvent::Error(format!(
                                    "Unexpected PUBREC for packet id {packet_id}: {context}"
                                )));
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Pubcomp(pubcomp) => {
                            let packet_id = pubcomp.packet_id();
                            match pending_publish
                                .remove(&packet_id)
                                .map(OutgoingPublishState::complete_with_pubcomp)
                            {
                                Some(Ok(topic)) => {
                                    let reason = pubcomp.reason_code();
                                    if optional_reason_is_success(reason, |code| code.is_success()) {
                                        let _ = event_tx.send(ClientEvent::Published {
                                            topic,
                                            packet_id: Some(packet_id),
                                        });
                                    } else {
                                        let _ = event_tx.send(ClientEvent::Error(format!(
                                            "PUBCOMP rejected publish to '{topic}' (packet id {packet_id}): {}",
                                            reason.expect("failure reason exists")
                                        )));
                                    }
                                    let _ = endpoint.release_packet_id(packet_id).await;
                                }
                                Some(Err(state)) => {
                                    pending_publish.insert(packet_id, state);
                                    let _ = event_tx.send(ClientEvent::Error(format!(
                                        "Unexpected PUBCOMP for packet id {packet_id}: publish is not awaiting PUBCOMP"
                                    )));
                                }
                                None => {
                                    let _ = event_tx.send(ClientEvent::Error(format!(
                                        "Unexpected PUBCOMP for unknown packet id {packet_id}"
                                    )));
                                }
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Pubrel(pubrel) => {
                            let packet_id = pubrel.packet_id();
                            let reason = pubrel.reason_code();
                            if !optional_reason_is_success(reason, |code| code.is_success()) {
                                let _ = event_tx.send(ClientEvent::Error(format!(
                                    "Broker sent failed PUBREL for packet id {packet_id}: {}",
                                    reason.expect("failure reason exists")
                                )));
                            }

                            let message = incoming_qos2.remove(&packet_id);
                            let already_completed = completed_incoming_qos2.contains(&packet_id);
                            let mut builder = mqtt_ep::packet::v5_0::Pubcomp::builder()
                                .packet_id(packet_id);
                            if message.is_none() && !already_completed {
                                builder = builder.reason_code(
                                    mqtt_ep::result_code::PubcompReasonCode::PacketIdentifierNotFound,
                                );
                            }
                            match builder.build() {
                                Ok(pubcomp) => {
                                    if let Err(err) = endpoint.send(pubcomp).await {
                                        let _ = event_tx.send(ClientEvent::Error(format!(
                                            "Failed to send PUBCOMP for packet id {packet_id}: {err}"
                                        )));
                                        continue;
                                    }
                                }
                                Err(err) => {
                                    let _ = event_tx.send(ClientEvent::Error(format!(
                                        "Failed to build PUBCOMP for packet id {packet_id}: {err}"
                                    )));
                                    continue;
                                }
                            }

                            if let Some(message) = message {
                                completed_incoming_qos2.insert(packet_id);
                                let _ = event_tx.send(ClientEvent::MessageReceived {
                                    topic: message.topic,
                                    qos: message.qos,
                                    retain: message.retain,
                                    payload: message.payload,
                                });
                            } else if !already_completed {
                                let _ = event_tx.send(ClientEvent::Error(format!(
                                    "Unexpected PUBREL for unknown incoming QoS 2 packet id {packet_id}"
                                )));
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Disconnect(disconnect) => {
                            let _ = event_tx.send(ClientEvent::Disconnected(format!(
                                "Broker disconnected: {:?}",
                                disconnect.reason_code()
                            )));
                            let _ = endpoint.close().await;
                            break;
                        }
                        other => {
                            let _ = event_tx.send(ClientEvent::Status(format!(
                                "Received packet: {:?}",
                                other.packet_type()
                            )));
                        }
                    }
                }
            }
        }
    });

    ClientHandle {
        shutdown_tx: Some(shutdown_tx),
        join_handle,
        event_rx,
        command_tx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mqtt_ep::result_code::{ConnectReasonCode, SubackReasonCode, UnsubackReasonCode};

    #[test]
    fn connack_only_accepts_success_and_explains_authentication_failures() {
        assert_eq!(connack_error(ConnectReasonCode::Success), None);
        assert!(
            connack_error(ConnectReasonCode::BadUserNameOrPassword)
                .unwrap()
                .contains("username or password")
        );
        assert!(
            connack_error(ConnectReasonCode::NotAuthorized)
                .unwrap()
                .contains("authorization")
        );
        assert!(
            connack_error(ConnectReasonCode::ServerBusy)
                .unwrap()
                .contains("ServerBusy")
        );
    }

    #[test]
    fn suback_uses_granted_qos_and_rejects_failures_or_wrong_cardinality() {
        assert_eq!(
            suback_result(&[SubackReasonCode::GrantedQos1]),
            Ok((1, "GrantedQos1".to_string()))
        );
        assert_eq!(
            suback_result(&[SubackReasonCode::NotAuthorized]),
            Err("NotAuthorized".to_string())
        );
        assert!(suback_result(&[]).unwrap_err().contains("exactly one"));
        assert!(
            suback_result(&[SubackReasonCode::GrantedQos0, SubackReasonCode::GrantedQos1])
                .unwrap_err()
                .contains("received 2")
        );
    }

    #[test]
    fn unsuback_treats_no_existing_subscription_as_success() {
        assert_eq!(
            unsuback_result(&[UnsubackReasonCode::NoSubscriptionExisted]),
            Ok("NoSubscriptionExisted".to_string())
        );
        assert_eq!(
            unsuback_result(&[UnsubackReasonCode::NotAuthorized]),
            Err("NotAuthorized".to_string())
        );
    }

    #[test]
    fn outgoing_qos_states_only_accept_the_expected_acknowledgements() {
        let qos1 = OutgoingPublishState::Puback {
            topic: "qos1".to_string(),
        };
        assert_eq!(qos1.complete_with_puback(), Ok("qos1".to_string()));

        let qos2 = OutgoingPublishState::Pubrec {
            topic: "qos2".to_string(),
        };
        let qos2 = qos2.after_successful_pubrec().unwrap();
        assert_eq!(
            qos2,
            OutgoingPublishState::Pubcomp {
                topic: "qos2".to_string()
            }
        );
        assert_eq!(qos2.complete_with_pubcomp(), Ok("qos2".to_string()));

        let wrong = OutgoingPublishState::Pubrec {
            topic: "wrong".to_string(),
        };
        assert!(wrong.complete_with_puback().is_err());
    }

    #[test]
    fn omitted_publish_ack_reason_code_means_success() {
        assert!(optional_reason_is_success(
            None::<mqtt_ep::result_code::PubackReasonCode>,
            |code| code.is_success()
        ));
        assert!(!optional_reason_is_success(
            Some(mqtt_ep::result_code::PubackReasonCode::NotAuthorized),
            |code| code.is_success()
        ));
    }
}
