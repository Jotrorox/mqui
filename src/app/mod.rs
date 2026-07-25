use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::runtime::Runtime;

use crate::app::config_profiles::ProfileEntry;
use crate::app::state::{
    ActionableError, ActivityLevel, ErrorScope, MessageFilterMode, PayloadView, Tab, TabKind,
    TabState,
};
use crate::client;
use crate::models::client::ClientHandle;
use crate::models::ipc::{ClientCommand, ConnectionState};
use crate::models::mqtt::MqttLoginData;

pub(crate) mod config_profiles;
pub(crate) mod events;
pub(crate) mod persistence;
pub(crate) mod state;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientCommandResult {
    Accepted,
    NoClientHandle,
    ClientTaskFinished,
    ChannelFull,
    ChannelClosed,
}

impl ClientCommandResult {
    pub(crate) const fn user_message(self) -> Option<&'static str> {
        match self {
            Self::Accepted => None,
            Self::NoClientHandle => Some("No client is running; connect and try again."),
            Self::ClientTaskFinished => {
                Some("The client task has finished; reconnect and try again.")
            }
            Self::ChannelFull => Some("The client command queue is full; wait and try again."),
            Self::ChannelClosed => {
                Some("The client command channel is closed; reconnect and try again.")
            }
        }
    }
}

pub struct App {
    pub(crate) next_tab_id: u64,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: Option<u64>,
    pub(crate) show_mqtt_popup: bool,
    pub(crate) renaming_tab: Option<u64>,
    pub(crate) rename_buffer: String,
    pub(crate) dragging_tab: Option<u64>,
    pub(crate) mqtt_form: MqttLoginData,
    pub(crate) profile_entries: Vec<ProfileEntry>,
    pub(crate) selected_profile_id: Option<String>,
    pub(crate) profile_status: Option<String>,
    pub(crate) confirm_profile_overwrite: bool,
    pub(crate) profile_rename_open: bool,
    pub(crate) profile_rename_buffer: String,
    pub(crate) confirm_profile_delete: bool,
    pub(crate) runtime: Runtime,
    pub(crate) clients: HashMap<u64, ClientHandle>,
    pub(crate) repaint_context: egui::Context,
    pub(crate) reconnect_attempts: HashMap<u64, u32>,
    pub(crate) reconnect_deadlines: HashMap<u64, std::time::Instant>,
    pub(crate) manually_disconnected: HashSet<u64>,
    pub(crate) connection_states: HashMap<u64, ConnectionState>,
    pub(crate) workspace_warning: Option<String>,
    workspace_path: Option<std::path::PathBuf>,
    workspace_snapshot: Vec<u8>,
    workspace_dirty_since: Option<Instant>,
    restored_connections_pending: Vec<u64>,
}

impl Default for App {
    fn default() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");

        let mut app = Self {
            next_tab_id: 0,
            tabs: Vec::new(),
            active_tab: None,
            show_mqtt_popup: false,
            renaming_tab: None,
            rename_buffer: String::new(),
            dragging_tab: None,
            mqtt_form: MqttLoginData::default(),
            profile_entries: Vec::new(),
            selected_profile_id: None,
            profile_status: None,
            confirm_profile_overwrite: false,
            profile_rename_open: false,
            profile_rename_buffer: String::new(),
            confirm_profile_delete: false,
            runtime,
            clients: HashMap::new(),
            repaint_context: egui::Context::default(),
            reconnect_attempts: HashMap::new(),
            reconnect_deadlines: HashMap::new(),
            manually_disconnected: HashSet::new(),
            connection_states: HashMap::new(),
            workspace_warning: None,
            workspace_path: persistence::workspace_path().ok(),
            workspace_snapshot: Vec::new(),
            workspace_dirty_since: None,
            restored_connections_pending: Vec::new(),
        };

        app.refresh_profiles();
        app.load_workspace();
        app
    }
}

