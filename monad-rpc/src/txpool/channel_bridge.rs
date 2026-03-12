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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_consensus::TxEnvelope;
use flume::Receiver;
use monad_eth_txpool_types::{
    EthTxPoolBridgeEvictionQueue, EthTxPoolBridgeState, EthTxPoolBridgeStateView,
    EthTxPoolIpcTx, TxStatusReceiverSender,
};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use super::{client::EthTxPoolBridgeClient, handle::EthTxPoolBridgeHandle};

pub const ETH_TXPOOL_CHANNEL_BRIDGE_CHANNEL_SIZE: usize = 1024;

pub struct EthTxPoolChannelBridge {
    tx_sender: mpsc::Sender<EthTxPoolIpcTx>,
    state: EthTxPoolBridgeState,
    eviction_queue: Arc<Mutex<EthTxPoolBridgeEvictionQueue>>,
}

impl EthTxPoolChannelBridge {
    pub fn start(
        tx_sender: mpsc::Sender<EthTxPoolIpcTx>,
        state_view: EthTxPoolBridgeStateView,
        state: EthTxPoolBridgeState,
        eviction_queue: Arc<Mutex<EthTxPoolBridgeEvictionQueue>>,
    ) -> (EthTxPoolBridgeClient, EthTxPoolBridgeHandle) {
        let (flume_tx, flume_rx) = flume::bounded(ETH_TXPOOL_CHANNEL_BRIDGE_CHANNEL_SIZE);

        let client = EthTxPoolBridgeClient::new(flume_tx, state_view);

        let bridge = Self {
            tx_sender,
            state,
            eviction_queue,
        };

        let handle = EthTxPoolBridgeHandle::new(tokio::task::spawn(bridge.run(flume_rx)));

        (client, handle)
    }

    async fn run(self, tx_receiver: Receiver<(TxEnvelope, TxStatusReceiverSender)>) {
        let mut cleanup_timer = tokio::time::interval(Duration::from_secs(5));
        cleanup_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;

                result = tx_receiver.recv_async() => {
                    let tx_pair = match result {
                        Ok(tx_pair) => tx_pair,
                        Err(flume::RecvError::Disconnected) => {
                            error!("EthTxPoolChannelBridge tx receiver disconnected");
                            break;
                        },
                    };

                    for (tx, tx_status_recv_send) in std::iter::once(tx_pair).chain(tx_receiver.drain()) {
                        {
                            let mut eq = self.eviction_queue.lock().expect("eviction queue poisoned");
                            if !self.state.add_tx(&mut eq, &tx, tx_status_recv_send) {
                                continue;
                            }
                        }

                        let ipc_tx = EthTxPoolIpcTx::new_with_default_priority(tx, Vec::default());
                        if let Err(e) = self.tx_sender.send(ipc_tx).await {
                            error!("EthTxPoolChannelBridge channel send failed (receiver dropped): {}", e);
                            return;
                        }
                    }
                }

                now = cleanup_timer.tick() => {
                    debug!("EthTxPoolChannelBridge running state cleanup");
                    let mut eq = self.eviction_queue.lock().expect("eviction queue poisoned");
                    self.state.cleanup(&mut eq, now);
                }
            }
        }

        warn!("EthTxPoolChannelBridge shutting down");
    }
}
