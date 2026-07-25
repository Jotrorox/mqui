use std::sync::mpsc::Receiver;

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
        self.event_rx.recv_timeout(timeout)
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}
