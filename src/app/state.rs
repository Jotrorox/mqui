use std::collections::VecDeque;

use crate::models::mqtt::{MqttLoginData, ReceivedMessage, SubscriptionEntry};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabKind {
    Client,
}

#[derive(Clone, Debug)]
pub(crate) enum TabState {
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

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) state: TabState,
}
