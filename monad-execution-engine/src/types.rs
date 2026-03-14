// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use std::collections::{HashMap, HashSet};

use alloy_primitives::{Address, B256, U256};

#[derive(Clone, Debug)]
pub struct BlockHeader {
    pub parent_hash: B256,
    pub ommers_hash: B256,
    pub beneficiary: Address,
    pub state_root: B256,
    pub transactions_root: B256,
    pub receipts_root: B256,
    /// Fixed 256 bytes (Bloom filter). C++ type: byte_string_fixed<256>.
    pub logs_bloom: [u8; 256],
    pub difficulty: u64,
    pub number: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub timestamp: u64,
    pub extra_data: Vec<u8>,
    pub mix_hash: B256,
    /// Fixed 8 bytes. C++ type: byte_string_fixed<8>. RLP-encoded as byte string, not integer.
    pub nonce: [u8; 8],
    pub base_fee_per_gas: Option<u64>,
    pub withdrawals_root: Option<B256>,
    pub blob_gas_used: Option<u64>,
    pub excess_blob_gas: Option<u64>,
    pub parent_beacon_block_root: Option<B256>,
    pub requests_hash: Option<B256>,
}

impl Default for BlockHeader {
    fn default() -> Self {
        Self {
            parent_hash: B256::ZERO,
            ommers_hash: B256::ZERO,
            beneficiary: Address::ZERO,
            state_root: B256::ZERO,
            transactions_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: [0u8; 256],
            difficulty: 0,
            number: 0,
            gas_limit: 0,
            gas_used: 0,
            timestamp: 0,
            extra_data: Vec::new(),
            mix_hash: B256::ZERO,
            nonce: [0u8; 8],
            base_fee_per_gas: None,
            withdrawals_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
            requests_hash: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Account {
    pub nonce: u64,
    pub balance: U256,
    pub code_hash: B256,
    pub incarnation: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub status: bool,
    pub cumulative_gas_used: u64,
    pub logs: Vec<Log>,
}

#[derive(Clone, Debug, Default)]
pub struct Log {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Transaction {
    pub hash: B256,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct AccountDelta {
    pub old_account: Option<Account>,
    pub new_account: Option<Account>,
    pub storage: HashMap<B256, StorageDelta>,
}

#[derive(Clone, Debug, Default)]
pub struct StorageDelta {
    pub old_value: B256,
    pub new_value: B256,
}

pub type StateDeltas = HashMap<Address, AccountDelta>;
pub type CodeMap = HashMap<B256, Vec<u8>>;

#[derive(Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub ommers: Vec<BlockHeader>,
    pub withdrawals: Vec<Withdrawal>,
}

#[derive(Clone, Debug, Default)]
pub struct Withdrawal {
    pub index: u64,
    pub validator_index: u64,
    pub address: Address,
    pub amount: u64,
}

#[derive(Clone, Debug)]
pub struct BlockExecOutput {
    pub eth_header: BlockHeader,
    pub eth_block_hash: B256,
    pub transactions: Vec<Transaction>,
    pub receipts: Vec<Receipt>,
}

#[derive(Clone, Debug, Default)]
pub struct BlockMetrics {
    pub num_retries: u64,
    pub tx_exec_time_us: u64,
}

#[derive(Clone, Debug)]
pub struct ConsensusHeader {
    pub seqno: u64,
    pub parent_id: B256,
    pub block_body_id: B256,
    pub block_round: u64,
    pub epoch: u64,
    pub timestamp_ns: u128,
    pub author: [u8; 33],
    pub execution_inputs: BlockHeader,
    pub delayed_execution_results: Vec<BlockHeader>,
    pub base_fee_trend: u64,
    pub base_fee_moment: u64,
}

impl ConsensusHeader {
    pub fn parent_id(&self) -> B256 {
        self.parent_id
    }
}

#[derive(Clone, Debug)]
pub struct ConsensusBody {
    pub transactions: Vec<Transaction>,
    pub ommers: Vec<BlockHeader>,
    pub withdrawals: Vec<Withdrawal>,
}

#[derive(Clone, Debug, Default)]
pub struct BlockCacheEntry {
    pub block_number: u64,
    pub parent_id: B256,
    pub senders_and_authorities: HashSet<Address>,
}

pub type BlockCache = HashMap<B256, BlockCacheEntry>;

pub struct ChainConfig {
    pub chain_id: U256,
}
