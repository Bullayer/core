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

use std::{collections::HashMap, sync::Arc};

use alloy_primitives::B256;
use itertools::Itertools;
use monad_execution_engine::events::{BlockCommitState, ExecutionEvent};
use monad_types::BlockId;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use super::{EventServerClient, EventServerEvent, BROADCAST_CHANNEL_SIZE};
use crate::types::{eth_json::MonadNotification, serialize::JsonSerialized};

/// Cached block data from a BlockProposed event, reused across state transitions.
struct ProposedBlock {
    block_id: BlockId,
    header: alloy_rpc_types::Header,
    transactions: Vec<alloy_rpc_types::Transaction>,
    receipts: Vec<alloy_rpc_types::TransactionReceipt>,
}

pub struct EventServer;

impl EventServer {
    pub fn start(
        mut event_rx: broadcast::Receiver<ExecutionEvent>,
    ) -> EventServerClient {
        let (broadcast_tx, _) = broadcast::channel(BROADCAST_CHANNEL_SIZE);

        let tx = broadcast_tx.clone();
        let handle = tokio::spawn(async move {
            const MAX_PROPOSED_BLOCKS: usize = 64;
            let mut proposed_blocks: HashMap<BlockId, Arc<ProposedBlock>> = HashMap::new();

            loop {
                let event = match event_rx.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "event server lagged behind");
                        broadcast_event(&tx, EventServerEvent::Gap);
                        proposed_blocks.clear();
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("execution event channel closed, event server exiting");
                        break;
                    }
                };

                match event {
                    ExecutionEvent::BlockProposed {
                        block_id,
                        header,
                        transactions,
                        receipts,
                        eth_block_hash,
                        ..
                    } => {
                        if proposed_blocks.len() >= MAX_PROPOSED_BLOCKS {
                            warn!(
                                count = proposed_blocks.len(),
                                "proposed_blocks cache full, evicting oldest entries"
                            );
                            proposed_blocks.clear();
                        }

                        let proposed = build_proposed_block(
                            block_id,
                            &header,
                            &transactions,
                            &receipts,
                            eth_block_hash,
                        );
                        let proposed = Arc::new(proposed);
                        broadcast_block_updates(
                            &tx,
                            &proposed,
                            BlockCommitState::Proposed,
                        );
                        proposed_blocks.insert(block_id, proposed);
                    }

                    ExecutionEvent::BlockVoted {
                        block_id,
                        ..
                    } => {
                        if let Some(proposed) = proposed_blocks.get(&block_id) {
                            broadcast_block_updates(
                                &tx,
                                proposed,
                                BlockCommitState::Voted,
                            );
                        } else {
                            warn!(?block_id, "BlockVoted for unknown block");
                        }
                    }

                    ExecutionEvent::BlockFinalized {
                        block_id,
                        ..
                    } => {
                        if let Some(proposed) = proposed_blocks.remove(&block_id) {
                            broadcast_block_updates(
                                &tx,
                                &proposed,
                                BlockCommitState::Finalized,
                            );
                        } else {
                            warn!(?block_id, "BlockFinalized for unknown block");
                        }
                    }

                    ExecutionEvent::BlockVerified { seq_num } => {
                        debug!(seq_num = seq_num.0, "BlockVerified event (no broadcast)");
                    }
                }
            }
        });

        EventServerClient::new(broadcast_tx, handle)
    }
}

fn build_proposed_block(
    block_id: BlockId,
    header: &alloy_consensus::Header,
    transactions: &[alloy_consensus::TxEnvelope],
    receipts: &[alloy_consensus::ReceiptEnvelope],
    eth_block_hash: B256,
) -> ProposedBlock {
    let rpc_header = convert_header(header, eth_block_hash);
    let base_fee = rpc_header.inner.base_fee_per_gas;

    let rpc_txs = convert_transactions(transactions, eth_block_hash, header.number, base_fee);
    let rpc_receipts = convert_receipts(
        transactions,
        receipts,
        eth_block_hash,
        header.number,
        header.timestamp,
        base_fee,
    );

    ProposedBlock {
        block_id,
        header: rpc_header,
        transactions: rpc_txs,
        receipts: rpc_receipts,
    }
}

