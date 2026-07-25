use std::sync::mpsc::Receiver;

use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::models::ipc::{ClientCommand, ClientEvent};

#[derive(Debug)]
pub(crate) struct ClientHandle {
    pub(crate) cancellation: CancellationToken,
    pub(crate) join_handle: JoinHandle<()>,
    pub(crate) event_rx: Receiver<ClientEvent>,
    pub(crate) command_tx: tokio_mpsc::Sender<ClientCommand>,
}
