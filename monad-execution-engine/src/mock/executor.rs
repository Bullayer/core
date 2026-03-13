// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::{Address, B256};

use crate::traits::{BlockExecutor, BlockHashBuffer, ExecutionDb, ExecutionError};
use crate::types::{Block, BlockExecOutput, ChainConfig};
use crate::validation::compute_block_hash;

/// Mock block executor that returns deterministic results without running EVM.
pub struct MockBlockExecutor;

impl MockBlockExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockBlockExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockExecutor for MockBlockExecutor {
    fn execute_block(
        &self,
        _chain: &ChainConfig,
        block: &Block,
        _senders: &[Address],
        _authorities: &[Vec<Option<Address>>],
        _db: &mut dyn ExecutionDb,
        _block_hash_buffer: &dyn BlockHashBuffer,
    ) -> Result<BlockExecOutput, ExecutionError> {
        let mut output_header = block.header.clone();
        // Mock: set state_root and receipts_root to deterministic values
        output_header.state_root = B256::from([0xAA; 32]);
        output_header.receipts_root = B256::from([0xBB; 32]);
        output_header.gas_used = block.transactions.len() as u64 * 21000;

        let block_hash = compute_block_hash(&output_header);

        Ok(BlockExecOutput {
            eth_header: output_header,
            eth_block_hash: block_hash,
            transactions: Vec::new(),
            senders: Vec::new(),
            receipts: Vec::new(),
        })
    }
}
