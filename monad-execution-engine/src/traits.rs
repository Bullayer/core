// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::{Address, B256};

use crate::types::{
    Account, Block, BlockExecOutput, BlockHeader, ChainConfig, CodeMap, Receipt, StateDeltas,
    Transaction,
};

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("block validation error: {0}")]
    ValidationError(String),
    #[error("missing sender for transaction {0}")]
    MissingSender(usize),
    #[error("wrong ommers hash")]
    WrongOmmersHash,
    #[error("wrong merkle root")]
    WrongMerkleRoot,
    #[error("gas above limit")]
    GasAboveLimit,
    #[error("database error: {0}")]
    DatabaseError(String),
    #[error("internal error: {0}")]
    InternalError(String),
}

pub trait BlockHashBuffer: Send + Sync {
    fn get(&self, block_number: u64) -> B256;
}

/// Abstraction over the execution database (C++ mpt::Db interface).
pub trait ExecutionDb: Send + Sync {
    fn has_executed(&self, block_id: &B256, seq_num: u64) -> bool;
    fn get_latest_finalized_version(&self) -> u64;

    fn read_account(&self, address: &Address) -> Option<Account>;
    fn read_storage(&self, address: &Address, slot: &B256) -> B256;
    fn read_eth_header(&self) -> BlockHeader;

    fn set_block_and_prefix(&mut self, block_number: u64, block_id: B256);

    fn commit(
        &mut self,
        block_id: B256,
        header: &BlockHeader,
        state_deltas: &StateDeltas,
        code: &CodeMap,
        receipts: &[Receipt],
        transactions: &[Transaction],
    );

    fn finalize(&mut self, block_number: u64, block_id: B256);
    fn update_voted_metadata(&mut self, block_number: u64, block_id: B256);
    fn update_proposed_metadata(&mut self, block_number: u64, block_id: B256);
    fn update_verified_block(&mut self, block_number: u64);
}

/// Abstraction over the EVM block execution.
pub trait BlockExecutor: Send + Sync {
    fn execute_block(
        &self,
        chain: &ChainConfig,
        block: &Block,
        db: &mut dyn ExecutionDb,
        block_hash_buffer: &dyn BlockHashBuffer,
    ) -> Result<BlockExecOutput, ExecutionError>;
}