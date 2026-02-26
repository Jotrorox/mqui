#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use mqtt_endpoint_tokio::mqtt_ep;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tokio::task::JoinHandle;

const MAX_STORED_MESSAGES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TabKind {
    Client,
}

#[derive(Clone, Debug)]
enum TabState {
    Client {
        mqtt_login: MqttLoginData,
        connection_status: String,
        last_error: Option<String>,
        subscribe_topic: String,
        subscribe_qos: u8,
        unsubscribe_topic: String,
        publish_topic: String,
        publish_qos: u8,
        publish_retain: bool,
        publish_payload: String,
        payload_view_hex: bool,
        topic_filter: String,
        max_messages: usize,
        subscriptions: Vec<SubscriptionEntry>,
        messages: VecDeque<ReceivedMessage>,
        received_count: u64,
        published_count: u64,
    },
}

#[derive(Clone, Debug, Default)]
struct MqttLoginData {
    broker: String,
    port: String,
    username: String,
    password: String,
}

impl MqttLoginData {
    fn broker_addr(&self) -> String {
        let broker = self.broker.trim();
        if broker.is_empty() {
            return "127.0.0.1:1883".to_string();
        }

        if broker.contains(':') {
            broker.to_string()
        } else {
            let port = self.port.trim();
            let port = if port.is_empty() { "1883" } else { port };
            format!("{broker}:{port}")
        }
    }

    fn username_opt(&self) -> Option<&str> {
        let value = self.username.trim();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }

    fn password_opt(&self) -> Option<&str> {
        let value = self.password.trim();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }
}

#[derive(Clone, Debug)]
struct SubscriptionEntry {
    topic: String,
    qos: u8,
}

#[derive(Clone, Debug)]
struct ReceivedMessage {
    timestamp: SystemTime,
    topic: String,
    qos: u8,
    retain: bool,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct ClientHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: JoinHandle<()>,
    event_rx: Receiver<ClientEvent>,
    command_tx: tokio_mpsc::UnboundedSender<ClientCommand>,
}

#[derive(Debug)]
enum ClientEvent {
    Status(String),
    Error(String),
    Connected,
    Disconnected(String),
    Subscribed {
        topic: String,
        qos: u8,
        details: String,
    },
    Unsubscribed {
        topic: String,
        details: String,
    },
    Published {
        topic: String,
        packet_id: Option<u16>,
    },
    MessageReceived {
        topic: String,
        qos: u8,
        retain: bool,
        payload: Vec<u8>,
    },
}

#[derive(Debug)]
enum ClientCommand {
    Subscribe { topic: String, qos: u8 },
    Unsubscribe { topic: String },
    Publish {
        topic: String,
        payload: Vec<u8>,
        qos: u8,
        retain: bool,
    },
}

#[derive(Clone, Debug)]
struct Tab {
    id: u64,
    title: String,
    state: TabState,
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "MQUI",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

struct App {
    next_tab_id: u64,
    tabs: Vec<Tab>,
    active_tab: Option<u64>,
    show_mqtt_popup: bool,
    mqtt_form: MqttLoginData,
    runtime: Runtime,
    clients: HashMap<u64, ClientHandle>,
}

impl Default for App {
    fn default() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");

        Self {
            next_tab_id: 0,
            tabs: Vec::new(),
            active_tab: None,
            show_mqtt_popup: false,
            mqtt_form: MqttLoginData::default(),
            runtime,
            clients: HashMap::new(),
        }
    }
}

impl App {
    fn new_tab(&mut self, kind: TabKind, mqtt_login: MqttLoginData) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;

        let (title, state) = match kind {
            TabKind::Client => {
                let title = if mqtt_login.broker.is_empty() {
                    format!("Client {id}")
                } else {
                    mqtt_login.broker.clone()
                };
                (
                    title,
                    TabState::Client {
                        mqtt_login,
                        connection_status: "Connecting...".to_string(),
                        last_error: None,
                        subscribe_topic: "t1".to_string(),
                        subscribe_qos: 0,
                        unsubscribe_topic: "".to_string(),
                        publish_topic: "t1".to_string(),
                        publish_qos: 0,
                        publish_retain: false,
                        publish_payload: "hello".to_string(),
                        payload_view_hex: false,
                        topic_filter: "".to_string(),
                        max_messages: 200,
                        subscriptions: Vec::new(),
                        messages: VecDeque::new(),
                        received_count: 0,
                        published_count: 0,
                    },
                )
            }
        };

