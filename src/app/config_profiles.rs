use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::models::mqtt::{ConnectionInputMode, MqttLoginData, TlsVerificationMode, TransportKind};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub(crate) struct ProfileEntry {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) file_path: PathBuf,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoginTemplateFile {
    #[serde(default)]
    profile_id: Option<String>,
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
    fn from_login(
        profile_id: Option<String>,
        profile_name: Option<String>,
        login: &MqttLoginData,
    ) -> Self {
        Self {
            profile_id,
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

pub(crate) fn list_profiles() -> Result<Vec<ProfileEntry>, String> {
    list_profiles_in(&profiles_dir()?)
}

fn list_profiles_in(dir: &Path) -> Result<Vec<ProfileEntry>, String> {
    fs::create_dir_all(dir).map_err(|err| {
        format!(
            "Failed to create profile directory {}: {err}",
            dir.display()
        )
    })?;
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)
        .map_err(|err| format!("Failed to read profile directory {}: {err}", dir.display()))?
    {
        let item = match item {
            Ok(item) => item,
            Err(err) => {
                entries.push(ProfileEntry {
                    id: String::new(),
                    display_name: "Unreadable directory entry".into(),
                    file_path: dir.to_path_buf(),
                    warning: Some(err.to_string()),
                });
                continue;
            }
        };
        let path = item.path();
        if path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }
        let fallback = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("profile")
            .to_string();
        let (id, display_name, warning) = match fs::read_to_string(&path) {
            Err(err) => (
                fallback.clone(),
                fallback.clone(),
                Some(format!("Cannot read file: {err}")),
            ),
            Ok(text) => match toml::from_str::<LoginTemplateFile>(&text) {
                Err(err) => (
                    fallback.clone(),
                    fallback.clone(),
                    Some(format!("Malformed TOML: {err}")),
                ),
                Ok(template) => {
                    let name = template
                        .profile_name
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| fallback.clone());
                    (fallback.clone(), name, None)
                }
            },
        };
        entries.push(ProfileEntry {
            id,
            display_name,
            file_path: path,
            warning,
        });
    }

    let mut name_counts = HashMap::new();
    for entry in entries.iter().filter(|entry| entry.warning.is_none()) {
        *name_counts
            .entry(normalized_name(&entry.display_name))
            .or_insert(0usize) += 1;
    }
    for entry in &mut entries {
        if name_counts
            .get(&normalized_name(&entry.display_name))
            .copied()
            .unwrap_or(0)
            > 1
        {
            entry.warning =
                Some("Duplicate profile display name; rename one entry to remove ambiguity".into());
        }
    }
    entries.sort_by_key(|entry| entry.display_name.to_lowercase());
    Ok(entries)
}

pub(crate) fn create_profile(profile_name: &str, login: &MqttLoginData) -> Result<String, String> {
    create_profile_in(&profiles_dir()?, profile_name, login)
}

fn create_profile_in(
    dir: &Path,
    profile_name: &str,
    login: &MqttLoginData,
) -> Result<String, String> {
    let name = checked_name(profile_name)?;
    ensure_unique_name(dir, name, None)?;
    fs::create_dir_all(dir).map_err(|err| {
        format!(
            "Failed to create profile directory {}: {err}",
            dir.display()
        )
    })?;
    for _ in 0..100 {
        let id = new_id();
        let path = path_for_id(dir, &id)?;
        if path.exists() {
            continue;
        }
        let template =
            LoginTemplateFile::from_login(Some(id.clone()), Some(name.to_string()), login);
        atomic_write(&path, &serialize(&template, name)?, false)?;
        return Ok(id);
    }
    Err("Could not allocate a unique profile identity".into())
}

pub(crate) fn overwrite_profile(
    id: &str,
    profile_name: &str,
    login: &MqttLoginData,
) -> Result<(), String> {
    overwrite_profile_in(&profiles_dir()?, id, profile_name, login)
}

fn overwrite_profile_in(
    dir: &Path,
    id: &str,
    profile_name: &str,
    login: &MqttLoginData,
) -> Result<(), String> {
    let name = checked_name(profile_name)?;
    let path = existing_path_for_id(dir, id)?;
    ensure_unique_name(dir, name, Some(id))?;
    let template =
        LoginTemplateFile::from_login(Some(id.to_string()), Some(name.to_string()), login);
    atomic_write(&path, &serialize(&template, name)?, true)
}

