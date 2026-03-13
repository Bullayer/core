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
/// Mock version uses HashMap; future version wraps TrieDB.
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
        senders: &[Address],
        transactions: &[Transaction],
    );

    fn finalize(&mut self, block_number: u64, block_id: B256);
    fn update_voted_metadata(&mut self, block_number: u64, block_id: B256);
    fn update_proposed_metadata(&mut self, block_number: u64, block_id: B256);
    fn update_verified_block(&mut self, block_number: u64);
}

/// Abstraction over the EVM block execution.
/// Mock version returns deterministic results; future version wraps real EVM.
pub trait BlockExecutor: Send + Sync {
    fn execute_block(
        &self,
        chain: &ChainConfig,
        block: &Block,
        senders: &[Address],
        authorities: &[Vec<Option<Address>>],
        db: &mut dyn ExecutionDb,
        block_hash_buffer: &dyn BlockHashBuffer,
    ) -> Result<BlockExecOutput, ExecutionError>;
}

/// Abstraction over signature recovery (sender + EIP-7702 authorities).
/// Mock version extracts from transaction directly.
pub trait SignatureRecovery: Send + Sync {
    fn recover_senders(&self, txs: &[Transaction]) -> Vec<Address>;
    fn recover_authorities(&self, txs: &[Transaction]) -> Vec<Vec<Option<Address>>>;
}
