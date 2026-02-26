#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use mqtt_endpoint_tokio::mqtt_ep;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabKind {
    Client,
}

#[derive(Clone, Debug)]
pub enum TabState {
    Client {
        mqtt_login: MqttLoginData,
        connection_status: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct MqttLoginData {
    pub broker: String,
    pub port: String,
    pub username: String,
    pub password: String,
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

#[derive(Debug)]
struct ClientHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: JoinHandle<()>,
    event_rx: Receiver<ClientEvent>,
}

#[derive(Debug)]
enum ClientEvent {
    Status(String),
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
    pub state: TabState,
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
                    },
                )
            }
        };

        self.tabs.push(Tab {
            id,
            title,
            kind,
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
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let client_id = format!("mqui-client-{tab_id}");

        let join_handle = self.runtime.spawn(async move {
            let _ = event_tx.send(ClientEvent::Status("Connecting to broker...".to_string()));

            let endpoint = mqtt_ep::endpoint::Endpoint::<mqtt_ep::role::Client>::new(
                mqtt_ep::Version::V5_0,
            );
            let addr = login.broker_addr();

            let connect_flow = async {
                let tcp_stream =
                    mqtt_ep::transport::connect_helper::connect_tcp(&addr, None)
                        .await
                        .map_err(|err| err.to_string())?;
                let transport = mqtt_ep::transport::TcpTransport::from_stream(tcp_stream);
                endpoint
                    .attach(transport, mqtt_ep::endpoint::Mode::Client)
                    .await
                    .map_err(|err| err.to_string())?;

                let mut connect_builder = mqtt_ep::packet::v5_0::Connect::builder()
                    .client_id(&client_id)
                    .map_err(|err| err.to_string())?
                    .keep_alive(60)
                    .clean_start(true);

                if let Some(username) = login.username_opt() {
                    connect_builder = connect_builder
                        .user_name(username)
                        .map_err(|err| err.to_string())?;

                    if let Some(password) = login.password_opt() {
                        connect_builder = connect_builder
                            .password(password.as_bytes().to_vec())
                            .map_err(|err| err.to_string())?;
                    }
                }

                let connect_packet = connect_builder.build().map_err(|err| err.to_string())?;

                endpoint.send(connect_packet).await.map_err(|err| err.to_string())?;

                let packet = endpoint.recv().await.map_err(|err| err.to_string())?;
                Ok::<String, String>(format!("Connected: {packet:?}"))
            };

            tokio::select! {
                _ = &mut shutdown_rx => {
                    let _ = endpoint.close().await;
                    let _ = event_tx.send(ClientEvent::Status("Closed".to_string()));
                }
                result = connect_flow => {
                    match result {
                        Ok(msg) => {
                            let _ = event_tx.send(ClientEvent::Status(msg));
                            let _ = shutdown_rx.await;
                            let _ = endpoint.close().await;
                            let _ = event_tx.send(ClientEvent::Status("Closed".to_string()));
                        }
                        Err(err) => {
                            let _ = event_tx.send(ClientEvent::Status(format!("Connection failed: {err}")));
                            let _ = endpoint.close().await;
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

    fn pump_client_events(&mut self) {
        for tab in &mut self.tabs {
            let TabState::Client {
                connection_status, ..
            } = &mut tab.state;

            let Some(client) = self.clients.get_mut(&tab.id) else {
                continue;
            };

            loop {
                match client.event_rx.try_recv() {
                    Ok(ClientEvent::Status(status)) => *connection_status = status,
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
                        ui.add(
                            egui::TextEdit::singleline(&mut self.mqtt_form.password)
                                .password(true),
                        );

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

            match &mut tab.state {
                TabState::Client {
                    mqtt_login,
                    connection_status,
                } => {
                    ui.heading("MQTT Login Data");
                    ui.label(format!("Broker: {}", mqtt_login.broker));
                    ui.label(format!("Port: {}", mqtt_login.port));
                    ui.label(format!(
                        "Username: {}",
                        mqtt_login.username_opt().unwrap_or("<not set>")
                    ));
                    ui.label(format!(
                        "Password: {}",
                        if mqtt_login.password_opt().is_some() {
                            "<set>"
                        } else {
                            "<not set>"
                        }
                    ));
                    ui.separator();
                    ui.label(format!("Client status: {connection_status}"));
                }
            }
        });
    }
}
