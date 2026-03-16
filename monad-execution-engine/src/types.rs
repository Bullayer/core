// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use std::collections::HashMap;

use alloy_consensus::{Header, ReceiptEnvelope, TxEnvelope};
use alloy_primitives::{Address, B256};
use monad_eth_types::{EthAccount, Ommer, ProposedEthHeader, Withdrawal};

#[derive(Clone, Debug)]
pub struct Transaction {
    pub hash: B256,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct AccountDelta {
    pub old_account: Option<EthAccount>,
    pub new_account: Option<EthAccount>,
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
    pub header: ProposedEthHeader,
    pub transactions: Vec<TxEnvelope>,
    pub ommers: Vec<Ommer>,
    pub withdrawals: Vec<Withdrawal>,
}

#[derive(Clone, Debug)]
pub struct BlockExecOutput {
    pub eth_header: Header,
    pub eth_block_hash: B256,
    pub transactions: Vec<TxEnvelope>,
    pub receipts: Vec<ReceiptEnvelope>,
}

#[derive(Clone, Debug, Default)]
pub struct BlockMetrics {
    pub num_retries: u64,
    pub tx_exec_time_us: u64,
}
