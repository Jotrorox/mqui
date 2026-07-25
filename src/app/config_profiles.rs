use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::models::mqtt::{ConnectionInputMode, MqttLoginData, TlsVerificationMode, TransportKind};

#[derive(Clone, Debug)]
pub(crate) struct ProfileEntry {
    pub(crate) display_name: String,
    pub(crate) file_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoginTemplateFile {
    #[serde(default)]
    profile_name: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    broker: String,
    #[serde(default)]
    port: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    keep_alive_secs: u16,
    #[serde(default)]
    testament_and_last_will: String,
    #[serde(default)]
    testament_topic: String,
    #[serde(default)]
    testament_qos: u8,
    #[serde(default)]
    testament_retain: bool,
    #[serde(default)]
    connection_mode: ConnectionInputMode,
    #[serde(default)]
    connection_url: String,
    #[serde(default)]
    transport: TransportKind,
    #[serde(default = "default_ws_path")]
    ws_path: String,
    #[serde(default)]
    tls_verification: TlsVerificationMode,
    #[serde(default)]
    tls_ca_cert_path: String,
    #[serde(default = "default_true")]
    automatic_reconnect: bool,
    #[serde(default = "default_reconnect_max_delay")]
    reconnect_max_delay_secs: u16,
}

impl LoginTemplateFile {
    fn from_login(profile_name: Option<String>, login: &MqttLoginData) -> Self {
        Self {
            profile_name,
            name: login.name.clone(),
            broker: login.broker.clone(),
            port: login.port.clone(),
            username: login.username.clone(),
            client_id: login.client_id.clone(),
            keep_alive_secs: login.effective_keep_alive_secs(),
            testament_and_last_will: login.testament_and_last_will.clone(),
            testament_topic: login.testament_topic.clone(),
            testament_qos: login.testament_qos,
            testament_retain: login.testament_retain,
            connection_mode: login.connection_mode,
            connection_url: login.connection_url.clone(),
            transport: login.transport,
            ws_path: login.ws_path.clone(),
            tls_verification: login.tls_verification,
            tls_ca_cert_path: login.tls_ca_cert_path.clone(),
            automatic_reconnect: login.automatic_reconnect,
            reconnect_max_delay_secs: login.reconnect_max_delay_secs,
        }
    }

    fn into_login(self) -> MqttLoginData {
        MqttLoginData {
            name: self.name,
            broker: self.broker,
            port: self.port,
            username: self.username,
            password: String::new(),
            client_id: self.client_id,
            keep_alive_secs: self.keep_alive_secs.max(1),
            testament_and_last_will: self.testament_and_last_will,
            testament_topic: self.testament_topic,
            testament_qos: self.testament_qos,
            testament_retain: self.testament_retain,
            connection_mode: self.connection_mode,
            connection_url: self.connection_url,
            transport: self.transport,
            ws_path: self.ws_path,
            tls_verification: self.tls_verification,
            tls_ca_cert_path: self.tls_ca_cert_path,
            automatic_reconnect: self.automatic_reconnect,
            reconnect_max_delay_secs: self.reconnect_max_delay_secs.max(1),
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn default_reconnect_max_delay() -> u16 {
    30
}

pub(crate) fn list_profiles() -> Result<Vec<ProfileEntry>, String> {
    let dir = profiles_dir()?;
    let mut entries = Vec::new();

    let dir_entries = fs::read_dir(&dir)
        .map_err(|err| format!("Failed to read profile directory {}: {err}", dir.display()))?;

    for dir_entry in dir_entries {
        let dir_entry = match dir_entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let path = dir_entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }

        let fallback = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("profile")
            .to_string();

        let display_name = match fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<LoginTemplateFile>(&text).ok())
            .and_then(|template| template.profile_name)
        {
            Some(name) if !name.trim().is_empty() => name,
            _ => fallback,
        };

        entries.push(ProfileEntry {
            display_name,
            file_path: path,
        });
    }

    entries.sort_by_key(|entry| entry.display_name.to_lowercase());
    Ok(entries)
}

pub(crate) fn save_profile(profile_name: &str, login: &MqttLoginData) -> Result<(), String> {
    let trimmed = profile_name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }

    let file_name = format!("{}.toml", safe_file_name(trimmed));
    let path = profiles_dir()?.join(file_name);

    let template = LoginTemplateFile::from_login(Some(trimmed.to_string()), login);
    let serialized = toml::to_string_pretty(&template)
        .map_err(|err| format!("Failed to serialize profile '{trimmed}': {err}"))?;

