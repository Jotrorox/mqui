//! Versioned workspace persistence.
//!
//! Schema changes should remain additive and defaultable within a version. Increment
//! `SCHEMA_VERSION` only for an incompatible change and add an explicit migration.

use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use url::Url;

use super::App;
use super::state::{MessageFilterMode, PayloadView, Tab, TabState};
use crate::models::ipc::ConnectionState;
use crate::models::mqtt::{
    ConnectionInputMode, MqttLoginData, SubscriptionEntry, TlsVerificationMode, TransportKind,
};

pub(crate) const SCHEMA_VERSION: u32 = 1;
#[cfg(test)]
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Workspace {
    version: u32,
    active_tab: Option<u64>,
    tabs: Vec<PersistedTab>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedTab {
    id: u64,
    title: String,
    client: PersistedClient,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct PersistedClient {
    login: PersistedLogin,
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
    subscriptions: Vec<PersistedSubscription>,
}

impl Default for PersistedClient {
    fn default() -> Self {
        Self {
            login: PersistedLogin::default(),
            subscribe_topic: "t1".to_string(),
            subscribe_qos: 0,
            unsubscribe_topic: String::new(),
            publish_topic: "t1".to_string(),
            publish_qos: 0,
            publish_retain: false,
            publish_payload: "hello".to_string(),
            payload_view_hex: false,
            topic_filter: String::new(),
            max_messages: 200,
            subscriptions: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct PersistedLogin {
    name: String,
    broker: String,
    port: String,
    username: String,
    client_id: String,
    keep_alive_secs: u16,
    testament_and_last_will: String,
    testament_topic: String,
    testament_qos: u8,
    testament_retain: bool,
    connection_mode: ConnectionInputMode,
    connection_url: String,
    transport: TransportKind,
    ws_path: String,
    tls_verification: TlsVerificationMode,
    tls_ca_cert_path: String,
    automatic_reconnect: bool,
    reconnect_max_delay_secs: u16,
}

impl Default for PersistedLogin {
    fn default() -> Self {
        let mut login = Self::from(&MqttLoginData::default());
        // Missing this field in legacy state must not opt into network activity.
        login.automatic_reconnect = false;
        login
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedSubscription {
    topic: String,
    qos: u8,
}

pub(crate) struct RestoredWorkspace {
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: Option<u64>,
    pub(crate) next_tab_id: u64,
    pub(crate) reconnect_tabs: Vec<u64>,
}

impl Workspace {
    fn capture(app: &App) -> Self {
        Self {
            version: SCHEMA_VERSION,
            active_tab: app.active_tab,
            tabs: app.tabs.iter().map(PersistedTab::from).collect(),
        }
    }

    pub(crate) fn restore(self) -> RestoredWorkspace {
        let mut used = HashSet::new();
        let mut next = self
            .tabs
            .iter()
            .map(|tab| tab.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut id_map = Vec::with_capacity(self.tabs.len());
        let mut tabs = Vec::with_capacity(self.tabs.len());
        let mut reconnect_tabs = Vec::new();

        for mut tab in self.tabs {
            let old_id = tab.id;
            let id = if used.insert(old_id) {
                old_id
            } else {
                while !used.insert(next) {
                    next = next.saturating_add(1);
                }
                let id = next;
                next = next.saturating_add(1);
                id
            };
            id_map.push((old_id, id));
            let login = MqttLoginData::from(std::mem::take(&mut tab.client.login));
            if login.automatic_reconnect {
                reconnect_tabs.push(id);
            }
            tabs.push(tab.into_tab(id, login));
        }
        let active_tab = self
            .active_tab
            .and_then(|wanted| id_map.iter().find(|(old, _)| *old == wanted).map(|x| x.1))
            .or_else(|| tabs.first().map(|tab| tab.id));
        let next_tab_id = tabs
            .iter()
            .map(|tab| tab.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        RestoredWorkspace {
            tabs,
            active_tab,
            next_tab_id,
            reconnect_tabs,
        }
    }
}

impl From<&Tab> for PersistedTab {
    fn from(tab: &Tab) -> Self {
        let TabState::Client {
            mqtt_login,
            subscribe_topic,
            subscribe_qos,
            unsubscribe_topic,
            publish_topic,
            publish_qos,
            publish_retain,
            publish_payload,
            payload_view,
            topic_filter,
            max_messages,
            subscriptions,
            ..
        } = &tab.state;
        Self {
            id: tab.id,
            title: tab.title.clone(),
            client: PersistedClient {
                login: PersistedLogin::from(mqtt_login),
                subscribe_topic: subscribe_topic.clone(),
                subscribe_qos: *subscribe_qos,
                unsubscribe_topic: unsubscribe_topic.clone(),
                publish_topic: publish_topic.clone(),
                publish_qos: *publish_qos,
                publish_retain: *publish_retain,
                publish_payload: publish_payload.clone(),
                payload_view_hex: *payload_view == PayloadView::Hex,
                topic_filter: topic_filter.clone(),
                max_messages: *max_messages,
                subscriptions: subscriptions
                    .iter()
                    .map(|sub| PersistedSubscription {
                        topic: sub.topic.clone(),
                        qos: sub.qos,
                    })
                    .collect(),
            },
        }
    }
}

impl PersistedTab {
    fn into_tab(self, id: u64, login: MqttLoginData) -> Tab {
        Tab {
            id,
            title: self.title,
            state: TabState::Client {
                mqtt_login: login,
                connection_state: ConnectionState::Disconnected,
                current_error: None,
                activity: VecDeque::new(),
                subscribe_topic: self.client.subscribe_topic,
                subscribe_qos: self.client.subscribe_qos.min(2),
                unsubscribe_topic: self.client.unsubscribe_topic,
                editing_subscription_topic: None,
                editing_subscription_value: String::new(),
                editing_subscription_qos: 0,
                publish_topic: self.client.publish_topic,
                publish_qos: self.client.publish_qos.min(2),
                publish_retain: self.client.publish_retain,
                publish_payload: self.client.publish_payload,
                payload_view: if self.client.payload_view_hex {
                    PayloadView::Hex
                } else {
                    PayloadView::Text
                },
                topic_filter: self.client.topic_filter,
                message_filter_mode: MessageFilterMode::Substring,
                payload_search: String::new(),
                max_messages: self.client.max_messages.clamp(1, 1000),
                capture_paused: false,
                paused_message_count: 0,
                next_message_id: 0,
                selected_message_id: None,
                subscriptions: self
                    .client
                    .subscriptions
                    .into_iter()
                    .map(|sub| SubscriptionEntry {
                        topic: sub.topic,
                        qos: sub.qos.min(2),
                    })
                    .collect(),
                messages: VecDeque::new(),
                received_count: 0,
                dropped_message_count: 0,
                current_client_dropped_message_count: 0,
                published_count: 0,
            },
        }
    }
}

impl From<&MqttLoginData> for PersistedLogin {
    fn from(login: &MqttLoginData) -> Self {
        Self {
            name: login.name.clone(),
            broker: login.broker.clone(),
            port: login.port.clone(),
            username: login.username.clone(),
            client_id: login.client_id.clone(),
            keep_alive_secs: login.keep_alive_secs,
            testament_and_last_will: login.testament_and_last_will.clone(),
            testament_topic: login.testament_topic.clone(),
            testament_qos: login.testament_qos,
            testament_retain: login.testament_retain,
            connection_mode: login.connection_mode,
            connection_url: sanitize_url(&login.connection_url),
            transport: login.transport,
            ws_path: login.ws_path.clone(),
            tls_verification: login.tls_verification,
            tls_ca_cert_path: login.tls_ca_cert_path.clone(),
            automatic_reconnect: login.automatic_reconnect,
            reconnect_max_delay_secs: login.reconnect_max_delay_secs,
        }
    }
}

impl From<PersistedLogin> for MqttLoginData {
    fn from(login: PersistedLogin) -> Self {
        Self {
            name: login.name,
            broker: login.broker,
            port: login.port,
            username: login.username,
            password: String::new(),
            client_id: login.client_id,
            keep_alive_secs: login.keep_alive_secs.max(1),
            testament_and_last_will: login.testament_and_last_will,
            testament_topic: login.testament_topic,
            testament_qos: login.testament_qos.min(2),
            testament_retain: login.testament_retain,
            connection_mode: login.connection_mode,
            connection_url: sanitize_url(&login.connection_url),
            transport: login.transport,
            ws_path: login.ws_path,
            tls_verification: login.tls_verification,
            tls_ca_cert_path: login.tls_ca_cert_path,
            automatic_reconnect: login.automatic_reconnect,
            reconnect_max_delay_secs: login.reconnect_max_delay_secs.max(1),
        }
    }
}

fn sanitize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(mut url) = Url::parse(trimmed) {
        if !url.username().is_empty() || url.password().is_some() {
            let _ = url.set_username("");
            let _ = url.set_password(None);
        }
        return url.to_string();
    }
    // A malformed URL is not connectable; discard an authority containing userinfo
    // rather than risk writing a credential-shaped value to disk.
    if let Some((scheme, rest)) = trimmed.split_once("://")
        && let Some(at) = rest.rfind('@')
    {
        return format!("{scheme}://{}", &rest[at + 1..]);
    }
    trimmed.to_string()
}

pub(crate) fn workspace_path() -> Result<PathBuf, String> {
    ProjectDirs::from("io", "jotrorox", "mqui")
        .map(|dirs| {
            dirs.state_dir()
                .unwrap_or_else(|| dirs.config_dir())
                .join("workspace-v1.toml")
        })
        .ok_or_else(|| "platform application directory could not be determined".to_string())
}

pub(crate) fn serialize(app: &App) -> Result<Vec<u8>, toml::ser::Error> {
    toml::to_string_pretty(&Workspace::capture(app)).map(String::into_bytes)
}

pub(crate) fn load(path: &Path) -> Result<Option<Workspace>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let workspace: Workspace = toml::from_slice(&bytes).map_err(|err| err.to_string())?;
    if workspace.version > SCHEMA_VERSION {
        return Err(format!(
            "workspace schema {} is newer than supported schema {SCHEMA_VERSION}",
            workspace.version
        ));
    }
    Ok(Some(workspace))
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".workspace-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| err.error)?;
    if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::TabKind;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mqui-{label}-{}-{}.toml",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn sample_app() -> App {
        let mut app = App::default();
        app.tabs.clear();
        app.clients.clear();
        app.connection_states.clear();
        app.active_tab = None;
        app.next_tab_id = 7;
        let mut login = MqttLoginData {
            name: "local".into(),
            broker: "broker.example".into(),
            username: "alice".into(),
            password: "do-not-save".into(),
            automatic_reconnect: false,
            ..Default::default()
        };
        login.connection_url = "mqtt://alice:url-secret@broker.example:1883".into();
        app.new_tab_without_connection(7, "Renamed", login);
        let TabState::Client {
            subscribe_topic,
            subscribe_qos,
            publish_topic,
            publish_qos,
            publish_retain,
            publish_payload,
            payload_view,
            topic_filter,
            max_messages,
            subscriptions,
            ..
        } = &mut app.tabs[0].state;
        *subscribe_topic = "sensors/#".into();
        *subscribe_qos = 1;
        *publish_topic = "commands/start".into();
        *publish_qos = 2;
        *publish_retain = true;
        *publish_payload = "{\"go\":true}".into();
        *payload_view = PayloadView::Hex;
        *topic_filter = "sensors".into();
        *max_messages = 321;
        subscriptions.push(SubscriptionEntry {
            topic: "sensors/+".into(),
            qos: 2,
        });
        app
    }

    #[test]
    fn round_trip_restores_workspace_without_messages() {
        let app = sample_app();
        let encoded = serialize(&app).unwrap();
        let decoded: Workspace = toml::from_slice(&encoded).unwrap();
        let restored = decoded.restore();
        assert_eq!(restored.tabs.len(), 1);
        assert_eq!(restored.tabs[0].id, 7);
        assert_eq!(restored.tabs[0].title, "Renamed");
        assert_eq!(restored.active_tab, Some(7));
        assert_eq!(restored.next_tab_id, 8);
        let TabState::Client {
            publish_payload,
            max_messages,
            subscriptions,
            messages,
            ..
        } = &restored.tabs[0].state;
        assert_eq!(publish_payload, "{\"go\":true}");
        assert_eq!(*max_messages, 321);
        assert_eq!(subscriptions[0].qos, 2);
        assert!(messages.is_empty());
    }

    #[test]
    fn passwords_and_url_userinfo_are_omitted() {
        let encoded = String::from_utf8(serialize(&sample_app()).unwrap()).unwrap();
        assert!(!encoded.contains("do-not-save"));
        assert!(!encoded.contains("url-secret"));
        assert!(!encoded.contains("alice@"));
        assert!(!encoded.contains("password"));
        assert!(encoded.contains("mqtt://broker.example:1883"));
    }

    #[test]
    fn corrupted_file_returns_an_error() {
        let path = temp_path("corrupt");
        fs::write(&path, b"tabs = [ definitely not toml").unwrap();
        assert!(load(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn older_schema_and_missing_fields_use_defaults() {
        let workspace: Workspace = toml::from_str(
            r#"
version = 0
[[tabs]]
id = 4
title = "old"
"#,
        )
        .unwrap();
        let restored = workspace.restore();
        assert_eq!(restored.tabs[0].id, 4);
        let TabState::Client { max_messages, .. } = restored.tabs[0].state;
        assert_eq!(max_messages, 200);
    }

    #[test]
    fn duplicate_ids_are_reassigned_and_next_id_advances() {
        let workspace: Workspace = toml::from_str(
            r#"
version = 1
active_tab = 3
[[tabs]]
id = 3
[[tabs]]
id = 3
[[tabs]]
id = 9
"#,
        )
        .unwrap();
        let restored = workspace.restore();
        let ids: HashSet<_> = restored.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(restored.next_tab_id > *ids.iter().max().unwrap());
        assert_eq!(restored.active_tab, Some(3));
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let path = temp_path("atomic");
        fs::write(&path, b"old").unwrap();
        atomic_write(&path, b"new workspace").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new workspace");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".workspace-")
            })
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_file(path).unwrap();
    }

    impl App {
        fn new_tab_without_connection(&mut self, id: u64, title: &str, login: MqttLoginData) {
            self.next_tab_id = id;
            self.new_tab(TabKind::Client, login);
            self.stop_client(id);
            self.tabs.last_mut().unwrap().title = title.into();
            self.set_connection_state(id, ConnectionState::Disconnected);
        }
    }
}