pub(crate) fn rename_profile(id: &str, new_name: &str) -> Result<(), String> {
    rename_profile_in(&profiles_dir()?, id, new_name)
}

fn rename_profile_in(dir: &Path, id: &str, new_name: &str) -> Result<(), String> {
    let name = checked_name(new_name)?;
    ensure_unique_name(dir, name, Some(id))?;
    let path = existing_path_for_id(dir, id)?;
    let text = fs::read_to_string(&path)
        .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
    let mut template: LoginTemplateFile = toml::from_str(&text)
        .map_err(|err| format!("Failed to parse TOML {}: {err}", path.display()))?;
    template.profile_id = Some(id.to_string());
    template.profile_name = Some(name.to_string());
    atomic_write(&path, &serialize(&template, name)?, true)
}

pub(crate) fn delete_profile(id: &str) -> Result<(), String> {
    delete_profile_in(&profiles_dir()?, id)
}

fn delete_profile_in(dir: &Path, id: &str) -> Result<(), String> {
    let path = existing_path_for_id(dir, id)?;
    fs::remove_file(&path).map_err(|err| format!("Failed to delete {}: {err}", path.display()))
}

pub(crate) fn export_profile(id: &str, destination: &Path) -> Result<(), String> {
    let source = existing_path_for_id(&profiles_dir()?, id)?;
    export_profile_file(&source, destination)
}

fn export_profile_file(source: &Path, destination: &Path) -> Result<(), String> {
    let contents =
        fs::read(source).map_err(|err| format!("Failed to read {}: {err}", source.display()))?;
    atomic_write(destination, &contents, true)
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
    Ok(project_dirs.config_dir().join("profiles"))
}

fn checked_name(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if value.is_empty() {
        Err("Profile name cannot be empty".into())
    } else {
        Ok(value)
    }
}

fn normalized_name(value: &str) -> String {
    value.trim().to_lowercase()
}

fn ensure_unique_name(dir: &Path, name: &str, except_id: Option<&str>) -> Result<(), String> {
    if list_profiles_in(dir)?.iter().any(|entry| {
        entry.warning.is_none()
            && Some(entry.id.as_str()) != except_id
            && normalized_name(&entry.display_name) == normalized_name(name)
    }) {
        return Err(format!("A profile named '{name}' already exists"));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn path_for_id(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("Invalid profile identity".into());
    }
    Ok(dir.join(format!("{id}.toml")))
}

fn existing_path_for_id(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let entries = list_profiles_in(dir)?;
    entries
        .into_iter()
        .find(|entry| entry.id == id && entry.file_path.parent() == Some(dir))
        .map(|entry| entry.file_path)
        .ok_or_else(|| format!("Profile identity '{id}' was not found"))
}

fn serialize(template: &LoginTemplateFile, name: &str) -> Result<Vec<u8>, String> {
    toml::to_string_pretty(template)
        .map(String::into_bytes)
        .map_err(|err| format!("Failed to serialize profile '{name}': {err}"))
}

fn new_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("profile-{time:032x}-{sequence:016x}")
}