fn convert_header(
    header: &alloy_consensus::Header,
    eth_block_hash: B256,
) -> alloy_rpc_types::Header {
    let size = header.size();
    alloy_rpc_types::Header {
        hash: eth_block_hash,
        inner: header.clone(),
        total_difficulty: Some(alloy_primitives::U256::ZERO),
        size: Some(alloy_primitives::U256::from(size)),
    }
}

fn convert_transactions(
    transactions: &[alloy_consensus::TxEnvelope],
    block_hash: B256,
    block_number: u64,
    base_fee: Option<u64>,
) -> Vec<alloy_rpc_types::Transaction> {
    use alloy_consensus::transaction::SignerRecoverable;

    transactions
        .iter()
        .enumerate()
        .map(|(tx_idx, tx)| {
            let sender = tx.recover_signer().unwrap_or_default();
            let effective_gas_price =
                alloy_consensus::Transaction::effective_gas_price(tx, base_fee);

            alloy_rpc_types::Transaction {
                inner: alloy_consensus::transaction::Recovered::new_unchecked(
                    tx.clone(),
                    sender,
                ),
                block_hash: Some(block_hash),
                block_number: Some(block_number),
                transaction_index: Some(tx_idx as u64),
                effective_gas_price: Some(effective_gas_price),
            }
        })
        .collect()
}

fn convert_receipts(
    transactions: &[alloy_consensus::TxEnvelope],
    receipts: &[alloy_consensus::ReceiptEnvelope],
    block_hash: B256,
    block_number: u64,
    block_timestamp: u64,
    base_fee: Option<u64>,
) -> Vec<alloy_rpc_types::TransactionReceipt> {
    use alloy_consensus::transaction::SignerRecoverable;

    let mut log_index = 0u64;

    receipts
        .iter()
        .enumerate()
        .map(|(tx_idx, receipt_envelope)| {
            let tx = transactions.get(tx_idx);
            let tx_hash = tx.map(|t| *t.tx_hash()).unwrap_or_default();
            let sender = tx
                .and_then(|t| t.recover_signer().ok())
                .unwrap_or_default();

            let rpc_receipt_envelope = map_receipt_envelope(
                receipt_envelope.clone(),
                block_hash,
                block_number,
                block_timestamp,
                tx_hash,
                tx_idx as u64,
                &mut log_index,
            );

            let inner_receipt = receipt_envelope.as_receipt().expect("valid receipt");
            let cumulative_gas_used = inner_receipt.cumulative_gas_used;
            let gas_used = if tx_idx > 0 {
                let prev_cumulative = receipts
                    .get(tx_idx - 1)
                    .and_then(|r| r.as_receipt())
                    .map(|r| r.cumulative_gas_used)
                    .unwrap_or(0);
                cumulative_gas_used.saturating_sub(prev_cumulative)
            } else {
                cumulative_gas_used
            };

            let effective_gas_price = tx
                .map(|t| alloy_consensus::Transaction::effective_gas_price(t, base_fee))
                .unwrap_or(0);

            let (to, contract_address) = if let Some(t) = tx {
                let to_addr = alloy_consensus::Transaction::to(t);
                match to_addr {
                    Some(addr) => (Some(addr), None),
                    None => {
                        let nonce = alloy_consensus::Transaction::nonce(t);
                        (None, Some(sender.create(nonce)))
                    }
                }
            } else {
                (None, None)
            };

            alloy_rpc_types::TransactionReceipt {
                inner: rpc_receipt_envelope,
                transaction_hash: tx_hash,
                transaction_index: Some(tx_idx as u64),
                block_hash: Some(block_hash),
                block_number: Some(block_number),
                gas_used,
                effective_gas_price,
                blob_gas_used: None,
                blob_gas_price: None,
                from: sender,
                to,
                contract_address,
            }
        })
        .collect()
}

