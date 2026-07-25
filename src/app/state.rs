use std::collections::VecDeque;
use std::time::SystemTime;

use crate::models::ipc::ConnectionState;
use crate::models::mqtt::{MqttLoginData, ReceivedMessage, SubscriptionEntry};

pub(crate) const MAX_ACTIVITY_ITEMS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorScope {
    Connection,
    Subscribe,
    Unsubscribe,
    Publish,
    General,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionableError {
    pub(crate) message: String,
    pub(crate) scope: ErrorScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityLevel {
    Info,
    Success,
    Warning,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivityItem {
    pub(crate) timestamp: SystemTime,
    pub(crate) level: ActivityLevel,
    pub(crate) message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabKind {
    Client,
}

#[derive(Clone, Debug)]
pub(crate) enum TabState {
    Client {
        mqtt_login: MqttLoginData,
        connection_state: ConnectionState,
        current_error: Option<ActionableError>,
        activity: VecDeque<ActivityItem>,
        subscribe_topic: String,
        subscribe_qos: u8,
        unsubscribe_topic: String,
        editing_subscription_topic: Option<String>,
        editing_subscription_value: String,
        editing_subscription_qos: u8,
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
        dropped_message_count: u64,
        current_client_dropped_message_count: u64,
        published_count: u64,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) state: TabState,
}
