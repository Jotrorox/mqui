use std::time::SystemTime;

pub(crate) const MAX_STORED_MESSAGES: usize = 1000;

#[derive(Clone, Debug, Default)]
pub(crate) struct MqttLoginData {
    pub(crate) broker: String,
    pub(crate) port: String,
    pub(crate) username: String,
    pub(crate) password: String,
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
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }

    pub(crate) fn password_opt(&self) -> Option<&str> {
        let value = self.password.trim();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
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