impl App {
    /// Restores durable UI state but deliberately creates no network clients.
    fn load_workspace(&mut self) {
        let Some(path) = self.workspace_path.clone() else {
            self.workspace_warning =
                Some("Workspace persistence is unavailable on this platform.".to_string());
            return;
        };
        match persistence::load(&path) {
            Ok(Some(workspace)) => {
                let restored = workspace.restore();
                self.tabs = restored.tabs;
                self.active_tab = restored.active_tab;
                self.next_tab_id = restored.next_tab_id;
                self.connection_states = self
                    .tabs
                    .iter()
                    .map(|tab| (tab.id, ConnectionState::Disconnected))
                    .collect();
                self.restored_connections_pending = restored.reconnect_tabs;
            }
            Ok(None) => {}
            Err(err) => {
                self.workspace_warning = Some(format!(
                    "Could not restore workspace from {}: {err}",
                    path.display()
                ));
            }
        }
        self.workspace_snapshot = persistence::serialize(self).unwrap_or_default();
    }

    fn save_workspace(&mut self, force: bool) {
        let Some(path) = self.workspace_path.as_deref() else {
            return;
        };
        let Ok(snapshot) = persistence::serialize(self) else {
            self.workspace_warning = Some("Could not serialize workspace state.".to_string());
            return;
        };
        if snapshot != self.workspace_snapshot {
            self.workspace_snapshot = snapshot;
            self.workspace_dirty_since.get_or_insert_with(Instant::now);
        }
        let due = self
            .workspace_dirty_since
            .is_some_and(|since| since.elapsed() >= Duration::from_millis(750));
        if force || (due && self.workspace_dirty_since.is_some()) {
            match persistence::atomic_write(path, &self.workspace_snapshot) {
                Ok(()) => self.workspace_dirty_since = None,
                Err(err) => {
                    self.workspace_warning = Some(format!(
                        "Could not save workspace to {}: {err}",
                        path.display()
                    ));
                }
            }
        }
    }

    pub(crate) fn new_tab(&mut self, kind: TabKind, mqtt_login: MqttLoginData) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;

        let (title, state) = match kind {
            TabKind::Client => {
                let custom_name = mqtt_login.name.trim();
                let title = if !custom_name.is_empty() {
                    custom_name.to_string()
                } else if mqtt_login.connection_mode
                    == crate::models::mqtt::ConnectionInputMode::Url
                {
                    let connection_url = mqtt_login.connection_url.trim();
                    if !connection_url.is_empty() {
                        connection_url.to_string()
                    } else {
                        mqtt_login
                            .resolve_connection()
                            .map(|resolved| resolved.display_label)
                            .unwrap_or_else(|_| format!("Client {id}"))
                    }
                } else if !mqtt_login.broker.trim().is_empty() {
                    mqtt_login.broker.trim().to_string()
                } else {
                    mqtt_login
                        .resolve_connection()
                        .map(|resolved| resolved.display_label)
                        .unwrap_or_else(|_| format!("Client {id}"))
                };
                (
                    title,
                    TabState::Client {
                        mqtt_login,
                        connection_state: ConnectionState::Connecting,
                        current_error: None,
                        activity: VecDeque::new(),
                        subscribe_topic: "t1".to_string(),
                        subscribe_qos: 0,
                        unsubscribe_topic: "".to_string(),
                        editing_subscription_topic: None,
                        editing_subscription_value: String::new(),
                        editing_subscription_qos: 0,
                        publish_topic: "t1".to_string(),
                        publish_qos: 0,
                        publish_retain: false,
                        publish_payload: "hello".to_string(),
                        payload_view: PayloadView::Text,
                        topic_filter: "".to_string(),
                        message_filter_mode: MessageFilterMode::Substring,
                        payload_search: String::new(),
                        max_messages: 200,
                        capture_paused: false,
                        paused_message_count: 0,
                        next_message_id: 0,
                        selected_message_id: None,
                        subscriptions: Vec::new(),
                        messages: VecDeque::new(),
                        received_count: 0,
                        dropped_message_count: 0,
                        current_client_dropped_message_count: 0,
                        published_count: 0,
                    },
                )
            }
        };

        self.tabs.push(Tab { id, title, state });
        self.connection_states
            .insert(id, ConnectionState::Connecting);
        self.active_tab = Some(id);

