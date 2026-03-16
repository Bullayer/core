// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Dual-queue mode: catch-up finalized blocks + real-time proposed blocks.

use std::collections::VecDeque;

use monad_consensus_types::block::ConsensusFullBlock;
use monad_crypto::certificate_signature::{CertificateSignaturePubKey, CertificateSignatureRecoverable};
use monad_eth_types::EthExecutionProtocol;
use monad_types::{BlockId, FinalizedHeader, SeqNum};
use monad_validator::signature_collection::SignatureCollection;
use tokio::sync::{broadcast, mpsc};

use crate::block_hash::{BlockHashBufferFinalized, BlockHashChain};
use crate::command::ExecutionCommand;
use crate::events::ExecutionEvent;
use crate::propose::propose_block;
use crate::traits::{BlockExecutor, ExecutionDb};
use crate::validation::validate_delayed_execution_results;

struct ToExecute<ST, SCT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    block_id: BlockId,
    block: ConsensusFullBlock<ST, SCT, EthExecutionProtocol>,
}

struct ToFinalize {
    seq_num: SeqNum,
    block_id: BlockId,
    verified_blocks: Vec<SeqNum>,
}

pub async fn runloop_monad<ST, SCT>(
    mut db: Box<dyn ExecutionDb>,
    executor: Box<dyn BlockExecutor>,
    mut cmd_rx: mpsc::UnboundedReceiver<ExecutionCommand<ST, SCT>>,
    event_tx: broadcast::Sender<ExecutionEvent>,
)
where
    ST: CertificateSignatureRecoverable + 'static,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>> + 'static,
{
    let finalized_n = db.get_latest_finalized_version();

    let mut block_hash_buffer = BlockHashBufferFinalized::new();
    if finalized_n != SeqNum::MAX && finalized_n > SeqNum(0) {
        block_hash_buffer.init_from_db(&*db, finalized_n);
    }
    let mut block_hash_chain = BlockHashChain::new(block_hash_buffer);

    let start_seq_num = if finalized_n == SeqNum::MAX { SeqNum(0) } else { finalized_n };
    let mut last_finalized_seq_num = start_seq_num;

    let mut to_execute: VecDeque<ToExecute<ST, SCT>> = VecDeque::new();
    let mut to_finalize: VecDeque<ToFinalize> = VecDeque::new();

    loop {
        to_execute.clear();
        to_finalize.clear();

        let first_cmd = cmd_rx.recv().await;
        match first_cmd {
            None => break,
            Some(ExecutionCommand::Shutdown) => break,
            Some(cmd) => classify_command(cmd, &*db, &mut to_execute, &mut to_finalize),
        }

        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                ExecutionCommand::Shutdown => {
                    tracing::info!("runloop received shutdown");
                    return;
                }
                cmd => classify_command(cmd, &*db, &mut to_execute, &mut to_finalize),
            }
        }

        if to_execute.is_empty() && to_finalize.is_empty() {
            continue;
        }

        for item in to_execute.drain(..) {
            let seq_num = item.block.get_seq_num();
            let parent_id = item.block.get_parent_id();

            db.update_voted_metadata(seq_num - SeqNum(1), parent_id);

            let voted_seq = seq_num - SeqNum(1);
            if voted_seq > last_finalized_seq_num && voted_seq != SeqNum::MAX {
                let _ = event_tx.send(ExecutionEvent::BlockVoted {
                    seq_num: voted_seq,
                    block_id: parent_id,
                });
            }

            let block_hash_buffer_ref = block_hash_chain.find_chain(&parent_id);
            if !validate_delayed_execution_results(
                &block_hash_buffer_ref,
                item.block.get_execution_results()
            ) {
                tracing::error!(
                    seq_num = seq_num.0,
                    "delayed execution results validation failed, skipping block"
                );
                continue;
            }

            match propose_block(
                &item.block,
                &mut block_hash_chain,
                &mut *db,
                &*executor,
                seq_num == start_seq_num,
            ) {
                Ok(output) => {
                    db.update_proposed_metadata(seq_num, item.block_id);

                    let _ = event_tx.send(ExecutionEvent::BlockProposed {
                        seq_num,
                        block_id: item.block_id,
                        parent_id,
                        header: output.eth_header.clone(),
                        transactions: output.transactions,
                        receipts: output.receipts,
                        eth_block_hash: output.eth_block_hash,
                    });

                    tracing::info!(
                        seq_num = seq_num.0,
                        block_id = ?item.block_id,
                        gas_used = output.eth_header.gas_used,
                        "block executed successfully"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        seq_num = seq_num.0,
                        block_id = ?item.block_id,
                        error = %e,
                        "block execution failed, terminating runloop"
                    );
                    return;
                }
            }
        }

        for item in to_finalize.drain(..) {
            tracing::info!(
                seq_num = item.seq_num.0,
                block_id = ?item.block_id,
                "processing finalization"
            );
            db.finalize(item.seq_num, item.block_id);
            block_hash_chain.finalize(item.block_id);
            last_finalized_seq_num = item.seq_num;

            let _ = event_tx.send(ExecutionEvent::BlockFinalized {
                seq_num: item.seq_num,
                block_id: item.block_id,
            });

            if let Some(&last_verified) = item.verified_blocks.last() {
                if last_verified != SeqNum::MAX {
                    db.update_verified_block(last_verified);
                    let _ = event_tx.send(ExecutionEvent::BlockVerified {
                        seq_num: last_verified,
                    });
                }
            }
        }
    }

    tracing::warn!("runloop exiting");
}

fn classify_command<ST, SCT>(
    cmd: ExecutionCommand<ST, SCT>,
    db: &dyn ExecutionDb,
    to_execute: &mut VecDeque<ToExecute<ST, SCT>>,
    to_finalize: &mut VecDeque<ToFinalize>,
)
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    match cmd {
        ExecutionCommand::Propose {
            block_id,
            block,
        } => {
            if !db.has_executed(&block_id, block.get_seq_num()) {
                to_execute.push_back(ToExecute {
                    block_id,
                    block,
                });
            }
            // block.get_seq_num() = last_finalized_seq_num + 1
        }
        ExecutionCommand::Finalize {
            seq_num,
            block_id,
            block,
        } => {
            to_finalize.push_back(ToFinalize {
                seq_num,
                block_id,
                verified_blocks: block.get_execution_results().iter()
                    .map(|h| h.seq_num()).collect(),
            });
            if !db.has_executed(&block_id, seq_num) {
                to_execute.push_back(ToExecute {
                    block_id,
                    block,
                });
            }
        }
        ExecutionCommand::Shutdown => {}
    }
}
