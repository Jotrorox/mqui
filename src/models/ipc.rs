#[derive(Debug)]
pub(crate) enum ClientEvent {
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
    Subscribe { topic: String, qos: u8 },
    Unsubscribe { topic: String },
    Publish {
        topic: String,
        payload: Vec<u8>,
        qos: u8,
        retain: bool,
    },
}