        self.start_client(id);
    }

    pub(crate) fn close_tab(&mut self, tab_id: u64) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };

        self.stop_client(tab_id);
        self.connection_states.remove(&tab_id);
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

    pub(crate) fn disconnect_client(&mut self, tab_id: u64) {
        let was_connected =
            self.connection_states.get(&tab_id) == Some(&ConnectionState::Connected);
        self.manually_disconnected.insert(tab_id);
        self.reconnect_deadlines.remove(&tab_id);
        self.set_connection_state(tab_id, ConnectionState::Disconnecting);
        if was_connected {
            if self.send_client_command(tab_id, ClientCommand::Disconnect)
                != ClientCommandResult::Accepted
            {
                self.stop_client(tab_id);
                self.set_connection_state(tab_id, ConnectionState::Disconnected);
            }
        } else {
            self.stop_client(tab_id);
            self.set_connection_state(tab_id, ConnectionState::Disconnected);
        }
    }

    pub(crate) fn force_disconnect_client(&mut self, tab_id: u64) {
        self.manually_disconnected.insert(tab_id);
        self.reconnect_deadlines.remove(&tab_id);
        self.set_connection_state(tab_id, ConnectionState::Disconnecting);
        self.stop_client(tab_id);
        self.set_connection_state(tab_id, ConnectionState::Disconnected);
    }

    pub(crate) fn reconnect_client(&mut self, tab_id: u64) {
        self.stop_client(tab_id);
        self.reconnect_attempts.remove(&tab_id);
        self.reconnect_deadlines.remove(&tab_id);
        self.manually_disconnected.remove(&tab_id);
        self.set_connection_state(tab_id, ConnectionState::Reconnecting);

        self.start_client(tab_id);
    }

    pub(crate) fn cancel_reconnect(&mut self, tab_id: u64) {
        self.manually_disconnected.insert(tab_id);
        self.reconnect_deadlines.remove(&tab_id);
        self.set_connection_state(tab_id, ConnectionState::Disconnecting);
        self.stop_client(tab_id);
        self.set_connection_state(tab_id, ConnectionState::Disconnected);
    }

    pub(crate) fn duplicate_tab(&mut self, tab_id: u64) {
        let Some((title, login)) = self.tabs.iter().find_map(|tab| {
            if tab.id != tab_id {
                return None;
            }

            let TabState::Client { mqtt_login, .. } = &tab.state;
            Some((tab.title.clone(), mqtt_login.clone()))
        }) else {
            return;
        };

        self.new_tab(TabKind::Client, login);
        if let Some(new_tab) = self.tabs.last_mut() {
            new_tab.title = format!("{title} copy");
        }
    }

    pub(crate) fn rename_tab(&mut self, tab_id: u64, new_title: String) {
        let title = new_title.trim();
        if title.is_empty() {
            return;
        }

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = title.to_string();
        }
    }

    pub(crate) fn reorder_tabs(&mut self, source_id: u64, target_id: u64) {
        if source_id == target_id {
            return;
        }

        let Some(source_idx) = self.tabs.iter().position(|tab| tab.id == source_id) else {
            return;
        };
        let Some(target_idx) = self.tabs.iter().position(|tab| tab.id == target_id) else {
            return;
        };

        let tab = self.tabs.remove(source_idx);
        let insertion_idx = if source_idx < target_idx {
            target_idx - 1
        } else {
            target_idx
        };
        self.tabs.insert(insertion_idx, tab);
    }

    fn start_client(&mut self, tab_id: u64) {
        let Some(login) = self.tabs.iter_mut().find_map(|tab| {
            if tab.id != tab_id {
                return None;
            }

            match &mut tab.state {
                TabState::Client {
                    mqtt_login,
                    current_client_dropped_message_count,
                    ..
                } => {
                    *current_client_dropped_message_count = 0;
                    Some(mqtt_login.clone())
                }
            }
        }) else {
            return;
        };

        let subscriptions = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| match &tab.state {
                TabState::Client { subscriptions, .. } => subscriptions.clone(),
            })
            .unwrap_or_default();
        let handle =
            client::spawn_client(&self.runtime, tab_id, login, self.repaint_context.clone());
        for subscription in subscriptions {
            let _ = handle.command_tx.try_send(ClientCommand::Subscribe {
                topic: subscription.topic,
                qos: subscription.qos,
            });
        }
        self.clients.insert(tab_id, handle);
    }

    fn stop_client(&mut self, tab_id: u64) {
        self.reconnect_deadlines.remove(&tab_id);
        if let Some(handle) = self.clients.remove(&tab_id) {
            handle.cancellation.cancel();
            handle.join_handle.abort();
        }
    }

    pub(crate) fn send_client_command(
        &mut self,
        tab_id: u64,
        command: ClientCommand,
    ) -> ClientCommandResult {
        let result = match self.clients.get_mut(&tab_id) {
            None => ClientCommandResult::NoClientHandle,
            Some(client) if client.join_handle.is_finished() => {
                ClientCommandResult::ClientTaskFinished
            }
            Some(client) => match client.command_tx.try_send(command) {
                Ok(()) => ClientCommandResult::Accepted,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    ClientCommandResult::ChannelFull
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    ClientCommandResult::ChannelClosed
                }
            },
        };
        if let Some(message) = result.user_message()
            && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id)
        {
            let TabState::Client { current_error, .. } = &mut tab.state;
            *current_error = Some(ActionableError {
                message: message.to_string(),
                scope: ErrorScope::General,
            });
        }
        result
    }

    pub(crate) fn refresh_profiles(&mut self) {
        match config_profiles::list_profiles() {
            Ok(entries) => {
                self.profile_entries = entries;
                if let Some(selected) = &self.selected_profile_id {
                    let exists = self
                        .profile_entries
                        .iter()
                        .any(|entry| &entry.id == selected);
                    if !exists {
                        self.selected_profile_id = None;
                    }
                }
                let warning_count = self
                    .profile_entries
                    .iter()
                    .filter(|entry| entry.warning.is_some())
                    .count();
                if warning_count > 0 {
                    self.profile_status = Some(format!(
                        "{warning_count} profile file(s) need attention; see the profile list"
                    ));
                }
            }
            Err(err) => {
                self.profile_entries.clear();
                self.selected_profile_id = None;
                self.profile_status = Some(err);
            }
        }
    }

    pub(crate) fn save_current_profile(&mut self, overwrite: bool) {
        let profile_name = self.mqtt_form.name.trim();
        if profile_name.is_empty() {
            self.profile_status = Some("Name is required to save configuration".to_string());
            return;
        }
        if let Err(err) = self.mqtt_form.resolve_connection() {
            self.profile_status = Some(err);
            return;
        }

        let result = if overwrite {
            self.selected_profile_id
                .as_deref()
                .ok_or_else(|| "No profile is selected for overwrite".to_string())
                .and_then(|id| {
                    config_profiles::overwrite_profile(id, profile_name, &self.mqtt_form)
                })
                .map(|()| self.selected_profile_id.clone().unwrap_or_default())
        } else {
            config_profiles::create_profile(profile_name, &self.mqtt_form)
        };
        match result {
            Ok(id) => {
                self.selected_profile_id = Some(id);
                self.profile_status = Some(format!("Saved profile '{profile_name}'"));
                self.confirm_profile_overwrite = false;
                self.refresh_profiles();
            }
            Err(err) => {
                self.profile_status = Some(err);
            }
        }
    }

    pub(crate) fn load_profile_into_form(&mut self, profile_id: &str) {
        let Some(entry) = self
            .profile_entries
            .iter()
            .find(|entry| entry.id == profile_id)
            .cloned()
        else {
            self.profile_status = Some("Profile not found".to_string());
            return;
        };

        match config_profiles::load_profile_file(&entry.file_path) {
            Ok(login) => {
                self.mqtt_form = login;
                self.selected_profile_id = Some(profile_id.to_string());
                self.profile_status = Some(format!("Loaded profile '{}'", entry.display_name));
            }
            Err(err) => {
                self.profile_status = Some(err);
            }
        }
    }

    pub(crate) fn load_template_from_file_picker(&mut self) {
        let file = rfd::FileDialog::new()
            .add_filter("TOML", &["toml"])
            .pick_file();

        let Some(path) = file else {
            return;
        };

        match config_profiles::load_template_file(&path) {
            Ok(login) => {
                self.mqtt_form = login;
                self.selected_profile_id = None;
                self.profile_status = Some(format!("Loaded template {}", path.display()));
            }
            Err(err) => {
                self.profile_status = Some(err);
            }
        }
    }

    pub(crate) fn rename_selected_profile(&mut self) {
        let Some(id) = self.selected_profile_id.as_deref() else {
            self.profile_status = Some("No profile selected".into());
            return;
        };
        match config_profiles::rename_profile(id, &self.profile_rename_buffer) {
            Ok(()) => {
                self.profile_status = Some(format!(
                    "Renamed profile to '{}'",
                    self.profile_rename_buffer.trim()
                ));
                self.profile_rename_open = false;
                self.refresh_profiles();
            }
            Err(err) => self.profile_status = Some(err),
        }
    }

    pub(crate) fn delete_selected_profile(&mut self) {
        let Some(id) = self.selected_profile_id.clone() else {
            self.profile_status = Some("No profile selected".into());
            return;
        };
        match config_profiles::delete_profile(&id) {
            Ok(()) => {
                self.selected_profile_id = None;
                self.confirm_profile_delete = false;
                self.profile_status = Some("Deleted profile".into());
                self.refresh_profiles();
            }
            Err(err) => self.profile_status = Some(err),
        }
    }

    pub(crate) fn export_selected_profile(&mut self) {
        let Some(id) = self.selected_profile_id.clone() else {
            self.profile_status = Some("No profile selected".into());
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("TOML", &["toml"])
            .set_file_name("profile.toml")
            .save_file()
        else {
            return;
        };
        match config_profiles::export_profile(&id, &path) {
            Ok(()) => self.profile_status = Some(format!("Exported profile to {}", path.display())),
            Err(err) => self.profile_status = Some(err),
        }
    }

    fn stop_all_clients(&mut self) {
        let ids: Vec<u64> = self.clients.keys().copied().collect();
        for id in ids {
            self.stop_client(id);
        }
    }

    pub(crate) fn set_connection_state(&mut self, tab_id: u64, state: ConnectionState) {
        let valid = self
            .connection_states
            .get(&tab_id)
            .is_none_or(|current| current.can_transition_to(state));
        if valid {
            if state == ConnectionState::Connected {
                self.reconnect_attempts.remove(&tab_id);
                self.reconnect_deadlines.remove(&tab_id);
                self.manually_disconnected.remove(&tab_id);
            }
            self.connection_states.insert(tab_id, state);
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                let TabState::Client {
                    connection_state,
                    current_error,
                    ..
                } = &mut tab.state;
                *connection_state = state;
                if state == ConnectionState::Disconnected
                    && current_error
                        .as_ref()
                        .is_some_and(|error| error.scope == ErrorScope::Connection)
                {
                    *current_error = None;
                }
            }
        }
    }

    fn maintain_clients(&mut self) {
        let finished: Vec<u64> = self
            .clients
            .iter()
            .filter_map(|(id, handle)| handle.join_handle.is_finished().then_some(*id))
            .collect();
        for id in finished {
            self.clients.remove(&id);
            let Some((enabled, maximum)) = self.tabs.iter().find_map(|tab| {
                (tab.id == id).then(|| match &tab.state {
                    TabState::Client { mqtt_login, .. } => (
                        mqtt_login.automatic_reconnect,
                        std::time::Duration::from_secs(u64::from(
                            mqtt_login.reconnect_max_delay_secs.max(1),
                        )),
                    ),
                })
            }) else {
                continue;
            };
            if self.manually_disconnected.contains(&id) {
                self.set_connection_state(id, ConnectionState::Disconnected);
            } else if enabled {
                self.set_connection_state(id, ConnectionState::Reconnecting);
                let attempt = self.reconnect_attempts.entry(id).or_default();
                let delay = client::reconnect_delay(*attempt, maximum);
                *attempt = attempt.saturating_add(1);
                self.reconnect_deadlines
                    .insert(id, std::time::Instant::now() + delay);
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    let TabState::Client { activity, .. } = &mut tab.state;
                    events::push_activity(
                        activity,
                        std::time::SystemTime::now(),
                        ActivityLevel::Warning,
                        format!("Reconnect scheduled in {:.1}s", delay.as_secs_f32()),
                    );
                }
            } else {
                self.set_connection_state(id, ConnectionState::Failed);
            }
        }

        let now = std::time::Instant::now();
        let due: Vec<u64> = self
            .reconnect_deadlines
            .iter()
            .filter_map(|(id, deadline)| (*deadline <= now).then_some(*id))
            .collect();
        for id in due {
            self.reconnect_deadlines.remove(&id);
            self.set_connection_state(id, ConnectionState::Connecting);
            self.start_client(id);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.save_workspace(true);
        self.stop_all_clients();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.repaint_context = ui.ctx().clone();
        for id in std::mem::take(&mut self.restored_connections_pending) {
            self.set_connection_state(id, ConnectionState::Connecting);
            self.start_client(id);
        }
        self.maintain_clients();
        events::pump_client_events(self);
        crate::ui::render(self, ui);
        self.save_workspace(false);
        if let Some(since) = self.workspace_dirty_since {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(750).saturating_sub(since.elapsed()));
        }
        if let Some(delay) = self
            .reconnect_deadlines
            .values()
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()))
            .min()
        {
            ui.ctx().request_repaint_after(delay);
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.save_workspace(true);
    }

    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(30)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize},
        mpsc,
    };

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::models::client::ClientHandle;

    fn empty_app() -> App {
        let mut app = App::default();
        app.stop_all_clients();
        app.tabs.clear();
        app.clients.clear();
        app.connection_states.clear();
        app.reconnect_attempts.clear();
        app.reconnect_deadlines.clear();
        app.manually_disconnected.clear();
        app
    }

    #[test]
    fn command_without_a_live_client_is_reported() {
        let mut app = empty_app();
        assert_eq!(
            app.send_client_command(99, ClientCommand::Disconnect),
            ClientCommandResult::NoClientHandle
        );
    }

    #[test]
    fn queue_full_is_distinct_from_a_closed_channel() {
        let mut app = empty_app();
        let (command_tx, _command_rx) = tokio::sync::mpsc::channel(1);
        command_tx.try_send(ClientCommand::Disconnect).unwrap();
        let (_event_tx, event_rx) = mpsc::sync_channel(1);
        let join_handle = app.runtime.spawn(std::future::pending());
        app.clients.insert(
            7,
            ClientHandle {
                cancellation: CancellationToken::new(),
                join_handle,
                event_rx,
                command_tx,
                queued_messages: Arc::new(AtomicUsize::new(0)),
                dropped_messages: Arc::new(AtomicU64::new(0)),
            },
        );
        assert_eq!(
            app.send_client_command(7, ClientCommand::Disconnect),
            ClientCommandResult::ChannelFull
        );
    }

    #[test]
    fn manual_disconnect_during_backoff_and_cancel_reconnect_stop_restart() {
        let mut app = empty_app();
        for id in [1, 2] {
            app.connection_states
                .insert(id, ConnectionState::Reconnecting);
            app.reconnect_deadlines
                .insert(id, Instant::now() + Duration::from_secs(30));
        }

        app.disconnect_client(1);
        app.cancel_reconnect(2);

        for id in [1, 2] {
            assert_eq!(
                app.connection_states.get(&id),
                Some(&ConnectionState::Disconnected)
            );
            assert!(!app.reconnect_deadlines.contains_key(&id));
            assert!(app.manually_disconnected.contains(&id));
        }
    }

    #[test]
    fn successful_connection_resets_reconnect_bookkeeping() {
        let mut app = empty_app();
        app.connection_states
            .insert(1, ConnectionState::Reconnecting);
        app.reconnect_attempts.insert(1, 4);
        app.reconnect_deadlines
            .insert(1, Instant::now() + Duration::from_secs(30));
        app.set_connection_state(1, ConnectionState::Connected);
        assert!(!app.reconnect_attempts.contains_key(&1));
        assert!(!app.reconnect_deadlines.contains_key(&1));
    }
}
