use std::collections::{HashMap, VecDeque};

use eframe::egui;
use tokio::runtime::Runtime;

use crate::app::state::{Tab, TabKind, TabState};
use crate::client;
use crate::models::client::ClientHandle;
use crate::models::ipc::ClientCommand;
use crate::models::mqtt::MqttLoginData;

pub(crate) mod events;
pub(crate) mod state;

pub struct App {
    pub(crate) next_tab_id: u64,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: Option<u64>,
    pub(crate) show_mqtt_popup: bool,
    pub(crate) mqtt_form: MqttLoginData,
    pub(crate) runtime: Runtime,
    pub(crate) clients: HashMap<u64, ClientHandle>,
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
    pub(crate) fn new_tab(&mut self, kind: TabKind, mqtt_login: MqttLoginData) {
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

        self.tabs.push(Tab { id, title, state });
        self.active_tab = Some(id);

        self.start_client(id);
    }

    pub(crate) fn close_tab(&mut self, tab_id: u64) {
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

        let handle = client::spawn_client(&self.runtime, tab_id, login);
        self.clients.insert(tab_id, handle);
    }

    fn stop_client(&mut self, tab_id: u64) {
        if let Some(mut handle) = self.clients.remove(&tab_id) {
            if let Some(shutdown_tx) = handle.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
            let _ = handle.join_handle.is_finished();
        }
    }

    pub(crate) fn send_client_command(&mut self, tab_id: u64, command: ClientCommand) {
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
        events::pump_client_events(self);
        crate::ui::render(self, ctx);
        ctx.request_repaint();
    }
}
