// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use monad_crypto::certificate_signature::{CertificateSignaturePubKey, CertificateSignatureRecoverable};
use monad_validator::signature_collection::SignatureCollection;
use tokio::sync::{broadcast, mpsc};

use crate::command::ExecutionCommand;
use crate::events::ExecutionEvent;
use crate::runloop::runloop_monad;
use crate::traits::{BlockExecutor, ExecutionDb};

const EVENT_CHANNEL_CAPACITY: usize = 4096;

pub struct ExecutionEngine<ST, SCT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    cmd_tx: mpsc::UnboundedSender<ExecutionCommand<ST, SCT>>,
    event_tx: broadcast::Sender<ExecutionEvent>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl<ST, SCT> ExecutionEngine<ST, SCT>
where
    ST: CertificateSignatureRecoverable + 'static,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>> + 'static,
{
    pub fn start(
        db: Box<dyn ExecutionDb>,
        executor: Box<dyn BlockExecutor>,
    ) -> (Self, broadcast::Receiver<ExecutionEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let event_tx_clone = event_tx.clone();
        let handle = tokio::spawn(async move {
            runloop_monad::<ST, SCT>(db, executor, cmd_rx, event_tx_clone).await;
        });

        let engine = Self {
            cmd_tx,
            event_tx,
            handle: Some(handle),
        };
        (engine, event_rx)
    }

    pub fn command_sender(&self) -> mpsc::UnboundedSender<ExecutionCommand<ST, SCT>> {
        self.cmd_tx.clone()
    }

    pub fn event_sender(&self) -> broadcast::Sender<ExecutionEvent> {
        self.event_tx.clone()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<ExecutionEvent> {
        self.event_tx.subscribe()
    }

    pub async fn shutdown(mut self) {
        let _ = self.cmd_tx.send(ExecutionCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}
