use mqtt_endpoint_tokio::mqtt_ep;
use std::collections::HashMap;
use std::sync::mpsc;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::models::client::ClientHandle;
use crate::models::ipc::{ClientCommand, ClientEvent};
use crate::models::mqtt::MqttLoginData;
use crate::utils::qos::qos_to_u8;

pub(crate) fn spawn_client(runtime: &Runtime, tab_id: u64, login: MqttLoginData) -> ClientHandle {
    let (event_tx, event_rx) = mpsc::channel();
    let (command_tx, mut command_rx) = tokio_mpsc::unbounded_channel::<ClientCommand>();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let client_id = format!("mqui-client-{}-{tab_id}", std::process::id());

    let join_handle = runtime.spawn(async move {
        let _ = event_tx.send(ClientEvent::Status("Connecting to broker...".to_string()));

        let endpoint = mqtt_ep::endpoint::Endpoint::<mqtt_ep::role::Client>::new(mqtt_ep::Version::V5_0);
        let addr = login.broker_addr();

        let tcp_stream = match mqtt_ep::transport::connect_helper::connect_tcp(&addr, None).await {
            Ok(stream) => stream,
            Err(err) => {
                let _ = event_tx.send(ClientEvent::Disconnected(format!("TCP connect failed: {err}")));
                return;
            }
        };

        let transport = mqtt_ep::transport::TcpTransport::from_stream(tcp_stream);
        if let Err(err) = endpoint
            .attach(transport, mqtt_ep::endpoint::Mode::Client)
            .await
        {
            let _ = event_tx.send(ClientEvent::Disconnected(format!("Attach failed: {err}")));
            return;
        }

        let mut connect_builder = match mqtt_ep::packet::v5_0::Connect::builder().client_id(&client_id) {
            Ok(builder) => builder.keep_alive(60).clean_start(true),
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
                let _ = event_tx.send(ClientEvent::Status(format!("Connected to {addr}")));
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
                            if let Some((_, waiting_for_pubcomp)) = pending_publish.get_mut(&packet_id) {
                                if *waiting_for_pubcomp {
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
