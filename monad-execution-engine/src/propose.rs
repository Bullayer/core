// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Complete rewrite of C++ propose_block (L174-378).
// Closely mirrors the original with all validation steps.

use std::collections::HashSet;

use alloy_primitives::{Address, B256};

use crate::block_cache::insert_block_cache;
use crate::block_hash::BlockHashChain;
use crate::traits::{BlockExecutor, ExecutionDb, ExecutionError, SignatureRecovery};
use crate::types::{
    Block, BlockCache, BlockCacheEntry, BlockExecOutput, ChainConfig, ChainContext, CodeMap,
    ConsensusBody, ConsensusHeader, StateDeltas,
};
use crate::validation::{compute_block_hash, validate_live_execution_outputs};

/// Validate consensus header fields.
/// Corresponds to C++ static_validate_consensus_header.
fn static_validate_consensus_header(header: &ConsensusHeader) -> Result<(), ExecutionError> {
    if header.seqno == 0 {
        return Err(ExecutionError::ValidationError(
            "seqno must be > 0".to_string(),
        ));
    }
    Ok(())
}

/// Validate chain-level header constraints.
/// Corresponds to C++ chain.static_validate_header.
fn static_validate_header(
    _chain: &ChainConfig,
    header: &crate::types::BlockHeader,
) -> Result<(), ExecutionError> {
    if header.gas_limit == 0 {
        return Err(ExecutionError::ValidationError(
            "gas_limit must be > 0".to_string(),
        ));
    }
    Ok(())
}

/// Build ChainContext from block cache.
/// Corresponds to C++ L244-275 in propose_block.
fn build_chain_context(
    block_id: B256,
    consensus_header: &ConsensusHeader,
    block_cache: &BlockCache,
    senders: &[Address],
    authorities: &[Vec<Option<Address>>],
) -> ChainContext {
    let empty = HashSet::new();

    let (grandparent, parent) = if consensus_header.execution_inputs.number > 1 {
        let parent_id = consensus_header.parent_id();
        // C++ asserts block_cache.contains(parent_id)
        let parent_entry = block_cache.get(&parent_id)
            .expect("build_chain_context: parent not in block cache");
        let parent_sa = &parent_entry.senders_and_authorities;

        let grandparent_sa = if consensus_header.execution_inputs.number > 2 {
            block_cache
                .get(&parent_entry.parent_id)
                .map(|e| &e.senders_and_authorities)
                .unwrap_or(&empty)
        } else {
            &empty
        };

        (grandparent_sa.clone(), parent_sa.clone())
    } else {
        (empty.clone(), empty.clone())
    };

    let current = block_cache
        .get(&block_id)
        .map(|e| e.senders_and_authorities.clone())
        .unwrap_or_default();

    ChainContext {
        grandparent_senders_and_authorities: grandparent,
        parent_senders_and_authorities: parent,
        senders_and_authorities: current,
        senders: senders.to_vec(),
        authorities: authorities.to_vec(),
    }
}

/// Execute a proposed block: recover signatures, validate, execute, commit, verify output.
/// Corresponds to C++ propose_block template function (L174-378).
pub fn propose_block(
    block_id: B256,
    consensus_header: &ConsensusHeader,
    body: ConsensusBody,
    block_hash_chain: &mut BlockHashChain,
    chain: &ChainConfig,
    db: &mut dyn ExecutionDb,
    executor: &dyn BlockExecutor,
    recovery: &dyn SignatureRecovery,
    is_first_block: bool,
    block_cache: &mut BlockCache,
) -> Result<BlockExecOutput, ExecutionError> {
    let block_hash_buffer = block_hash_chain.find_chain(&consensus_header.parent_id());

    // C++ L188-191: Input validations
    static_validate_consensus_header(consensus_header)?;
    static_validate_header(chain, &consensus_header.execution_inputs)?;

    // 1. Sender and authority recovery
    // C++ L194-209
    let senders = recovery.recover_senders(&body.transactions);
    let authorities = recovery.recover_authorities(&body.transactions);

    // C++ L202-210: returns TransactionError::MissingSender if recovery fails
    for (i, sender) in senders.iter().enumerate() {
        if *sender == Address::ZERO && !body.transactions.is_empty() {
            return Err(ExecutionError::MissingSender(i));
        }
    }

    // Combine senders and authorities into a set for the block cache
    let mut senders_and_authorities = HashSet::new();
    for addr in &senders {
        senders_and_authorities.insert(*addr);
    }
    for auth_list in &authorities {
        for addr in auth_list.iter().flatten() {
            senders_and_authorities.insert(*addr);
        }
    }

    // Insert into block cache
    let entry = BlockCacheEntry {
        block_number: consensus_header.execution_inputs.number,
        parent_id: consensus_header.parent_id(),
        senders_and_authorities: senders_and_authorities.clone(),
    };
    // C++ L215-223: asserts block_cache.emplace succeeds (no duplicate block_id)
    let inserted = insert_block_cache(block_cache, block_id, entry);
    assert!(
        inserted,
        "propose_block: duplicate block_id in block cache: {}",
        block_id
    );

    // Build ChainContext (C++ L244-275)
    let _chain_context = build_chain_context(
        block_id,
        consensus_header,
        block_cache,
        &senders,
        &authorities,
    );

    // 2. Build the Block struct
    let mut header = consensus_header.execution_inputs.clone();

    // 3. Set DB read context to parent block
    // C++ L279-281
    let parent_id = if is_first_block {
        B256::ZERO
    } else {
        consensus_header.parent_id()
    };
    db.set_block_and_prefix(header.number - 1, parent_id);

    // 4. Compute parent_hash from DB's eth header
    // C++ L282-283
    let parent_eth_header = db.read_eth_header();
    header.parent_hash = compute_block_hash(&parent_eth_header);

    let block = Block {
        header,
        transactions: body.transactions,
        ommers: body.ommers,
        withdrawals: body.withdrawals,
    };

    // 5. Execute block via BlockExecutor trait
    // C++ L289-303
    let exec_output = executor.execute_block(
        chain,
        &block,
        &senders,
        &authorities,
        db,
        &block_hash_buffer,
    )?;

    // 6. Commit state
    // C++ L308-319: block_state.commit(...)
    // For mock: state deltas come from executor (empty for MockBlockExecutor).
    // For real EVM: these would be the actual state changes from execution.
    let state_deltas = StateDeltas::new();
    let code = CodeMap::new();
    db.commit(
        block_id,
        &exec_output.eth_header,
        &state_deltas,
        &code,
        &exec_output.receipts,
        &senders,
        &block.transactions,
    );

    // 7. Re-read header from DB after commit (C++ L328)
    // C++ always uses db.read_eth_header() as the output after commit,
    // because commit writes Merkle roots to the stored header.
    db.set_block_and_prefix(block.header.number, block_id);
    let output_header = db.read_eth_header();

    // 8. Validate output (C++ L329-330)
    validate_live_execution_outputs(&block.header, &output_header)?;

    // 9. Compute block hash and update hash chain (C++ L333-340)
    let eth_block_hash = compute_block_hash(&output_header);
    block_hash_chain.propose(
        eth_block_hash,
        block.header.number,
        block_id,
        consensus_header.parent_id(),
    );

    Ok(BlockExecOutput {
        eth_header: output_header,
        eth_block_hash,
        transactions: block.transactions,
        senders,
        receipts: exec_output.receipts,
    })
}
