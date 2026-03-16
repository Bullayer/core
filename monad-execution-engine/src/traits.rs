// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_consensus::{Header, ReceiptEnvelope, TxEnvelope};
use alloy_primitives::{Address, B256};
use monad_types::{BlockId, SeqNum};

use monad_eth_types::EthAccount;

use crate::types::{
    Block, BlockExecOutput, CodeMap, StateDeltas,
};

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("block validation error: {0}")]
    ValidationError(String),
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
    fn get(&self, seq_num: SeqNum) -> B256;
}

pub trait ExecutionDb: Send + Sync {
    fn has_executed(&self, block_id: &BlockId, seq_num: SeqNum) -> bool;
    fn get_latest_finalized_version(&self) -> SeqNum;

    fn read_account(&self, address: &Address) -> Option<EthAccount>;
    fn read_storage(&self, address: &Address, slot: &B256) -> B256;
    fn read_eth_header(&self) -> Header;

    fn set_block_and_prefix(&mut self, seq_num: SeqNum, block_id: BlockId);

    fn commit(
        &mut self,
        block_id: BlockId,
        header: &Header,
        state_deltas: &StateDeltas,
        code: &CodeMap,
        receipts: &[ReceiptEnvelope],
        transactions: &[TxEnvelope],
    );

    fn finalize(&mut self, seq_num: SeqNum, block_id: BlockId);
    fn update_voted_metadata(&mut self, seq_num: SeqNum, block_id: BlockId);
    fn update_proposed_metadata(&mut self, seq_num: SeqNum, block_id: BlockId);
    fn update_verified_block(&mut self, seq_num: SeqNum);
}

pub trait BlockExecutor: Send + Sync {
    fn execute_block(
        &self,
        block: &Block,
        db: &mut dyn ExecutionDb,
        block_hash_buffer: &dyn BlockHashBuffer,
    ) -> Result<BlockExecOutput, ExecutionError>;
}