fn atomic_write(path: &Path, contents: &[u8], replace: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Destination {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("Failed to create directory {}: {err}", parent.display()))?;
    let mut temp = None;
    for _ in 0..100 {
        let candidate = parent.join(format!(".mqui-{}.tmp", new_id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                return Err(format!(
                    "Failed to create temporary file in {}: {err}",
                    parent.display()
                ));
            }
        }
    }
    let (temp_path, mut file) =
        temp.ok_or_else(|| format!("Could not create temporary file in {}", parent.display()))?;
    let result = (|| {
        file.write_all(contents).map_err(|err| {
            format!(
                "Failed to write temporary profile {}: {err}",
                temp_path.display()
            )
        })?;
        file.flush().map_err(|err| {
            format!(
                "Failed to flush temporary profile {}: {err}",
                temp_path.display()
            )
        })?;
        file.sync_all().map_err(|err| {
            format!(
                "Failed to sync temporary profile {}: {err}",
                temp_path.display()
            )
        })?;
        drop(file);
        if !replace && path.exists() {
            return Err(format!(
                "Refusing to overwrite existing file {}",
                path.display()
            ));
        }
        fs::rename(&temp_path, path)
            .map_err(|err| format!("Failed to install profile {}: {err}", path.display()))?;
        sync_directory(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(unix)]
fn sync_directory(path: &Path) {
    let _ = File::open(path).and_then(|dir| dir.sync_all());
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) {}

const fn default_true() -> bool {
    true
}
const fn default_reconnect_max_delay() -> u16 {
    30
}
fn default_ws_path() -> String {
    "/mqtt".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mqui-test-{name}-{}", new_id()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn login(name: &str) -> MqttLoginData {
        MqttLoginData {
            name: name.into(),
            broker: "localhost".into(),
            port: "1883".into(),
            password: "secret".into(),
            ..MqttLoginData::default()
        }
    }

    #[test]
    fn colliding_sanitized_names_get_distinct_ids() {
        let dir = temp_dir("collision");
        let first = create_profile_in(&dir, "Production EU", &login("Production EU")).unwrap();
        let second = create_profile_in(&dir, "Production_EU", &login("Production_EU")).unwrap();
        assert_ne!(first, second);
        assert_eq!(list_profiles_in(&dir).unwrap().len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn create_refuses_duplicate_and_overwrite_is_explicit() {
        let dir = temp_dir("overwrite");
        let id = create_profile_in(&dir, "Dev", &login("one")).unwrap();
        assert!(create_profile_in(&dir, "dev", &login("two")).is_err());
        overwrite_profile_in(&dir, &id, "Dev", &login("two")).unwrap();
        let entry = list_profiles_in(&dir).unwrap().pop().unwrap();
        assert_eq!(load_profile_file(&entry.file_path).unwrap().name, "two");
        assert!(
            !fs::read_to_string(entry.file_path)
                .unwrap()
                .contains("secret")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rename_delete_and_export_work() {
        let dir = temp_dir("actions");
        let id = create_profile_in(&dir, "Old", &login("Old")).unwrap();
        rename_profile_in(&dir, &id, "New").unwrap();
        assert_eq!(list_profiles_in(&dir).unwrap()[0].display_name, "New");
        let export = dir.join("copy.txt");
        export_profile_file(&existing_path_for_id(&dir, &id).unwrap(), &export).unwrap();
        assert_eq!(load_profile_file(&export).unwrap().name, "Old");
        delete_profile_in(&dir, &id).unwrap();
        assert!(list_profiles_in(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_and_duplicate_legacy_files_are_visible() {
        let dir = temp_dir("malformed");
        fs::write(dir.join("broken.toml"), "not = [toml").unwrap();
        fs::write(
            dir.join("one.toml"),
            "profile_name = \"Same\"\nbroker = \"one\"",
        )
        .unwrap();
        fs::write(
            dir.join("two.toml"),
            "profile_name = \"same\"\nbroker = \"two\"",
        )
        .unwrap();
        let entries = list_profiles_in(&dir).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|entry| entry.warning.is_some()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_profile_loads_with_defaults() {
        let dir = temp_dir("legacy");
        let path = dir.join("Production EU.toml");
        fs::write(&path, "name = \"Legacy\"\nbroker = \"broker.example.com\"\nport = \"1883\"\nkeep_alive_secs = 30").unwrap();
        let entries = list_profiles_in(&dir).unwrap();
        assert_eq!(entries[0].id, "Production EU");
        let loaded = load_profile_file(&path).unwrap();
        assert_eq!(loaded.transport, TransportKind::Tcp);
        assert_eq!(loaded.ws_path, "/mqtt");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_write_cleans_temporary_files_and_uses_restrictive_permissions() {
        let dir = temp_dir("atomic");
        let path = dir.join("profile.toml");
        atomic_write(&path, b"first", false).unwrap();
        assert!(atomic_write(&path, b"second", false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert!(!fs::read_dir(&dir).unwrap().any(|item| {
            item.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identity_cannot_escape_directory() {
        let dir = temp_dir("escape");
        assert!(path_for_id(&dir, "../outside").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
