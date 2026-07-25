use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use url::{Host, Url};

pub(crate) const MAX_STORED_MESSAGES: usize = 1000;

const DEFAULT_BROKER_HOST: &str = "127.0.0.1";
const DEFAULT_WS_PATH: &str = "/mqtt";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ConnectionInputMode {
    #[default]
    Structured,
    Url,
}

impl ConnectionInputMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Structured => "Structured",
            Self::Url => "URL",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TransportKind {
    #[default]
    Tcp,
    Tls,
    Ws,
    Wss,
}

impl TransportKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Tls => "TLS",
            Self::Ws => "WebSocket",
            Self::Wss => "Secure WebSocket",
        }
    }

    pub(crate) fn scheme(self) -> &'static str {
        match self {
            Self::Tcp => "mqtt",
            Self::Tls => "mqtts",
            Self::Ws => "ws",
            Self::Wss => "wss",
        }
    }

    pub(crate) fn default_port(self) -> u16 {
        match self {
            Self::Tcp => 1883,
            Self::Tls => 8883,
            Self::Ws => 80,
            Self::Wss => 443,
        }
    }

    pub(crate) fn uses_tls(self) -> bool {
        matches!(self, Self::Tls | Self::Wss)
    }

    pub(crate) fn uses_websocket(self) -> bool {
        matches!(self, Self::Ws | Self::Wss)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TlsVerificationMode {
    #[default]
    SystemRoots,
    CustomCa,
    InsecureSkipVerify,
}

impl TlsVerificationMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SystemRoots => "System roots",
            Self::CustomCa => "Custom CA file",
            Self::InsecureSkipVerify => "Insecure (skip verification)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedConnection {
    pub(crate) transport: TransportKind,
    pub(crate) addr: String,
    pub(crate) tls_domain: Option<String>,
    pub(crate) ws_path: Option<String>,
    pub(crate) display_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MqttLoginData {
    pub(crate) name: String,
    pub(crate) broker: String,
    pub(crate) port: String,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) client_id: String,
    pub(crate) keep_alive_secs: u16,
    pub(crate) testament_and_last_will: String,
    pub(crate) testament_topic: String,
    pub(crate) testament_qos: u8,
    pub(crate) testament_retain: bool,
    pub(crate) connection_mode: ConnectionInputMode,
    pub(crate) connection_url: String,
    pub(crate) transport: TransportKind,
    pub(crate) ws_path: String,
    pub(crate) tls_verification: TlsVerificationMode,
    pub(crate) tls_ca_cert_path: String,
    pub(crate) automatic_reconnect: bool,
    pub(crate) reconnect_max_delay_secs: u16,
}

impl Default for MqttLoginData {
    fn default() -> Self {
        Self {
            name: String::new(),
            broker: String::new(),
            port: String::new(),
            username: String::new(),
            password: String::new(),
            client_id: String::new(),
            keep_alive_secs: 60,
            testament_and_last_will: String::new(),
            testament_topic: String::new(),
            testament_qos: 0,
            testament_retain: false,
            connection_mode: ConnectionInputMode::Structured,
            connection_url: String::new(),
            transport: TransportKind::Tcp,
            ws_path: DEFAULT_WS_PATH.to_string(),
            tls_verification: TlsVerificationMode::SystemRoots,
            tls_ca_cert_path: String::new(),
            automatic_reconnect: true,
            reconnect_max_delay_secs: 30,
        }
    }
}

