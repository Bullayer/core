// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use tokio::sync::broadcast;

use crate::events::ExecutionEvent;

/// Replaces monad-event-ring's mmap-based EventRing with a channel-based source.
pub struct ChannelEventSource {
    receiver: broadcast::Receiver<ExecutionEvent>,
}

impl ChannelEventSource {
    pub fn new(receiver: broadcast::Receiver<ExecutionEvent>) -> Self {
        Self { receiver }
    }

    pub async fn next_event(&mut self) -> Option<ExecutionEvent> {
        match self.receiver.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Closed) => None,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "event source lagged behind, skipping events");
                match self.receiver.recv().await {
                    Ok(event) => Some(event),
                    Err(_) => None,
                }
            }
        }
    }
}
