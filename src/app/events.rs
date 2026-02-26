use std::time::SystemTime;

use crate::app::App;
use crate::app::state::TabState;
use crate::models::ipc::ClientEvent;
use crate::models::mqtt::{MAX_STORED_MESSAGES, ReceivedMessage, SubscriptionEntry};

pub(crate) fn pump_client_events(app: &mut App) {
    for tab in &mut app.tabs {
        let TabState::Client {
            connection_status,
            last_error,
            subscriptions,
            messages,
            received_count,
            published_count,
            ..
        } = &mut tab.state;

        let Some(client) = app.clients.get_mut(&tab.id) else {
            continue;
        };

        loop {
            match client.event_rx.try_recv() {
                Ok(ClientEvent::Status(status)) => {
                    *connection_status = status;
                }
                Ok(ClientEvent::Error(err)) => {
                    *last_error = Some(err);
                }
                Ok(ClientEvent::Connected) => {
                    *connection_status = "Connected".to_string();
                    *last_error = None;
                }
                Ok(ClientEvent::Disconnected(msg)) => {
                    *connection_status = "Disconnected".to_string();
                    *last_error = Some(msg);
                }
                Ok(ClientEvent::Subscribed { topic, qos, details }) => {
                    if let Some(entry) = subscriptions.iter_mut().find(|entry| entry.topic == topic) {
                        entry.qos = qos;
                    } else {
                        subscriptions.push(SubscriptionEntry {
                            topic: topic.clone(),
                            qos,
                        });
                    }
                    *connection_status = format!("Subscribed to '{topic}'");
                    *last_error = Some(format!("SUBACK: {details}"));
                }
                Ok(ClientEvent::Unsubscribed { topic, details }) => {
                    subscriptions.retain(|entry| entry.topic != topic);
                    *connection_status = format!("Unsubscribed from '{topic}'");
                    *last_error = Some(format!("UNSUBACK: {details}"));
                }
                Ok(ClientEvent::Published { topic, packet_id }) => {
                    *published_count += 1;
                    if let Some(id) = packet_id {
                        *connection_status = format!("Published to '{topic}' (packet id {id})");
                    } else {
                        *connection_status = format!("Published to '{topic}'");
                    }
                }
                Ok(ClientEvent::MessageReceived {
                    topic,
                    qos,
                    retain,
                    payload,
                }) => {
                    *received_count += 1;
                    messages.push_back(ReceivedMessage {
                        timestamp: SystemTime::now(),
                        topic,
                        qos,
                        retain,
                        payload,
                    });
                    while messages.len() > MAX_STORED_MESSAGES {
                        let _ = messages.pop_front();
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }
}
