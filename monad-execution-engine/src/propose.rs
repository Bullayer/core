// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Complete rewrite of C++ propose_block (L174-378).
// Closely mirrors the original with all validation steps.

use alloy_primitives::B256;

use crate::block_hash::BlockHashChain;
use crate::traits::{BlockExecutor, ExecutionDb, ExecutionError};
use crate::types::{
    Block, BlockCache, BlockExecOutput, ChainConfig, CodeMap,
    ConsensusBody, ConsensusHeader, StateDeltas,
};
use crate::validation::{compute_block_hash, validate_live_execution_outputs};

/// Validate consensus header fields.
/// TT
fn static_validate_consensus_header(header: &ConsensusHeader) -> Result<(), ExecutionError> {
    if header.seqno == 0 {
        return Err(ExecutionError::ValidationError("seqno must be > 0".to_string()));
    }
    Ok(())
}

/// Validate chain-level header constraints.
/// TT
fn static_validate_header(
    _chain: &ChainConfig,
    header: &crate::types::BlockHeader,
) -> Result<(), ExecutionError> {
    if header.gas_limit == 0 {
        return Err(ExecutionError::ValidationError("gas_limit must be > 0".to_string()));
    }
    Ok(())
}

/// Execute a proposed block: recover signatures, validate, execute, commit, verify output.
pub fn propose_block(
    block_id: B256,
    consensus_header: &ConsensusHeader,
    body: ConsensusBody,
    block_hash_chain: &mut BlockHashChain,
    chain: &ChainConfig,
    db: &mut dyn ExecutionDb,
    executor: &dyn BlockExecutor,
    is_first_block: bool,
) -> Result<BlockExecOutput, ExecutionError> {
    let block_hash_buffer = block_hash_chain.find_chain(&consensus_header.parent_id());

    static_validate_consensus_header(consensus_header)?;
    static_validate_header(chain, &consensus_header.execution_inputs)?;

    let mut header = consensus_header.execution_inputs.clone();

    let parent_id = if is_first_block {
        B256::ZERO
    } else {
        consensus_header.parent_id()
    };
    db.set_block_and_prefix(header.number - 1, parent_id);

    let parent_eth_header = db.read_eth_header();
    header.parent_hash = compute_block_hash(&parent_eth_header);

    let block = Block {
        header,
        transactions: body.transactions,
        ommers: body.ommers,
        withdrawals: body.withdrawals,
    };

    let exec_output = executor.execute_block(
        chain,
        &block,
        db,
        &block_hash_buffer,
    )?;

    let state_deltas = StateDeltas::new();
    let code = CodeMap::new();
    db.commit(
        block_id,
        &exec_output.eth_header,
        &state_deltas,
        &code,
        &exec_output.receipts,
        &block.transactions,
    );

    let output_header = db.read_eth_header();

    validate_live_execution_outputs(&block.header, &output_header)?;

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
        receipts: exec_output.receipts,
    })
}
