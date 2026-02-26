use std::sync::mpsc::Receiver;

use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::models::ipc::{ClientCommand, ClientEvent};

#[derive(Debug)]
pub(crate) struct ClientHandle {
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
    pub(crate) join_handle: JoinHandle<()>,
    pub(crate) event_rx: Receiver<ClientEvent>,
    pub(crate) command_tx: tokio_mpsc::UnboundedSender<ClientCommand>,
}