impl MqttLoginData {
    pub(crate) fn broker_addr(&self) -> String {
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

    pub(crate) fn username_opt(&self) -> Option<&str> {
        let value = self.username.trim();
        if value.is_empty() { None } else { Some(value) }
    }

    pub(crate) fn password_opt(&self) -> Option<&str> {
        let value = self.password.trim();
        if value.is_empty() { None } else { Some(value) }
    }

    pub(crate) fn testament_and_last_will_opt(&self) -> Option<&str> {
        let value = self.testament_and_last_will.trim();
        if value.is_empty() { None } else { Some(value) }
    }

    pub(crate) fn testament_topic_opt(&self) -> Option<&str> {
        let value = self.testament_topic.trim();
        if value.is_empty() { None } else { Some(value) }
    }

    pub(crate) fn effective_client_id(&self, tab_id: u64) -> String {
        let value = self.client_id.trim();
        if value.is_empty() {
            format!("mqui-client-{}-{tab_id}", std::process::id())
        } else {
            value.to_string()
        }
    }

    pub(crate) fn effective_keep_alive_secs(&self) -> u16 {
        self.keep_alive_secs.max(1)
    }

    pub(crate) fn display_connection_label(&self) -> String {
        self.resolve_connection()
            .map(|resolved| resolved.display_label)
            .unwrap_or_else(|_| match self.connection_mode {
                ConnectionInputMode::Url => self.connection_url.trim().to_string(),
                ConnectionInputMode::Structured => self.broker_addr(),
            })
    }

    pub(crate) fn resolve_connection(&self) -> Result<ResolvedConnection, String> {
        match self.connection_mode {
            ConnectionInputMode::Structured => self.resolve_structured_connection(),
            ConnectionInputMode::Url => self.resolve_url_connection(),
        }
    }

    fn resolve_structured_connection(&self) -> Result<ResolvedConnection, String> {
        let transport = self.transport;
        let (host, port) =
            resolve_structured_host_and_port(self.broker.trim(), self.port.trim(), transport)?;
        let addr = format_addr(&host, port);
        let ws_path = transport
            .uses_websocket()
            .then(|| normalize_ws_path(self.ws_path.trim()));
        let display_label = build_display_label(transport, &host, port, ws_path.as_deref());

        Ok(ResolvedConnection {
            transport,
            addr,
            tls_domain: transport.uses_tls().then_some(host),
            ws_path,
            display_label,
        })
    }

    fn resolve_url_connection(&self) -> Result<ResolvedConnection, String> {
        let raw = self.connection_url.trim();
        if raw.is_empty() {
            return Err("Connection URL is required".to_string());
        }

        let url = Url::parse(raw).map_err(|err| format!("Invalid connection URL: {err}"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err("Connection URL must not include username or password".to_string());
        }
        if url.query().is_some() {
            return Err("Connection URL must not include a query string".to_string());
        }
        if url.fragment().is_some() {
            return Err("Connection URL must not include a fragment".to_string());
        }

        let transport = match url.scheme() {
            "mqtt" => TransportKind::Tcp,
            "mqtts" => TransportKind::Tls,
            "ws" => TransportKind::Ws,
            "wss" => TransportKind::Wss,
            other => return Err(format!("Unsupported connection URL scheme '{other}'")),
        };

        let host = url
            .host()
            .ok_or_else(|| "Connection URL must include a host".to_string())
            .map(host_to_string)?;
        let port = url.port().unwrap_or_else(|| transport.default_port());
        let ws_path = transport
            .uses_websocket()
            .then(|| normalize_url_path(url.path()));
        let display_label = build_display_label(transport, &host, port, ws_path.as_deref());

        Ok(ResolvedConnection {
            transport,
            addr: format_addr(&host, port),
            tls_domain: transport.uses_tls().then_some(host),
            ws_path,
            display_label,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SubscriptionEntry {
    pub(crate) topic: String,
    pub(crate) qos: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct ReceivedMessage {
    pub(crate) timestamp: SystemTime,
    pub(crate) topic: String,
    pub(crate) qos: u8,
    pub(crate) retain: bool,
    pub(crate) payload: Vec<u8>,
}

fn resolve_structured_host_and_port(
    broker: &str,
    port: &str,
    transport: TransportKind,
) -> Result<(String, u16), String> {
    let default_port = transport.default_port();

    if broker.is_empty() {
        let parsed_port = parse_port(port, default_port)?;
        return Ok((DEFAULT_BROKER_HOST.to_string(), parsed_port));
    }

    if !port.is_empty() {
        let host = normalize_host(broker)?;
        let parsed_port = parse_port(port, default_port)?;
        return Ok((host, parsed_port));
    }

    if let Some((host, parsed_port)) = parse_host_port_pair(broker)? {
        return Ok((host, parsed_port));
    }

    Ok((normalize_host(broker)?, default_port))
}

fn parse_port(raw: &str, default_port: u16) -> Result<u16, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(default_port);
    }

    value
        .parse::<u16>()
        .map_err(|_| format!("Invalid port '{value}'"))
}

fn parse_host_port_pair(input: &str) -> Result<Option<(String, u16)>, String> {
    let candidate = format!("mqtt://{input}");
    let Ok(url) = Url::parse(&candidate) else {
        return Ok(None);
    };

    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Broker field must contain only a host or host:port".to_string());
    }

    let Some(port) = url.port() else {
        return Ok(None);
    };
    let host = url
        .host()
        .ok_or_else(|| "Broker field must include a host".to_string())
        .map(host_to_string)?;

    Ok(Some((host, port)))
}

fn normalize_host(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    let host = trim_brackets(trimmed);

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(ip.to_string());
    }

    Host::parse(host)
        .map(host_to_string)
        .map_err(|err| format!("Invalid broker host '{trimmed}': {err}"))
}

fn trim_brackets(value: &str) -> &str {
    if value.starts_with('[') && value.ends_with(']') && value.len() > 2 {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn host_to_string<S>(host: Host<S>) -> String
where
    S: AsRef<str>,
{
    match host {
        Host::Domain(domain) => domain.as_ref().to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    }
}

fn format_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn normalize_ws_path(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        DEFAULT_WS_PATH.to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn normalize_url_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn build_display_label(
    transport: TransportKind,
    host: &str,
    port: u16,
    ws_path: Option<&str>,
) -> String {
    let authority = format_addr(host, port);
    match ws_path {
        Some(path) => format!("{}://{authority}{path}", transport.scheme()),
        None => format!("{}://{authority}", transport.scheme()),
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionInputMode, MqttLoginData, TlsVerificationMode, TransportKind};

    fn default_login() -> MqttLoginData {
        MqttLoginData::default()
    }

    #[test]
    fn structured_transport_defaults_ports_and_paths() {
        let cases = [
            (TransportKind::Tcp, "127.0.0.1:1883", None),
            (TransportKind::Tls, "127.0.0.1:8883", None),
            (TransportKind::Ws, "127.0.0.1:80", Some("/mqtt")),
            (TransportKind::Wss, "127.0.0.1:443", Some("/mqtt")),
        ];

        for (transport, addr, path) in cases {
            let mut login = default_login();
            login.transport = transport;
            let resolved = login.resolve_connection().unwrap();
            assert_eq!(resolved.addr, addr);
            assert_eq!(resolved.ws_path.as_deref(), path);
        }
    }

    #[test]
    fn structured_uses_broker_port_from_field() {
        let mut login = default_login();
        login.transport = TransportKind::Tls;
        login.broker = "broker.example.com".to_string();
        login.port = "9999".to_string();

        let resolved = login.resolve_connection().unwrap();
        assert_eq!(resolved.addr, "broker.example.com:9999");
        assert_eq!(resolved.tls_domain.as_deref(), Some("broker.example.com"));
    }

    #[test]
    fn structured_supports_host_port_in_broker_field() {
        let mut login = default_login();
        login.transport = TransportKind::Tcp;
        login.broker = "broker.example.com:2883".to_string();

        let resolved = login.resolve_connection().unwrap();
        assert_eq!(resolved.addr, "broker.example.com:2883");
    }

    #[test]
    fn url_parsing_supports_all_transport_schemes() {
        let cases = [
            ("mqtt://host", TransportKind::Tcp, "host:1883", None),
            ("mqtts://host", TransportKind::Tls, "host:8883", None),
            (
                "ws://host:8080/mqtt",
                TransportKind::Ws,
                "host:8080",
                Some("/mqtt"),
            ),
            ("wss://host/ws", TransportKind::Wss, "host:443", Some("/ws")),
        ];

        for (url, transport, addr, path) in cases {
            let mut login = default_login();
            login.connection_mode = ConnectionInputMode::Url;
            login.connection_url = url.to_string();

            let resolved = login.resolve_connection().unwrap();
            assert_eq!(resolved.transport, transport);
            assert_eq!(resolved.addr, addr);
            assert_eq!(resolved.ws_path.as_deref(), path);
        }
    }

    #[test]
    fn url_parsing_rejects_invalid_forms() {
        let cases = [
            ("http://host", "Unsupported connection URL scheme 'http'"),
            ("mqtt://", "Connection URL must include a host"),
            (
                "mqtt://user:pass@host",
                "Connection URL must not include username or password",
            ),
            (
                "ws://host/path?x=1",
                "Connection URL must not include a query string",
            ),
            (
                "wss://host/path#fragment",
                "Connection URL must not include a fragment",
            ),
        ];

        for (url, message) in cases {
            let mut login = default_login();
            login.connection_mode = ConnectionInputMode::Url;
            login.connection_url = url.to_string();

            let err = login.resolve_connection().unwrap_err();
            assert_eq!(err, message);
        }
    }

    #[test]
    fn display_label_uses_normalized_connection() {
        let mut structured = default_login();
        structured.transport = TransportKind::Wss;
        structured.broker = "broker.example.com".to_string();
        structured.ws_path = "mqtt".to_string();
        assert_eq!(
            structured.display_connection_label(),
            "wss://broker.example.com:443/mqtt"
        );

        let mut url = default_login();
        url.connection_mode = ConnectionInputMode::Url;
        url.connection_url = "mqtts://broker.example.com".to_string();
        assert_eq!(
            url.display_connection_label(),
            "mqtts://broker.example.com:8883"
        );
    }

    #[test]
    fn structured_ipv6_hosts_are_formatted_correctly() {
        let mut login = default_login();
        login.transport = TransportKind::Tls;
        login.broker = "::1".to_string();
        login.port = "8883".to_string();

        let resolved = login.resolve_connection().unwrap();
        assert_eq!(resolved.addr, "[::1]:8883");
        assert_eq!(resolved.display_label, "mqtts://[::1]:8883");
        assert_eq!(resolved.tls_domain.as_deref(), Some("::1"));
    }

    #[test]
    fn tls_verification_defaults_to_system_roots() {
        assert_eq!(
            default_login().tls_verification,
            TlsVerificationMode::SystemRoots
        );
    }
}
