#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting,
    Disconnecting,
    Disconnected,
    Failed,
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl ConnectionState {
    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        use ConnectionState::{
            Connected, Connecting, Disconnected, Disconnecting, Failed, Reconnecting,
        };
        matches!(
            (self, next),
            (Disconnected | Failed, Connecting | Reconnecting)
                | (
                    Connecting | Reconnecting,
                    Connected | Failed | Disconnecting
                )
                | (Connected, Reconnecting | Disconnecting | Failed)
                | (Disconnecting, Disconnected | Failed)
        ) || self == next
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionState::*;

    #[test]
    fn connection_state_transitions_are_explicit() {
        assert!(Disconnected.can_transition_to(Connecting));
        assert!(Connecting.can_transition_to(Connected));
        assert!(Connected.can_transition_to(Reconnecting));
        assert!(Reconnecting.can_transition_to(Failed));
        assert!(Connected.can_transition_to(Disconnecting));
        assert!(Disconnecting.can_transition_to(Disconnected));
        assert!(!Disconnected.can_transition_to(Connected));
        assert!(!Connected.can_transition_to(Connecting));
    }
}

#[derive(Debug)]
pub(crate) enum ClientEvent {
    State(ConnectionState),
    Status(String),
    Error(String),
    Connected,
    Disconnected(String),
    Subscribed {
        topic: String,
        qos: u8,
        details: String,
    },
    Unsubscribed {
        topic: String,
        details: String,
    },
    Published {
        topic: String,
        packet_id: Option<u16>,
    },
    MessageReceived {
        topic: String,
        qos: u8,
        retain: bool,
        payload: Vec<u8>,
    },
}

#[derive(Debug)]
pub(crate) enum ClientCommand {
    Disconnect,
    Subscribe {
        topic: String,
        qos: u8,
    },
    Unsubscribe {
        topic: String,
    },
    Publish {
        topic: String,
        payload: Vec<u8>,
        qos: u8,
        retain: bool,
    },
}
