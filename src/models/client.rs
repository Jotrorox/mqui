use std::sync::mpsc::Receiver;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::models::ipc::{ClientCommand, ClientEvent};

#[derive(Debug)]
pub struct ClientHandle {
    pub(crate) cancellation: CancellationToken,
    pub(crate) join_handle: JoinHandle<()>,
    pub(crate) event_rx: Receiver<ClientEvent>,
    pub(crate) command_tx: tokio_mpsc::Sender<ClientCommand>,
    pub(crate) queued_messages: Arc<AtomicUsize>,
    pub(crate) dropped_messages: Arc<AtomicU64>,
}

impl ClientHandle {
    pub fn try_send(&self, command: ClientCommand) -> Result<(), String> {
        self.command_tx
            .try_send(command)
            .map_err(|err| format!("client command channel is unavailable: {err}"))
    }

    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<ClientEvent, std::sync::mpsc::RecvTimeoutError> {
        let event = self.event_rx.recv_timeout(timeout)?;
        self.message_dequeued(&event);
        Ok(event)
    }

    /// Returns the total number of received MQTT messages discarded because
    /// this client's bounded UI event queue was full.
    pub fn dropped_message_count(&self) -> u64 {
        self.dropped_messages.load(Ordering::Relaxed)
    }

    pub(crate) fn try_recv(&self) -> Result<ClientEvent, std::sync::mpsc::TryRecvError> {
        let event = self.event_rx.try_recv()?;
        self.message_dequeued(&event);
        Ok(event)
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
