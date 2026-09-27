use crate::models::limits::{ByteBudget, BytePermit, MAX_PACKET_BYTES};
use std::sync::mpsc::Receiver;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::sync::{Mutex, atomic::AtomicBool};

use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::models::ipc::{ClientCommand, ClientEvent};

#[derive(Debug)]
pub(crate) struct QueuedEvent {
    pub(crate) event: ClientEvent,
    pub(crate) _permit: BytePermit,
}

#[derive(Debug, Default)]
pub(crate) struct ControlOverflow {
    pub(crate) failed: AtomicBool,
    pub(crate) terminal: Mutex<Option<ClientEvent>>,
}

#[derive(Debug)]
pub(crate) struct QueuedCommand {
    pub(crate) command: ClientCommand,
    pub(crate) permit: BytePermit,
}

#[derive(Debug)]
pub(crate) struct CommandSender {
    pub(crate) sender: tokio_mpsc::Sender<QueuedCommand>,
    pub(crate) budget: ByteBudget,
}

impl CommandSender {
    pub(crate) fn try_send(
        &self,
        command: ClientCommand,
    ) -> Result<(), tokio_mpsc::error::TrySendError<ClientCommand>> {
        use tokio_mpsc::error::TrySendError;
        // Account for the endpoint's retained publish and our topic metadata too.
        let bytes = command.buffer_bytes();
        if command.packet_bytes() > MAX_PACKET_BYTES {
            return Err(TrySendError::Full(command));
        }
        let Some(permit) = self.budget.reserve(bytes.saturating_mul(2)) else {
            return Err(TrySendError::Full(command));
        };
        self.sender
            .try_send(QueuedCommand { command, permit })
            .map_err(|error| match error {
                TrySendError::Full(queued) => TrySendError::Full(queued.command),
                TrySendError::Closed(queued) => TrySendError::Closed(queued.command),
            })
    }
}

#[derive(Debug)]
pub struct ClientHandle {
    pub(crate) cancellation: CancellationToken,
    pub(crate) join_handle: JoinHandle<()>,
    pub(crate) event_rx: Receiver<QueuedEvent>,
    pub(crate) command_tx: CommandSender,
    pub(crate) queued_messages: Arc<AtomicUsize>,
    pub(crate) control_overflow: Arc<ControlOverflow>,
    pub(crate) event_stream_closed: AtomicBool,
    pub(crate) dropped_messages: Arc<AtomicU64>,
}

impl ClientHandle {
    pub fn try_send(&self, command: ClientCommand) -> Result<(), String> {
        if command.packet_bytes() > MAX_PACKET_BYTES {
            return Err(format!(
                "command exceeds the {MAX_PACKET_BYTES} byte packet limit"
            ));
        }
        self.command_tx
            .try_send(command)
            .map_err(|err| format!("client command channel is unavailable: {err}"))
    }

    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<ClientEvent, std::sync::mpsc::RecvTimeoutError> {
        // Drain queued events before the terminal overflow notification so that
        // an older Connected event cannot overwrite the terminal failure.
        match self.try_recv() {
            Ok(event) => return Ok(event),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match self.event_rx.recv_timeout(timeout) {
            Ok(queued) => {
                self.message_dequeued(&queued.event);
                Ok(queued.event)
            }
            Err(error) => self
                .control_overflow
                .terminal
                .lock()
                .unwrap()
                .take()
                .ok_or(error),
        }
    }

    /// Returns the total number of received MQTT messages discarded because
    /// this client's UI event queue or byte budget was full.
    pub fn dropped_message_count(&self) -> u64 {
        self.dropped_messages.load(Ordering::Relaxed)
    }

    pub(crate) fn try_recv(&self) -> Result<ClientEvent, std::sync::mpsc::TryRecvError> {
        match self.event_rx.try_recv() {
            Ok(queued) => {
                self.message_dequeued(&queued.event);
                Ok(queued.event)
            }
            Err(error) => {
                if let Some(event) = self.control_overflow.terminal.lock().unwrap().take() {
                    return Ok(event);
                }
                if error == std::sync::mpsc::TryRecvError::Disconnected {
                    self.event_stream_closed.store(true, Ordering::Release);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn events_drained(&self) -> bool {
        self.event_stream_closed.load(Ordering::Acquire)
    }

    fn message_dequeued(&self, event: &ClientEvent) {
        if matches!(event, ClientEvent::MessageReceived { .. }) {
            self.queued_messages.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_reserve_capacity_until_dropped_and_reject_large_packets() {
        let (sender, mut receiver) = tokio_mpsc::channel(10);
        let bytes = ClientCommand::Disconnect.buffer_bytes() * 2;
        let sender = CommandSender {
            sender,
            budget: ByteBudget::new(bytes),
        };
        sender.try_send(ClientCommand::Disconnect).unwrap();
        assert!(sender.try_send(ClientCommand::Disconnect).is_err());
        let queued = receiver.try_recv().unwrap();
        assert!(sender.try_send(ClientCommand::Disconnect).is_err());
        drop(queued);
        sender.try_send(ClientCommand::Disconnect).unwrap();
        drop(receiver.try_recv().unwrap());
        assert!(
            sender
                .try_send(ClientCommand::Publish {
                    topic: "t".into(),
                    payload: vec![0; MAX_PACKET_BYTES],
                    qos: 0,
                    retain: false
                })
                .is_err()
        );
        assert!(sender.budget.reserve(bytes).is_some());
    }
}
