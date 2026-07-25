use std::time::SystemTime;

use crate::app::App;
use crate::app::state::{
    ActionableError, ActivityItem, ActivityLevel, ErrorScope, MAX_ACTIVITY_ITEMS, TabState,
};
use crate::models::ipc::{ClientEvent, ConnectionState};
use crate::models::mqtt::{MAX_STORED_MESSAGES, ReceivedMessage, SubscriptionEntry};

fn error_scope(message: &str) -> ErrorScope {
    let message = message.to_ascii_lowercase();
    if message.contains("unsuback") || message.contains("unsubscribe") {
        ErrorScope::Unsubscribe
    } else if message.contains("suback") || message.contains("subscribe") {
        ErrorScope::Subscribe
    } else if message.contains("publish") || message.contains("puback") {
        ErrorScope::Publish
    } else {
        ErrorScope::General
    }
}

pub(crate) fn push_activity(
    activity: &mut std::collections::VecDeque<ActivityItem>,
    timestamp: SystemTime,
    level: ActivityLevel,
    message: impl Into<String>,
) {
    activity.push_front(ActivityItem {
        timestamp,
        level,
        message: message.into(),
    });
    activity.truncate(MAX_ACTIVITY_ITEMS);
}

fn clear_error(current_error: &mut Option<ActionableError>, scope: ErrorScope) {
    if current_error
        .as_ref()
        .is_some_and(|error| error.scope == scope)
    {
        *current_error = None;
    }
}

/// Applies one client event to a tab and returns a connection-state update for the app.
pub(crate) fn reduce_client_event(
    state: &mut TabState,
    event: ClientEvent,
    now: SystemTime,
) -> Option<ConnectionState> {
    let TabState::Client {
        connection_state,
        current_error,
        activity,
        subscriptions,
        messages,
        max_messages,
        received_count,
        capture_paused,
        paused_message_count,
        next_message_id,
        selected_message_id,
        published_count,
        ..
    } = state;

    match event {
        ClientEvent::State(state) => {
            *connection_state = state;
            Some(state)
        }
        ClientEvent::Status(status) => {
            push_activity(activity, now, ActivityLevel::Info, status);
            None
        }
        ClientEvent::Error(message) => {
            *current_error = Some(ActionableError {
                scope: error_scope(&message),
                message,
            });
            None
        }
        ClientEvent::Connected => {
            *connection_state = ConnectionState::Connected;
            clear_error(current_error, ErrorScope::Connection);
            push_activity(activity, now, ActivityLevel::Success, "Connected");
            Some(ConnectionState::Connected)
        }
        ClientEvent::Disconnected(message) => {
            *connection_state = ConnectionState::Failed;
            *current_error = Some(ActionableError {
                message,
                scope: ErrorScope::Connection,
            });
            Some(ConnectionState::Failed)
        }
        ClientEvent::Subscribed { topic, qos, .. } => {
            if let Some(entry) = subscriptions.iter_mut().find(|entry| entry.topic == topic) {
                entry.qos = qos;
            } else {
                subscriptions.push(SubscriptionEntry {
                    topic: topic.clone(),
                    qos,
                });
            }
            clear_error(current_error, ErrorScope::Subscribe);
            push_activity(
                activity,
                now,
                ActivityLevel::Success,
                format!("Subscribed to '{topic}' at QoS {qos}"),
            );
            None
        }
        ClientEvent::Unsubscribed { topic, .. } => {
            subscriptions.retain(|entry| entry.topic != topic);
            clear_error(current_error, ErrorScope::Unsubscribe);
            push_activity(
                activity,
                now,
                ActivityLevel::Success,
                format!("Unsubscribed from '{topic}'"),
            );
            None
        }
        ClientEvent::Published {
            topic,
            packet_id: _,
        } => {
            *published_count += 1;
            clear_error(current_error, ErrorScope::Publish);
            push_activity(
                activity,
                now,
                ActivityLevel::Success,
                format!("Published to '{topic}'"),
            );
            None
        }
        ClientEvent::MessageReceived {
            topic,
            qos,
            retain,
            payload,
        } => {
            *received_count += 1;
            if *capture_paused {
                *paused_message_count += 1;
                return None;
            }
            let id = *next_message_id;
            *next_message_id = next_message_id.wrapping_add(1);
            messages.push_back(ReceivedMessage::new(id, now, topic, qos, retain, payload));
            while messages.len() > (*max_messages).min(MAX_STORED_MESSAGES) {
                let _ = messages.pop_front();
            }
            if selected_message_id
                .is_some_and(|selected| !messages.iter().any(|message| message.id == selected))
            {
                *selected_message_id = None;
            }
            None
        }
    }
}

