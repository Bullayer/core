// Copyright (C) 2025 Category Labs, Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use alloy_primitives::{TxHash, U256};
use monad_eth_txpool_types::{
    EthTxPoolBridgeEvictionQueue, EthTxPoolBridgeState, EthTxPoolEvent, EthTxPoolEventType,
    EthTxPoolIpcTx, EthTxPoolSnapshot, EthTxPoolTxInputStream,
};
use pin_project::pin_project;
use tokio::{
    sync::mpsc,
    time::{self, Sleep},
};
use tracing::warn;

const MAX_BATCH_LEN: usize = 128;
const BATCH_TIMER_INTERVAL_MS: u64 = 8;

pub type EthTxPoolChannelTxSender = mpsc::Sender<EthTxPoolIpcTx>;

#[pin_project(project = EthTxPoolChannelInputStreamProjected)]
pub struct EthTxPoolChannelInputStream {
    rx: mpsc::Receiver<EthTxPoolIpcTx>,

    state: EthTxPoolBridgeState,
    eviction_queue: Arc<Mutex<EthTxPoolBridgeEvictionQueue>>,

    queue: BTreeMap<U256, VecDeque<EthTxPoolIpcTx>>,
    queue_len: usize,
    channel_closed: bool,
    #[pin]
    queue_timer: Sleep,
}

impl EthTxPoolChannelInputStream {
    pub fn new(
        rx: mpsc::Receiver<EthTxPoolIpcTx>,
        state: EthTxPoolBridgeState,
        eviction_queue: Arc<Mutex<EthTxPoolBridgeEvictionQueue>>,
    ) -> Self {
        Self {
            rx,
            state,
            eviction_queue,
            queue: BTreeMap::default(),
            queue_len: 0,
            channel_closed: false,
            queue_timer: time::sleep(Duration::ZERO),
        }
    }
}

impl EthTxPoolTxInputStream for EthTxPoolChannelInputStream {
    fn poll_txs(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _generate_snapshot: impl Fn() -> EthTxPoolSnapshot,
    ) -> Poll<Vec<EthTxPoolIpcTx>> {
        let EthTxPoolChannelInputStreamProjected {
            rx,
            state: _,
            eviction_queue: _,
            queue,
            queue_len,
            channel_closed,
            mut queue_timer,
        } = self.project();

        let queue_was_empty = *queue_len == 0;

        if !*channel_closed {
            while *queue_len < MAX_BATCH_LEN {
                match rx.poll_recv(cx) {
                    Poll::Ready(Some(tx)) => {
                        queue.entry(tx.priority).or_default().push_back(tx);
                        *queue_len += 1;
                    }
                    Poll::Ready(None) => {
                        warn!("EthTxPoolChannelInputStream: channel closed, no more txs will be received");
                        *channel_closed = true;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
        }

        if *queue_len == 0 {
            return Poll::Pending;
        }

        if queue_was_empty {
            queue_timer.set(time::sleep(Duration::from_millis(BATCH_TIMER_INTERVAL_MS)));
        }

        if *queue_len < MAX_BATCH_LEN && queue_timer.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }

        let mut batch = Vec::default();

        while let Some(batch_remaining_capacity) = MAX_BATCH_LEN.checked_sub(batch.len()) {
            if batch_remaining_capacity == 0 {
                break;
            }

            let Some(top_priority) = queue.last_entry() else {
                break;
            };

            if batch_remaining_capacity < top_priority.get().len() {
                batch.extend(top_priority.into_mut().drain(0..batch_remaining_capacity));
                break;
            } else {
                batch.extend(top_priority.remove());
            }
        }

        *queue_len -= batch.len();

        Poll::Ready(batch)
    }

    fn broadcast_tx_events(self: Pin<&mut Self>, events: BTreeMap<TxHash, EthTxPoolEventType>) {
        if events.is_empty() {
            return;
        }

        let this = self.project();

        let events: Vec<EthTxPoolEvent> = events
            .into_iter()
            .map(|(tx_hash, action)| EthTxPoolEvent { tx_hash, action })
            .collect();

        let mut eviction_queue = this.eviction_queue.lock().expect("eviction queue poisoned");
        this.state.handle_events(&mut eviction_queue, events);
    }
}