    fs::write(&path, serialized)
        .map_err(|err| format!("Failed to write profile {}: {err}", path.display()))
}

pub(crate) fn load_profile_file(path: &Path) -> Result<MqttLoginData, String> {
    let contents = fs::read_to_string(path)
        .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
    let template: LoginTemplateFile = toml::from_str(&contents)
        .map_err(|err| format!("Failed to parse TOML {}: {err}", path.display()))?;
    Ok(template.into_login())
}

pub(crate) fn load_template_file(path: &Path) -> Result<MqttLoginData, String> {
    load_profile_file(path)
}

fn profiles_dir() -> Result<PathBuf, String> {
    let project_dirs = ProjectDirs::from("io", "jotrorox", "mqui")
        .ok_or_else(|| "Could not resolve operating system config directory".to_string())?;

    let dir = project_dirs.config_dir().join("profiles");
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "Failed to create profile directory {}: {err}",
            dir.display()
        )
    })?;
    Ok(dir)
}

fn safe_file_name(value: &str) -> String {
    let mut output = String::new();

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            output.push(ch);
        } else {
            output.push('_');
        }
    }

    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "profile".to_string()
    } else {
        trimmed.to_string()
    }
}

fn default_ws_path() -> String {
    "/mqtt".to_string()
}

#[cfg(test)]
mod tests {
    use super::LoginTemplateFile;
    use crate::models::mqtt::{ConnectionInputMode, TlsVerificationMode, TransportKind};

    #[test]
    fn old_profiles_load_with_transport_defaults() {
        let template = toml::from_str::<LoginTemplateFile>(
            r#"
name = "Legacy"
broker = "broker.example.com"
port = "1883"
keep_alive_secs = 30
"#,
        )
        .unwrap();

        let login = template.into_login();
        assert_eq!(login.connection_mode, ConnectionInputMode::Structured);
        assert_eq!(login.transport, TransportKind::Tcp);
        assert_eq!(login.ws_path, "/mqtt");
        assert_eq!(login.tls_verification, TlsVerificationMode::SystemRoots);
        assert!(login.tls_ca_cert_path.is_empty());
    }

    #[test]
    fn profiles_round_trip_transport_fields() {
        let template = LoginTemplateFile {
            profile_name: Some("secure".to_string()),
            name: "Secure Broker".to_string(),
            broker: "broker.example.com".to_string(),
            port: "443".to_string(),
            username: "alice".to_string(),
            client_id: "client-1".to_string(),
            keep_alive_secs: 45,
            testament_and_last_will: "bye".to_string(),
            testament_topic: "last/will".to_string(),
            testament_qos: 1,
            testament_retain: true,
            connection_mode: ConnectionInputMode::Url,
            connection_url: "wss://broker.example.com/mqtt".to_string(),
            transport: TransportKind::Wss,
            ws_path: "/mqtt".to_string(),
            tls_verification: TlsVerificationMode::CustomCa,
            tls_ca_cert_path: "/tmp/ca.pem".to_string(),
            automatic_reconnect: true,
            reconnect_max_delay_secs: 30,
        };

        let serialized = toml::to_string_pretty(&template).unwrap();
        let round_tripped = toml::from_str::<LoginTemplateFile>(&serialized).unwrap();

        assert_eq!(round_tripped.connection_mode, ConnectionInputMode::Url);
        assert_eq!(
            round_tripped.connection_url,
            "wss://broker.example.com/mqtt"
        );
        assert_eq!(round_tripped.transport, TransportKind::Wss);
        assert_eq!(round_tripped.ws_path, "/mqtt");
        assert_eq!(
            round_tripped.tls_verification,
            TlsVerificationMode::CustomCa
        );
        assert_eq!(round_tripped.tls_ca_cert_path, "/tmp/ca.pem");
    }

    #[test]
    fn profile_toml_uses_readable_transport_strings() {
        let template = LoginTemplateFile {
            profile_name: Some("dev".to_string()),
            name: "Dev Broker".to_string(),
            broker: "localhost".to_string(),
            port: "443".to_string(),
            username: String::new(),
            client_id: String::new(),
            keep_alive_secs: 60,
            testament_and_last_will: String::new(),
            testament_topic: String::new(),
            testament_qos: 0,
            testament_retain: false,
            connection_mode: ConnectionInputMode::Url,
            connection_url: "wss://localhost/mqtt".to_string(),
            transport: TransportKind::Wss,
            ws_path: "/mqtt".to_string(),
            tls_verification: TlsVerificationMode::InsecureSkipVerify,
            tls_ca_cert_path: String::new(),
            automatic_reconnect: true,
            reconnect_max_delay_secs: 30,
        };

        let serialized = toml::to_string_pretty(&template).unwrap();
        assert!(serialized.contains("connection_mode = \"url\""));
        assert!(serialized.contains("transport = \"wss\""));
        assert!(serialized.contains("tls_verification = \"insecure-skip-verify\""));
    }
}
