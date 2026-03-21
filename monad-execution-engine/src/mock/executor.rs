// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_consensus::Header;
use alloy_primitives::{B256, Bytes, U256};
use revm::database::states::BundleState;

use crate::traits::{BlockExecutor, BlockHashBuffer, ExecutionDb, ExecutionError};
use crate::types::{Block, BlockExecOutput};
use crate::validation::compute_block_hash;

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
        block: &Block,
        _db: &mut dyn ExecutionDb,
        _block_hash_buffer: &dyn BlockHashBuffer,
    ) -> Result<BlockExecOutput, ExecutionError> {
        let proposed = &block.header;
        let mut output_header = Header {
            number: proposed.number,
            gas_limit: proposed.gas_limit,
            gas_used: block.transactions.len() as u64 * 21000,
            timestamp: proposed.timestamp,
            beneficiary: proposed.beneficiary,
            difficulty: U256::from(proposed.difficulty),
            ommers_hash: B256::from(proposed.ommers_hash),
            transactions_root: B256::from(proposed.transactions_root),
            extra_data: Bytes::copy_from_slice(&proposed.extra_data),
            mix_hash: B256::from(proposed.mix_hash),
            nonce: proposed.nonce.into(),
            base_fee_per_gas: Some(proposed.base_fee_per_gas),
            withdrawals_root: Some(B256::from(proposed.withdrawals_root)),
            blob_gas_used: Some(proposed.blob_gas_used),
            excess_blob_gas: Some(proposed.excess_blob_gas),
            parent_beacon_block_root: Some(B256::from(proposed.parent_beacon_block_root)),
            ..Default::default()
        };
        output_header.state_root = B256::from([0xAA; 32]);
        output_header.receipts_root = B256::from([0xBB; 32]);

        let block_hash = compute_block_hash(&output_header);

        Ok(BlockExecOutput {
            eth_header: output_header,
            eth_block_hash: block_hash,
            transactions: Vec::new(),
            receipts: Vec::new(),
            bundle: BundleState::default(),
        })
    }
}
