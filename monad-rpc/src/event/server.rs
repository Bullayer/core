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
    block_id: B256,
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
            let mut proposed_blocks: HashMap<B256, Arc<ProposedBlock>> = HashMap::new();

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
                            warn!(%block_id, "BlockVoted for unknown block");
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
                            warn!(%block_id, "BlockFinalized for unknown block");
                        }
                    }

                    ExecutionEvent::BlockVerified { block_number } => {
                        debug!(block_number, "BlockVerified event (no broadcast)");
                    }

                    ExecutionEvent::Gap => {
                        proposed_blocks.clear();
                        broadcast_event(&tx, EventServerEvent::Gap);
                    }
                }
            }
        });

        EventServerClient::new(broadcast_tx, handle)
    }
}

fn build_proposed_block(
    block_id: B256,
    header: &monad_execution_engine::types::BlockHeader,
    transactions: &[monad_execution_engine::types::Transaction],
    receipts: &[monad_execution_engine::types::Receipt],
    eth_block_hash: B256,
) -> ProposedBlock {
    let alloy_header = convert_header(header, eth_block_hash);
    let base_fee = alloy_header.inner.base_fee_per_gas;

    let alloy_txs = convert_transactions(
        transactions,
        eth_block_hash,
        header.number,
        base_fee,
    );

    let alloy_receipts = convert_receipts(
        transactions,
        receipts,
        eth_block_hash,
        header.number,
        header.timestamp,
        base_fee,
    );

    ProposedBlock {
        block_id,
        header: alloy_header,
        transactions: alloy_txs,
        receipts: alloy_receipts,
    }
}

fn convert_header(
    header: &monad_execution_engine::types::BlockHeader,
    eth_block_hash: B256,
) -> alloy_rpc_types::Header {
    let consensus_header = alloy_consensus::Header {
        parent_hash: header.parent_hash,
        ommers_hash: header.ommers_hash,
        beneficiary: header.beneficiary,
        state_root: header.state_root,
        transactions_root: header.transactions_root,
        receipts_root: header.receipts_root,
        logs_bloom: alloy_primitives::Bloom::from(header.logs_bloom),
        difficulty: alloy_primitives::U256::from(header.difficulty),
        number: header.number,
        gas_limit: header.gas_limit,
        gas_used: header.gas_used,
        timestamp: header.timestamp,
        extra_data: alloy_primitives::Bytes::copy_from_slice(&header.extra_data),
        mix_hash: header.mix_hash,
        nonce: alloy_primitives::B64::from(header.nonce),
        base_fee_per_gas: header.base_fee_per_gas,
        withdrawals_root: header.withdrawals_root,
        blob_gas_used: header.blob_gas_used.or(Some(0)),
        excess_blob_gas: header.excess_blob_gas.or(Some(0)),
        parent_beacon_block_root: header
            .parent_beacon_block_root
            .or(Some(alloy_primitives::B256::ZERO)),
        requests_hash: header.requests_hash.or(Some(alloy_primitives::B256::ZERO)),
    };

    let size = consensus_header.size();

    alloy_rpc_types::Header {
        hash: eth_block_hash,
        inner: consensus_header,
        total_difficulty: Some(alloy_primitives::U256::ZERO),
        size: Some(alloy_primitives::U256::from(size)),
    }
}

fn convert_transactions(
    transactions: &[monad_execution_engine::types::Transaction],
    block_hash: B256,
    block_number: u64,
    base_fee: Option<u64>,
) -> Vec<alloy_rpc_types::Transaction> {
    transactions
        .iter()
        .enumerate()
        .map(|(tx_idx, tx)| {
            let sender = senders.get(tx_idx).copied().unwrap_or_default();
            let tx_envelope = decode_tx_envelope(&tx.data, tx.hash);
            let effective_gas_price =
                alloy_consensus::Transaction::effective_gas_price(&tx_envelope, base_fee);

            alloy_rpc_types::Transaction {
                inner: alloy_consensus::transaction::Recovered::new_unchecked(
                    tx_envelope, sender,
                ),
                block_hash: Some(block_hash),
                block_number: Some(block_number),
                transaction_index: Some(tx_idx as u64),
                effective_gas_price: Some(effective_gas_price),
            }
        })
        .collect()
}

fn decode_tx_envelope(data: &[u8], tx_hash: B256) -> alloy_consensus::TxEnvelope {
    use alloy_rlp::Decodable;

    if data.is_empty() {
        return build_placeholder_tx_envelope(tx_hash);
    }

    match alloy_consensus::TxEnvelope::decode(&mut &data[..]) {
        Ok(envelope) => envelope,
        Err(_) => build_placeholder_tx_envelope(tx_hash),
    }
}

