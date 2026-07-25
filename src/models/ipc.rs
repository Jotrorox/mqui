#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
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
    pub(crate) const fn can_connect(self) -> bool {
        matches!(self, Self::Connected | Self::Disconnected | Self::Failed)
    }

    pub(crate) const fn can_disconnect(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Connected | Self::Reconnecting
        )
    }

    pub(crate) const fn can_force_disconnect(self) -> bool {
        !matches!(self, Self::Disconnected)
    }

    pub(crate) const fn can_use_client(self) -> bool {
        matches!(self, Self::Connected)
    }

    pub(crate) const fn can_cancel_reconnect(self) -> bool {
        matches!(self, Self::Reconnecting)
    }

    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        use ConnectionState::{
            Connected, Connecting, Disconnected, Disconnecting, Failed, Reconnecting,
        };
        matches!(
            (self, next),
            (Disconnected | Failed, Connecting | Reconnecting)
                | (Connecting, Connected | Failed | Disconnecting)
                | (
                    Reconnecting,
                    Connecting | Connected | Failed | Disconnecting
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
        assert!(Reconnecting.can_transition_to(Connecting));
        assert!(Connected.can_transition_to(Disconnecting));
        assert!(Disconnecting.can_transition_to(Disconnected));
        assert!(!Disconnected.can_transition_to(Connected));
        assert!(!Connected.can_transition_to(Connecting));
    }

    #[test]
    fn allowed_actions_follow_connection_state() {
        assert!(Disconnected.can_connect());
        assert!(Failed.can_connect());
        assert!(Connected.can_connect());
        assert!(Connected.can_disconnect());
        assert!(Reconnecting.can_cancel_reconnect());
        assert!(Connected.can_use_client());
        assert!(!Connecting.can_use_client());
        assert!(!Disconnected.can_force_disconnect());
    }
}

#[derive(Debug)]
pub enum ClientEvent {
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
pub enum ClientCommand {
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
