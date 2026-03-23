use mqtt_endpoint_tokio::mqtt_ep;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, Once};
use std::sync::mpsc;
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

fn build_tls_config(login: &MqttLoginData, domain: &str) -> Result<Option<Arc<ClientConfig>>, String> {
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

            let file = File::open(path)
                .map_err(|err| format!("Failed to open CA PEM file '{path}': {err}"))?;
            let mut reader = BufReader::new(file);
            let mut found_cert = false;
            for cert in rustls_pemfile::certs(&mut reader) {
                let cert = cert
                    .map_err(|err| format!("Failed to read CA PEM file '{path}': {err}"))?;
                root_store.add(cert).map_err(|err| {
                    format!("Failed to add certificate from CA PEM file '{path}': {err}")
                })?;
                found_cert = true;
            }

            if !found_cert {
                return Err(format!("CA PEM file '{path}' did not contain any certificates"));
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
            mqtt_ep::packet::Packet::V5_0Connack(_) => {
                let _ = event_tx.send(ClientEvent::Connected);
                let _ = event_tx.send(ClientEvent::Status(format!("Connected to {display_label}")));
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
        let mut pending_publish: HashMap<u16, (String, bool)> = HashMap::new();

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
                                    continue;
                                }
                            };

                            if let Err(err) = endpoint.send(subscribe_packet).await {
                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to send SUBSCRIBE: {err}")));
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
                                    continue;
                                }
                            };

                            if let Err(err) = endpoint.send(unsubscribe_packet).await {
                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to send UNSUBSCRIBE: {err}")));
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
                                    continue;
                                }
                            };

                            if let Err(err) = endpoint.send(publish_packet).await {
                                let _ = event_tx.send(ClientEvent::Error(format!("Failed to send PUBLISH: {err}")));
                                continue;
                            }

                            if let Some(id) = packet_id {
                                pending_publish.insert(id, (topic.clone(), qos_level == mqtt_ep::packet::Qos::ExactlyOnce));
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

                            let _ = event_tx.send(ClientEvent::MessageReceived {
                                topic: topic.clone(),
                                qos: qos_to_u8(qos_level),
                                retain,
                                payload,
                            });

                            match qos_level {
                                mqtt_ep::packet::Qos::AtMostOnce => {}
                                mqtt_ep::packet::Qos::AtLeastOnce => {
                                    if let Some(packet_id) = publish.packet_id() {
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
                                    }
                                }
                                mqtt_ep::packet::Qos::ExactlyOnce => {
                                    if let Some(packet_id) = publish.packet_id() {
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
                                    }
                                }
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Suback(suback) => {
                            let packet_id = suback.packet_id();
                            if let Some((topic, qos)) = pending_subscribe.remove(&packet_id) {
                                let _ = event_tx.send(ClientEvent::Subscribed {
                                    topic,
                                    qos,
                                    details: format!("{:?}", suback.reason_codes()),
                                });
                            } else {
                                let _ = event_tx.send(ClientEvent::Status(format!(
                                    "SUBACK for unknown packet id {packet_id}"
                                )));
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Unsuback(unsuback) => {
                            let packet_id = unsuback.packet_id();
                            if let Some(topic) = pending_unsubscribe.remove(&packet_id) {
                                let _ = event_tx.send(ClientEvent::Unsubscribed {
                                    topic,
                                    details: format!("{:?}", unsuback.reason_codes()),
                                });
                            } else {
                                let _ = event_tx.send(ClientEvent::Status(format!(
                                    "UNSUBACK for unknown packet id {packet_id}"
                                )));
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Puback(puback) => {
                            let packet_id = puback.packet_id();
                            if let Some((topic, _)) = pending_publish.remove(&packet_id) {
                                let _ = event_tx.send(ClientEvent::Published {
                                    topic,
                                    packet_id: Some(packet_id),
                                });
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Pubrec(pubrec) => {
                            let packet_id = pubrec.packet_id();
                            if let Some((_, waiting_for_pubcomp)) = pending_publish.get_mut(&packet_id)
                                && *waiting_for_pubcomp
                            {
                                let pubrel = match mqtt_ep::packet::v5_0::Pubrel::builder()
                                    .packet_id(packet_id)
                                    .build()
                                {
                                    Ok(packet) => packet,
                                    Err(err) => {
                                        let _ = event_tx.send(ClientEvent::Error(format!("Failed to build PUBREL: {err}")));
                                        continue;
                                    }
                                };

                                if let Err(err) = endpoint.send(pubrel).await {
                                    let _ = event_tx.send(ClientEvent::Error(format!("Failed to send PUBREL: {err}")));
                                }
                            }
                        }
                        mqtt_ep::packet::Packet::V5_0Pubcomp(pubcomp) => {
                            let packet_id = pubcomp.packet_id();
                            if let Some((topic, _)) = pending_publish.remove(&packet_id) {
                                let _ = event_tx.send(ClientEvent::Published {
                                    topic,
                                    packet_id: Some(packet_id),
                                });
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
