// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use tokio::sync::{broadcast, mpsc};

use crate::command::ExecutionCommand;
use crate::events::ExecutionEvent;
use crate::runloop::runloop_monad;
use crate::traits::{BlockExecutor, ExecutionDb, SignatureRecovery};
use crate::types::ChainConfig;

const EVENT_CHANNEL_CAPACITY: usize = 4096;
const COMMAND_CHANNEL_CAPACITY: usize = 1024;

pub struct ExecutionEngine {
    cmd_tx: mpsc::Sender<ExecutionCommand>,
    event_tx: broadcast::Sender<ExecutionEvent>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl ExecutionEngine {
    /// Create and start the execution engine.
    /// Returns the engine and a broadcast receiver for execution events.
    pub fn start(
        chain: ChainConfig,
        db: Box<dyn ExecutionDb>,
        executor: Box<dyn BlockExecutor>,
        recovery: Box<dyn SignatureRecovery>,
    ) -> (Self, broadcast::Receiver<ExecutionEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let event_tx_clone = event_tx.clone();
        let handle = tokio::spawn(async move {
            runloop_monad(chain, db, executor, recovery, cmd_rx, event_tx_clone).await;
        });

        let engine = Self {
            cmd_tx,
            event_tx,
            handle: Some(handle),
        };
        (engine, event_rx)
    }

    pub fn command_sender(&self) -> mpsc::Sender<ExecutionCommand> {
        self.cmd_tx.clone()
    }

    pub fn event_sender(&self) -> broadcast::Sender<ExecutionEvent> {
        self.event_tx.clone()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<ExecutionEvent> {
        self.event_tx.subscribe()
    }

    pub async fn shutdown(mut self) {
        let _ = self.cmd_tx.send(ExecutionCommand::Shutdown).await;
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}
