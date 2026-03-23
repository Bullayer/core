// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use std::sync::{Arc, RwLock};

use monad_crypto::certificate_signature::{CertificateSignaturePubKey, CertificateSignatureRecoverable};
use monad_validator::signature_collection::SignatureCollection;
use tokio::sync::{broadcast, mpsc};

use crate::command::ExecutionCommand;
use crate::events::ExecutionEvent;
use crate::runloop::runloop_monad;
use crate::statesync::deletion_tracker::FinalizedDeletions;
use crate::statesync::server_db::{LiveStateSyncProvider, StateSyncServerDb};
use crate::statesync::{StateSyncProvider, StateSyncTraversable};
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
        traversable: Arc<dyn StateSyncTraversable>,
    ) -> (Self, broadcast::Receiver<ExecutionEvent>, Arc<dyn StateSyncProvider>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let finalized_deletions = Arc::new(RwLock::new(FinalizedDeletions::new()));
        let server_db = StateSyncServerDb::new(db, Arc::clone(&finalized_deletions));

        let provider: Arc<dyn StateSyncProvider> = Arc::new(LiveStateSyncProvider::new(
            traversable,
            finalized_deletions,
        ));

        let event_tx_clone = event_tx.clone();
        let handle = tokio::spawn(async move {
            runloop_monad::<StateSyncServerDb, ST, SCT>(server_db, executor, cmd_rx, event_tx_clone).await;
        });

        let engine = Self {
            cmd_tx,
            event_tx,
            handle: Some(handle),
        };
        (engine, event_rx, provider)
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