pub(crate) fn pump_client_events(app: &mut App) {
    for tab in &mut app.tabs {
        let TabState::Client {
            dropped_message_count,
            current_client_dropped_message_count,
            ..
        } = &mut tab.state;

        let Some(client) = app.clients.get_mut(&tab.id) else {
            continue;
        };
        let current_dropped = client.dropped_message_count();
        *dropped_message_count +=
            current_dropped.saturating_sub(*current_client_dropped_message_count);
        *current_client_dropped_message_count = current_dropped;

        loop {
            match client.try_recv() {
                Ok(event) => {
                    if let Some(state) =
                        reduce_client_event(&mut tab.state, event, SystemTime::now())
                    {
                        app.connection_states.insert(tab.id, state);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::app::state::{MessageFilterMode, PayloadView};
    use crate::models::mqtt::MqttLoginData;

    fn state() -> TabState {
        TabState::Client {
            mqtt_login: MqttLoginData::default(),
            connection_state: ConnectionState::Connecting,
            current_error: None,
            activity: VecDeque::new(),
            subscribe_topic: String::new(),
            subscribe_qos: 0,
            unsubscribe_topic: String::new(),
            editing_subscription_topic: None,
            editing_subscription_value: String::new(),
            editing_subscription_qos: 0,
            publish_topic: String::new(),
            publish_qos: 0,
            publish_retain: false,
            publish_payload: String::new(),
            payload_view: PayloadView::Text,
            topic_filter: String::new(),
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
        }
    }

    fn error(state: &TabState) -> Option<&ActionableError> {
        let TabState::Client { current_error, .. } = state;
        current_error.as_ref()
    }

    #[test]
    fn information_does_not_erase_error() {
        let mut state = state();
        reduce_client_event(
            &mut state,
            ClientEvent::Error("Failed to publish".into()),
            SystemTime::now(),
        );
        reduce_client_event(
            &mut state,
            ClientEvent::Status("Keep-alive sent".into()),
            SystemTime::now(),
        );
        assert_eq!(error(&state).unwrap().message, "Failed to publish");
    }

    #[test]
    fn successful_reconnect_clears_connection_error() {
        let mut state = state();
        reduce_client_event(
            &mut state,
            ClientEvent::Disconnected("Connection refused".into()),
            SystemTime::now(),
        );
        reduce_client_event(&mut state, ClientEvent::Connected, SystemTime::now());
        assert!(error(&state).is_none());
    }

    #[test]
    fn publish_and_suback_are_activity_not_errors() {
        let mut state = state();
        reduce_client_event(
            &mut state,
            ClientEvent::Published {
                topic: "out".into(),
                packet_id: Some(1),
            },
            SystemTime::now(),
        );
        reduce_client_event(
            &mut state,
            ClientEvent::Subscribed {
                topic: "in".into(),
                qos: 1,
                details: "Granted QoS 1".into(),
            },
            SystemTime::now(),
        );
        let TabState::Client {
            current_error,
            activity,
            ..
        } = state;
        assert!(current_error.is_none());
        assert_eq!(activity.len(), 2);
    }

    #[test]
    fn activity_is_bounded() {
        let mut state = state();
        for index in 0..(MAX_ACTIVITY_ITEMS + 3) {
            reduce_client_event(
                &mut state,
                ClientEvent::Status(format!("Activity {index}")),
                SystemTime::now(),
            );
        }
        let TabState::Client { activity, .. } = state;
        assert_eq!(activity.len(), MAX_ACTIVITY_ITEMS);
        assert_eq!(activity.front().unwrap().message, "Activity 10");
    }

    fn receive(state: &mut TabState, topic: &str) {
        reduce_client_event(
            state,
            ClientEvent::MessageReceived {
                topic: topic.into(),
                qos: 1,
                retain: false,
                payload: topic.as_bytes().to_vec(),
            },
            SystemTime::now(),
        );
    }

    #[test]
    fn message_ids_are_stable_and_evicted_selection_is_cleared() {
        let mut state = state();
        let TabState::Client { max_messages, .. } = &mut state;
        *max_messages = 2;
        receive(&mut state, "one");
        receive(&mut state, "two");
        let TabState::Client {
            messages,
            selected_message_id,
            ..
        } = &mut state;
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        *selected_message_id = Some(0);

        receive(&mut state, "three");
        let TabState::Client {
            messages,
            selected_message_id,
            ..
        } = &state;
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(*selected_message_id, None);
    }

    #[test]
    fn paused_capture_counts_without_storing() {
        let mut state = state();
        let TabState::Client { capture_paused, .. } = &mut state;
        *capture_paused = true;
        receive(&mut state, "skipped");
        let TabState::Client {
            messages,
            received_count,
            paused_message_count,
            next_message_id,
            ..
        } = &state;
        assert!(messages.is_empty());
        assert_eq!(*received_count, 1);
        assert_eq!(*paused_message_count, 1);
        assert_eq!(*next_message_id, 0);
    }
}