        self.tabs.push(Tab {
            id,
            title,
            state,
        });
        self.active_tab = Some(id);

        self.start_client(id);
    }

    fn close_tab(&mut self, tab_id: u64) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };

        self.stop_client(tab_id);
        self.tabs.remove(idx);

        if self.active_tab == Some(tab_id) {
            self.active_tab = if self.tabs.is_empty() {
                None
            } else if idx > 0 {
                Some(self.tabs[idx - 1].id)
            } else {
                Some(self.tabs[0].id)
            };
        }
    }

    fn start_client(&mut self, tab_id: u64) {
        let Some(login) = self.tabs.iter().find_map(|tab| {
            if tab.id != tab_id {
                return None;
            }

            match &tab.state {
                TabState::Client { mqtt_login, .. } => Some(mqtt_login.clone()),
            }
        }) else {
            return;
        };

        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, mut command_rx) = tokio_mpsc::unbounded_channel::<ClientCommand>();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let client_id = format!("mqui-client-{tab_id}");

        let join_handle = self.runtime.spawn(async move {
            let _ = event_tx.send(ClientEvent::Status("Connecting to broker...".to_string()));

            let endpoint = mqtt_ep::endpoint::Endpoint::<mqtt_ep::role::Client>::new(
                mqtt_ep::Version::V5_0,
            );
            let addr = login.broker_addr();

            let tcp_stream = match mqtt_ep::transport::connect_helper::connect_tcp(&addr, None).await {
                Ok(stream) => stream,
                Err(err) => {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!(
                        "TCP connect failed: {err}"
                    )));
                    return;
                }
            };

            let transport = mqtt_ep::transport::TcpTransport::from_stream(tcp_stream);
            if let Err(err) = endpoint
                .attach(transport, mqtt_ep::endpoint::Mode::Client)
                .await
            {
                let _ = event_tx.send(ClientEvent::Disconnected(format!(
                    "Attach failed: {err}"
                )));
                return;
            }

            let mut connect_builder = match mqtt_ep::packet::v5_0::Connect::builder().client_id(&client_id) {
                Ok(builder) => builder.keep_alive(60).clean_start(true),
                Err(err) => {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!(
                        "Client ID setup failed: {err}"
                    )));
                    let _ = endpoint.close().await;
                    return;
                }
            };

            if let Some(username) = login.username_opt() {
                connect_builder = match connect_builder.user_name(username) {
                    Ok(builder) => builder,
                    Err(err) => {
                        let _ = event_tx.send(ClientEvent::Disconnected(format!(
                            "Username setup failed: {err}"
                        )));
                        let _ = endpoint.close().await;
                        return;
                    }
                };

                if let Some(password) = login.password_opt() {
                    connect_builder = match connect_builder.password(password.as_bytes().to_vec()) {
                        Ok(builder) => builder,
                        Err(err) => {
                            let _ = event_tx.send(ClientEvent::Disconnected(format!(
                                "Password setup failed: {err}"
                            )));
                            let _ = endpoint.close().await;
                            return;
                        }
                    };
                }
            }

            let connect_packet = match connect_builder.build() {
                Ok(packet) => packet,
                Err(err) => {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!(
                        "CONNECT build failed: {err}"
                    )));
                    let _ = endpoint.close().await;
                    return;
                }
            };

            if let Err(err) = endpoint.send(connect_packet).await {
                let _ = event_tx.send(ClientEvent::Disconnected(format!(
                    "CONNECT send failed: {err}"
                )));
                let _ = endpoint.close().await;
                return;
            }

            let connack = match endpoint.recv().await {
                Ok(packet) => packet,
                Err(err) => {
                    let _ = event_tx.send(ClientEvent::Disconnected(format!(
                        "CONNACK recv failed: {err}"
                    )));
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

        self.clients.insert(
            tab_id,
            ClientHandle {
                shutdown_tx: Some(shutdown_tx),
                join_handle,
                event_rx,
                command_tx,
            },
        );
    }

    fn stop_client(&mut self, tab_id: u64) {
        if let Some(mut handle) = self.clients.remove(&tab_id) {
            if let Some(shutdown_tx) = handle.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
            let _ = handle.join_handle.is_finished();
        }
    }

    fn send_client_command(&mut self, tab_id: u64, command: ClientCommand) {
        let Some(client) = self.clients.get_mut(&tab_id) else {
            return;
        };

        if client.command_tx.send(command).is_err() {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                let TabState::Client {
                    connection_status,
                    last_error,
                    ..
                } = &mut tab.state;
                *connection_status = "Client task is not available".to_string();
                *last_error = Some("Command channel is closed".to_string());
            }
        }
    }

    fn pump_client_events(&mut self) {
        for tab in &mut self.tabs {
            let TabState::Client {
                connection_status,
                last_error,
                subscriptions,
                messages,
                received_count,
                published_count,
                ..
            } = &mut tab.state;

            let Some(client) = self.clients.get_mut(&tab.id) else {
                continue;
            };

            loop {
                match client.event_rx.try_recv() {
                    Ok(ClientEvent::Status(status)) => {
                        *connection_status = status;
                    }
                    Ok(ClientEvent::Error(err)) => {
                        *last_error = Some(err);
                    }
                    Ok(ClientEvent::Connected) => {
                        *connection_status = "Connected".to_string();
                        *last_error = None;
                    }
                    Ok(ClientEvent::Disconnected(msg)) => {
                        *connection_status = "Disconnected".to_string();
                        *last_error = Some(msg);
                    }
                    Ok(ClientEvent::Subscribed {
                        topic,
                        qos,
                        details,
                    }) => {
                        if let Some(entry) = subscriptions.iter_mut().find(|entry| entry.topic == topic) {
                            entry.qos = qos;
                        } else {
                            subscriptions.push(SubscriptionEntry { topic: topic.clone(), qos });
                        }
                        *connection_status = format!("Subscribed to '{topic}'");
                        *last_error = Some(format!("SUBACK: {details}"));
                    }
                    Ok(ClientEvent::Unsubscribed { topic, details }) => {
                        subscriptions.retain(|entry| entry.topic != topic);
                        *connection_status = format!("Unsubscribed from '{topic}'");
                        *last_error = Some(format!("UNSUBACK: {details}"));
                    }
                    Ok(ClientEvent::Published { topic, packet_id }) => {
                        *published_count += 1;
                        if let Some(id) = packet_id {
                            *connection_status = format!("Published to '{topic}' (packet id {id})");
                        } else {
                            *connection_status = format!("Published to '{topic}'");
                        }
                    }
                    Ok(ClientEvent::MessageReceived {
                        topic,
                        qos,
                        retain,
                        payload,
                    }) => {
                        *received_count += 1;
                        messages.push_back(ReceivedMessage {
                            timestamp: SystemTime::now(),
                            topic,
                            qos,
                            retain,
                            payload,
                        });
                        while messages.len() > MAX_STORED_MESSAGES {
                            let _ = messages.pop_front();
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }
    }

    fn stop_all_clients(&mut self) {
        let ids: Vec<u64> = self.clients.keys().copied().collect();
        for id in ids {
            self.stop_client(id);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_all_clients();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_client_events();

        let top_bar_fill = ctx.style().visuals.panel_fill;

        egui::TopBottomPanel::top("tab_bar")
            .exact_height(40.0)
            .frame(
                egui::Frame::new()
                    .fill(top_bar_fill)
                    .inner_margin(egui::Margin::symmetric(6, 5)),
            )
            .show(ctx, |ui| {
                let mut tab_to_activate = None;
                let mut tab_to_close = None;
                let mut add_tab = false;

                ui.horizontal(|ui| {
                    ui.set_height(ui.available_height());
                    ui.spacing_mut().item_spacing.x = 2.0;

                    egui::ScrollArea::horizontal()
                        .id_salt("tabs_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for tab in &self.tabs {
                                    let selected = self.active_tab == Some(tab.id);
                                    let frame_fill = if selected {
                                        ui.visuals().selection.bg_fill
                                    } else {
                                        ui.visuals().widgets.inactive.bg_fill
                                    };
                                    let frame_stroke = if selected {
                                        ui.visuals().selection.stroke
                                    } else {
                                        ui.visuals().widgets.inactive.bg_stroke
                                    };
                                    let title_color = if selected {
                                        ui.visuals().selection.stroke.color
                                    } else {
                                        ui.visuals().text_color()
                                    };

                                    egui::Frame::new()
                                        .fill(frame_fill)
                                        .stroke(frame_stroke)
                                        .corner_radius(2.0)
                                        .inner_margin(egui::Margin::symmetric(12, 7))
                                        .show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.x = 8.0;

                                            let tab_response = ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&tab.title).color(title_color),
                                                )
                                                .sense(egui::Sense::click()),
                                            );
                                            if tab_response.clicked() {
                                                tab_to_activate = Some(tab.id);
                                            }

                                            if tab_response.hovered() || selected {
                                                let close_response = ui.add(
                                                    egui::Button::new(
                                                        egui::RichText::new("✕").small().strong(),
                                                    )
                                                    .small()
                                                    .frame(false),
                                                );
                                                if close_response.clicked() {
                                                    tab_to_close = Some(tab.id);
                                                }
                                            } else {
                                                ui.add_space(12.0);
                                            }
                                        });
                                }
                            });
                        });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        add_tab = ui
                            .add(
                                egui::Button::new(egui::RichText::new("+").strong())
                                    .small()
                                    .min_size(egui::vec2(26.0, 28.0)),
                            )
                            .clicked();
                    });
                });

                if let Some(id) = tab_to_activate {
                    self.active_tab = Some(id);
                }

                if let Some(id) = tab_to_close {
                    self.close_tab(id);
                }

                if add_tab {
                    self.show_mqtt_popup = true;
                }
            });

        if self.show_mqtt_popup {
            let mut open = self.show_mqtt_popup;
            let mut create_client = false;

            egui::Window::new("MQTT Login")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label("Broker");
                        ui.text_edit_singleline(&mut self.mqtt_form.broker);

                        ui.label("Port");
                        ui.text_edit_singleline(&mut self.mqtt_form.port);

                        ui.label("Username (optional)");
                        ui.text_edit_singleline(&mut self.mqtt_form.username);

                        ui.label("Password (optional)");
                        ui.add(egui::TextEdit::singleline(&mut self.mqtt_form.password).password(true));

                        ui.add_space(8.0);
                        if ui.button("Add client").clicked() {
                            create_client = true;
                        }
                    });
                });

            if create_client {
                self.new_tab(TabKind::Client, self.mqtt_form.clone());
                self.mqtt_form = MqttLoginData::default();
                open = false;
            }

            self.show_mqtt_popup = open;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(active_id) = self.active_tab else {
                ui.label("No client open. Press + to add an MQTT client.");
                return;
            };

            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active_id) else {
                ui.label("Active tab missing");
                return;
            };

            let mut commands_to_send: Vec<ClientCommand> = Vec::new();

            match &mut tab.state {
                TabState::Client {
                    mqtt_login,
                    connection_status,
                    last_error,
                    subscribe_topic,
                    subscribe_qos,
                    unsubscribe_topic,
                    publish_topic,
                    publish_qos,
                    publish_retain,
                    publish_payload,
                    payload_view_hex,
                    topic_filter,
                    max_messages,
                    subscriptions,
                    messages,
                    received_count,
                    published_count,
                } => {
                    ui.heading("MQTT Client");
                    ui.label(format!("Broker: {}", mqtt_login.broker_addr()));
                    ui.label(format!("Status: {connection_status}"));
                    if let Some(err) = last_error {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("Info: {err}"));
                    }
                    ui.label(format!(
                        "Totals: {} received / {} published",
                        received_count, published_count
                    ));

                    ui.separator();
                    ui.heading("Subscriptions");
                    ui.horizontal(|ui| {
                        ui.label("Topic");
                        ui.text_edit_singleline(subscribe_topic);
                        ui.label("QoS");
                        qos_picker(ui, "sub_qos", subscribe_qos);
                        if ui.button("Subscribe").clicked() {
                            let topic = subscribe_topic.trim().to_string();
                            if !topic.is_empty() {
                                commands_to_send.push(ClientCommand::Subscribe {
                                    topic: topic.clone(),
                                    qos: *subscribe_qos,
                                });
                                *unsubscribe_topic = topic;
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Unsubscribe topic");
                        ui.text_edit_singleline(unsubscribe_topic);
                        if ui.button("Unsubscribe").clicked() {
                            let topic = unsubscribe_topic.trim().to_string();
                            if !topic.is_empty() {
                                commands_to_send.push(ClientCommand::Unsubscribe { topic });
                            }
                        }
                    });

                    let mut remove_topic: Option<String> = None;
                    egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                        if subscriptions.is_empty() {
                            ui.label("No active subscriptions");
                        } else {
                            for entry in subscriptions.iter() {
                                ui.horizontal(|ui| {
                                    ui.label(format!("{} (QoS {})", entry.topic, entry.qos));
                                    if ui.small_button("Remove").clicked() {
                                        remove_topic = Some(entry.topic.clone());
                                    }
                                });
                            }
                        }
                    });
                    if let Some(topic) = remove_topic {
                        commands_to_send.push(ClientCommand::Unsubscribe { topic: topic.clone() });
                        *unsubscribe_topic = topic;
                    }

                    ui.separator();
                    ui.heading("Publish");
                    ui.horizontal(|ui| {
                        ui.label("Topic");
                        ui.text_edit_singleline(publish_topic);
                        ui.label("QoS");
                        qos_picker(ui, "pub_qos", publish_qos);
                        ui.checkbox(publish_retain, "Retain");
                    });
                    ui.label("Payload");
                    ui.add(egui::TextEdit::multiline(publish_payload).desired_rows(3));
                    if ui.button("Publish message").clicked() {
                        let topic = publish_topic.trim().to_string();
                        if !topic.is_empty() {
                            commands_to_send.push(ClientCommand::Publish {
                                topic,
                                payload: publish_payload.as_bytes().to_vec(),
                                qos: *publish_qos,
                                retain: *publish_retain,
                            });
                        }
                    }

                    ui.separator();
                    ui.heading("Messages");
                    ui.horizontal(|ui| {
                        ui.label("Filter");
                        ui.text_edit_singleline(topic_filter);
                        ui.label("Max rows");
                        ui.add(egui::DragValue::new(max_messages).range(1..=1000));
                        ui.checkbox(payload_view_hex, "Hex payload");
                        if ui.button("Clear").clicked() {
                            messages.clear();
                        }
                    });

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let filter = topic_filter.trim();
                        let mut shown = 0usize;

                        for msg in messages.iter().rev() {
                            if !filter.is_empty() && !msg.topic.contains(filter) {
                                continue;
                            }
                            if shown >= *max_messages {
                                break;
                            }

                            let ts = format_timestamp(msg.timestamp);
                            let payload_text = format_payload(&msg.payload, *payload_view_hex);
                            ui.group(|ui| {
                                ui.label(format!("[{ts}] {}", msg.topic));
                                ui.label(format!("QoS {} | retain {}", msg.qos, msg.retain));
                                ui.label(payload_text);
                            });
                            shown += 1;
                        }

                        if shown == 0 {
                            ui.label("No messages matched current filter.");
                        }
                    });
                }
            }

            for command in commands_to_send {
                self.send_client_command(active_id, command);
            }
        });

        ctx.request_repaint();
    }
}

fn qos_picker(ui: &mut egui::Ui, id: &str, value: &mut u8) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.to_string())
        .show_ui(ui, |ui| {
            ui.selectable_value(value, 0, "0");
            ui.selectable_value(value, 1, "1");
            ui.selectable_value(value, 2, "2");
        });
}

fn qos_to_u8(qos: mqtt_ep::packet::Qos) -> u8 {
    match qos {
        mqtt_ep::packet::Qos::AtMostOnce => 0,
        mqtt_ep::packet::Qos::AtLeastOnce => 1,
        mqtt_ep::packet::Qos::ExactlyOnce => 2,
    }
}

fn format_timestamp(ts: SystemTime) -> String {
    match ts.duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}", duration.as_secs()),
        Err(_) => "0".to_string(),
    }
}

fn format_payload(payload: &[u8], as_hex: bool) -> String {
    if as_hex {
        return payload
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
    }

    match String::from_utf8(payload.to_vec()) {
        Ok(text) => text,
        Err(_) => payload
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}
