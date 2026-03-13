// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Complete rewrite of C++ runloop_monad (L459-724).
// Dual-queue mode: catch-up finalized blocks + real-time proposed blocks.

use std::collections::VecDeque;

use alloy_primitives::B256;
use tokio::sync::{broadcast, mpsc};

use crate::block_cache::prune_block_cache;
use crate::block_hash::{BlockHashBufferFinalized, BlockHashChain};
use crate::command::ExecutionCommand;
use crate::events::ExecutionEvent;
use crate::propose::propose_block;
use crate::traits::{BlockExecutor, ExecutionDb, SignatureRecovery};
use crate::types::{BlockCache, ChainConfig, ConsensusBody, ConsensusHeader};
use crate::validation::validate_delayed_execution_results;

struct ToExecute {
    block_id: B256,
    header: ConsensusHeader,
    body: ConsensusBody,
}

struct ToFinalize {
    block_number: u64,
    block_id: B256,
    verified_blocks: Vec<u64>,
    is_vote: bool,
}

/// Main execution runloop.
/// Receives ExecutionCommands via channel, classifies them into to_execute/to_finalize queues,
/// and processes them in order.
pub async fn runloop_monad(
    chain: ChainConfig,
    mut db: Box<dyn ExecutionDb>,
    executor: Box<dyn BlockExecutor>,
    recovery: Box<dyn SignatureRecovery>,
    mut cmd_rx: mpsc::Receiver<ExecutionCommand>,
    event_tx: broadcast::Sender<ExecutionEvent>,
) {
    let finalized_n = db.get_latest_finalized_version();

    // C++ L487-497: init block_hash_buffer from DB
    let mut block_hash_buffer = BlockHashBufferFinalized::new();
    if finalized_n != u64::MAX && finalized_n > 0 {
        block_hash_buffer.init_from_db(&*db, finalized_n);
    }
    let mut block_hash_chain = BlockHashChain::new(block_hash_buffer);

    // C++ L499-549: preload block cache from DB for last N blocks
    let mut block_cache = BlockCache::new();
    // In mock phase: block cache starts empty. When real EVM is integrated,
    // we should preload senders_and_authorities from DB for the last few finalized blocks.
    // This is needed for ChainContext construction in propose_block.

    // C++ L465-467: start_block_num is fixed at runloop start, used for is_first_block
    let start_block_num = if finalized_n == u64::MAX {
        0
    } else {
        finalized_n
    };
    let mut finalized_block_num = start_block_num;

    let mut to_execute: VecDeque<ToExecute> = VecDeque::new();
    let mut to_finalize: VecDeque<ToFinalize> = VecDeque::new();

    loop {
        to_execute.clear();
        to_finalize.clear();

        // Drain all available commands from the channel
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

        // Process to_execute queue
        for item in to_execute.drain(..) {
            let block_number = item.header.execution_inputs.number;

            db.update_voted_metadata(item.header.seqno - 1, item.header.parent_id());

            let block_hash_buffer_ref = block_hash_chain.find_chain(&item.header.parent_id());
            let valid = validate_delayed_execution_results(
                &block_hash_buffer_ref,
                &item.header.delayed_execution_results,
            );
            if !valid {
                tracing::error!(
                    block_number,
                    "delayed execution results validation failed, skipping block"
                );
                continue;
            }

            // C++ L451: is_first_block uses fixed start_block_num, not current finalized
            let is_first_block = block_number == start_block_num;
            match propose_block(
                item.block_id,
                &item.header,
                item.body,
                &mut block_hash_chain,
                &chain,
                &mut *db,
                &*executor,
                &*recovery,
                is_first_block,
                &mut block_cache,
            ) {
                Ok(output) => {
                    db.update_proposed_metadata(item.header.seqno, item.block_id);

                    let _ = event_tx.send(ExecutionEvent::BlockProposed {
                        block_number,
                        block_id: item.block_id,
                        parent_id: item.header.parent_id(),
                        header: output.eth_header.clone(),
                        transactions: output.transactions,
                        senders: output.senders,
                        receipts: output.receipts,
                        eth_block_hash: output.eth_block_hash,
                    });

                    tracing::info!(
                        block_number,
                        block_id = %item.block_id,
                        gas_used = output.eth_header.gas_used,
                        "block executed successfully"
                    );
                }
                Err(e) => {
                    // C++ uses BOOST_OUTCOME_TRY which propagates errors and terminates the runloop.
                    tracing::error!(
                        block_number,
                        block_id = %item.block_id,
                        error = %e,
                        "block execution failed, terminating runloop"
                    );
                    return;
                }
            }
        }

        // Process to_finalize queue
        for item in to_finalize.drain(..) {
            if item.is_vote {
                // Vote: emit BlockVoted, then finalize
                let _ = event_tx.send(ExecutionEvent::BlockVoted {
                    block_number: item.block_number,
                    block_id: item.block_id,
                });
            }

            tracing::info!(
                block_number = item.block_number,
                block_id = %item.block_id,
                is_vote = item.is_vote,
                "processing finalization"
            );
            db.finalize(item.block_number, item.block_id);
            block_hash_chain.finalize(item.block_id);

            let _ = event_tx.send(ExecutionEvent::BlockFinalized {
                block_number: item.block_number,
                block_id: item.block_id,
            });

            finalized_block_num = item.block_number;

            if let Some(&last_verified) = item.verified_blocks.last() {
                if last_verified != u64::MAX {
                    db.update_verified_block(last_verified);
                    let _ = event_tx.send(ExecutionEvent::BlockVerified {
                        block_number: last_verified,
                    });
                }
            }
        }

        // Prune block cache
        prune_block_cache(&mut block_cache, finalized_block_num);
    }

    tracing::info!("runloop exiting");
}

fn classify_command(
    cmd: ExecutionCommand,
    db: &dyn ExecutionDb,
    to_execute: &mut VecDeque<ToExecute>,
    to_finalize: &mut VecDeque<ToFinalize>,
) {
    match cmd {
        ExecutionCommand::Propose {
            block_id,
            header,
            body,
        } => {
            if !db.has_executed(&block_id, header.seqno) {
                to_execute.push_back(ToExecute {
                    block_id,
                    header,
                    body,
                });
            }
        }
        ExecutionCommand::Vote {
            block_number,
            block_id,
        } => {
            to_finalize.push_back(ToFinalize {
                block_number,
                block_id,
                verified_blocks: Vec::new(),
                is_vote: true,
            });
        }
        ExecutionCommand::Finalize {
            block_number,
            block_id,
            verified_blocks,
        } => {
            to_finalize.push_back(ToFinalize {
                block_number,
                block_id,
                verified_blocks,
                is_vote: false,
            });
        }
        ExecutionCommand::Shutdown => {}
    }
}