fn map_receipt_envelope(
    envelope: alloy_consensus::ReceiptEnvelope,
    block_hash: B256,
    block_number: u64,
    block_timestamp: u64,
    tx_hash: B256,
    tx_idx: u64,
    log_index: &mut u64,
) -> alloy_consensus::ReceiptEnvelope<alloy_rpc_types::Log> {
    fn map_rwb(
        rwb: alloy_consensus::ReceiptWithBloom<alloy_consensus::Receipt>,
        block_hash: B256,
        block_number: u64,
        block_timestamp: u64,
        tx_hash: B256,
        tx_idx: u64,
        log_index: &mut u64,
    ) -> alloy_consensus::ReceiptWithBloom<alloy_consensus::Receipt<alloy_rpc_types::Log>> {
        let rpc_logs = rwb
            .receipt
            .logs
            .into_iter()
            .map(|log| {
                let rpc_log = alloy_rpc_types::Log {
                    inner: log,
                    block_hash: Some(block_hash),
                    block_number: Some(block_number),
                    block_timestamp: Some(block_timestamp),
                    transaction_hash: Some(tx_hash),
                    transaction_index: Some(tx_idx),
                    log_index: Some(*log_index),
                    removed: false,
                };
                *log_index += 1;
                rpc_log
            })
            .collect();

        alloy_consensus::ReceiptWithBloom {
            receipt: alloy_consensus::Receipt {
                status: rwb.receipt.status,
                cumulative_gas_used: rwb.receipt.cumulative_gas_used,
                logs: rpc_logs,
            },
            logs_bloom: rwb.logs_bloom,
        }
    }

    match envelope {
        alloy_consensus::ReceiptEnvelope::Legacy(rwb) => {
            alloy_consensus::ReceiptEnvelope::Legacy(map_rwb(
                rwb,
                block_hash,
                block_number,
                block_timestamp,
                tx_hash,
                tx_idx,
                log_index,
            ))
        }
        alloy_consensus::ReceiptEnvelope::Eip2930(rwb) => {
            alloy_consensus::ReceiptEnvelope::Eip2930(map_rwb(
                rwb,
                block_hash,
                block_number,
                block_timestamp,
                tx_hash,
                tx_idx,
                log_index,
            ))
        }
        alloy_consensus::ReceiptEnvelope::Eip1559(rwb) => {
            alloy_consensus::ReceiptEnvelope::Eip1559(map_rwb(
                rwb,
                block_hash,
                block_number,
                block_timestamp,
                tx_hash,
                tx_idx,
                log_index,
            ))
        }
        alloy_consensus::ReceiptEnvelope::Eip4844(rwb) => {
            alloy_consensus::ReceiptEnvelope::Eip4844(map_rwb(
                rwb,
                block_hash,
                block_number,
                block_timestamp,
                tx_hash,
                tx_idx,
                log_index,
            ))
        }
        alloy_consensus::ReceiptEnvelope::Eip7702(rwb) => {
            alloy_consensus::ReceiptEnvelope::Eip7702(map_rwb(
                rwb,
                block_hash,
                block_number,
                block_timestamp,
                tx_hash,
                tx_idx,
                log_index,
            ))
        }
    }
}

fn broadcast_block_updates(
    broadcast_tx: &broadcast::Sender<EventServerEvent>,
    block: &ProposedBlock,
    commit_state: BlockCommitState,
) {
    let block_id = block.block_id;

    let serialized_monad_header = JsonSerialized::new_shared_with_map(
        MonadNotification {
            block_id,
            commit_state,
            data: block.header.clone(),
        },
        |notification| notification.map(JsonSerialized::new_shared),
    );

    let transactions = block
        .transactions
        .iter()
        .zip_eq(block.receipts.iter())
        .map(|(tx, tx_receipt)| {
            let logs = tx_receipt
                .logs()
                .iter()
                .map(|log| {
                    JsonSerialized::new_shared_with_map(
                        MonadNotification {
                            block_id,
                            commit_state,
                            data: log.clone(),
                        },
                        |notification| notification.map(JsonSerialized::new_shared),
                    )
                })
                .collect_vec();

            (
                JsonSerialized::new_shared(tx.clone()),
                JsonSerialized::new_shared(tx_receipt.clone()),
                logs.into_boxed_slice(),
            )
        })
        .collect_vec();

    broadcast_event(
        broadcast_tx,
        EventServerEvent::Block {
            commit_state,
            header: serialized_monad_header,
            transactions: Arc::new(transactions.into_boxed_slice()),
        },
    );
}

fn broadcast_event(broadcast_tx: &broadcast::Sender<EventServerEvent>, event: EventServerEvent) {
    let _ = broadcast_tx.send(event);
}