fn build_placeholder_tx_envelope(tx_hash: B256) -> alloy_consensus::TxEnvelope {
    let tx = alloy_consensus::TxLegacy {
        chain_id: None,
        nonce: 0,
        gas_price: 0,
        gas_limit: 0,
        to: alloy_primitives::TxKind::Create,
        value: alloy_primitives::U256::ZERO,
        input: alloy_primitives::Bytes::new(),
    };

    alloy_consensus::TxEnvelope::Legacy(alloy_consensus::Signed::new_unchecked(
        tx,
        alloy_primitives::Signature::new(
            alloy_primitives::U256::ZERO,
            alloy_primitives::U256::ZERO,
            false,
        ),
        tx_hash,
    ))
}

fn convert_receipts(
    transactions: &[monad_execution_engine::types::Transaction],
    receipts: &[monad_execution_engine::types::Receipt],
    block_hash: B256,
    block_number: u64,
    block_timestamp: u64,
    base_fee: Option<u64>,
) -> Vec<alloy_rpc_types::TransactionReceipt> {
    let mut log_index = 0u64;

    receipts
        .iter()
        .enumerate()
        .map(|(tx_idx, receipt)| {
            let tx = transactions.get(tx_idx);
            let tx_hash = tx.map(|t| t.hash).unwrap_or_default();
            let sender = senders.get(tx_idx).copied().unwrap_or_default();
            let tx_data = tx.map(|t| &t.data[..]).unwrap_or(&[]);

            let logs: Vec<alloy_rpc_types::Log> = receipt
                .logs
                .iter()
                .map(|log| {
                    let alloy_log = alloy_rpc_types::Log {
                        inner: alloy_primitives::Log {
                            address: log.address,
                            data: alloy_primitives::LogData::new_unchecked(
                                log.topics.clone(),
                                alloy_primitives::Bytes::copy_from_slice(&log.data),
                            ),
                        },
                        block_hash: Some(block_hash),
                        block_number: Some(block_number),
                        block_timestamp: Some(block_timestamp),
                        transaction_hash: Some(tx_hash),
                        transaction_index: Some(tx_idx as u64),
                        log_index: Some(log_index),
                        removed: false,
                    };
                    log_index += 1;
                    alloy_log
                })
                .collect();

            let logs_bloom = alloy_primitives::logs_bloom(logs.iter().map(|l| &l.inner));

            let receipt_with_bloom = alloy_consensus::ReceiptWithBloom {
                receipt: alloy_consensus::Receipt {
                    status: alloy_consensus::Eip658Value::Eip658(receipt.status),
                    cumulative_gas_used: receipt.cumulative_gas_used,
                    logs,
                },
                logs_bloom,
            };

            let tx_envelope = decode_tx_envelope(tx_data, tx_hash);

            let receipt_envelope = match &tx_envelope {
                alloy_consensus::TxEnvelope::Eip2930(_) => {
                    alloy_consensus::ReceiptEnvelope::Eip2930(receipt_with_bloom)
                }
                alloy_consensus::TxEnvelope::Eip1559(_) => {
                    alloy_consensus::ReceiptEnvelope::Eip1559(receipt_with_bloom)
                }
                alloy_consensus::TxEnvelope::Eip4844(_) => {
                    alloy_consensus::ReceiptEnvelope::Eip4844(receipt_with_bloom)
                }
                alloy_consensus::TxEnvelope::Eip7702(_) => {
                    alloy_consensus::ReceiptEnvelope::Eip7702(receipt_with_bloom)
                }
                _ => alloy_consensus::ReceiptEnvelope::Legacy(receipt_with_bloom),
            };

            let gas_used = if tx_idx > 0 {
                receipt
                    .cumulative_gas_used
                    .saturating_sub(
                        receipts
                            .get(tx_idx - 1)
                            .map(|r| r.cumulative_gas_used)
                            .unwrap_or(0),
                    )
            } else {
                receipt.cumulative_gas_used
            };

            let effective_gas_price =
                alloy_consensus::Transaction::effective_gas_price(&tx_envelope, base_fee);

            let to = alloy_consensus::Transaction::to(&tx_envelope);
            let (to, contract_address) = match to {
                Some(addr) => (Some(addr), None),
                None => {
                    let nonce = alloy_consensus::Transaction::nonce(&tx_envelope);
                    (None, Some(sender.create(nonce)))
                }
            };

            alloy_rpc_types::TransactionReceipt {
                inner: receipt_envelope,
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

fn broadcast_block_updates(
    broadcast_tx: &broadcast::Sender<EventServerEvent>,
    block: &ProposedBlock,
    commit_state: BlockCommitState,
) {
    let block_id = BlockId(monad_types::Hash(block.block_id.0));

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
