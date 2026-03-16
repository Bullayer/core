// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use std::collections::HashMap;

use alloy_primitives::{Address, Bytes, B256, U256, U64};
use async_trait::async_trait;
use monad_types::{BlockId, SeqNum};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MonadTracer {
    NoopTracer = 0,
    CallTracer,
    PreStateTracer,
    StateDiffTracer,
    AccessListTracer,
}

#[derive(Clone, Debug, Default)]
pub enum EthCallResult {
    Success,
    OutOfGas,
    #[default]
    OtherError,
}

#[derive(Clone, Debug)]
pub enum CallResult {
    Success(SuccessCallResult),
    Failure(FailureCallResult),
    Revert(RevertCallResult),
}

#[derive(Clone, Debug, Default)]
pub struct SuccessCallResult {
    pub gas_used: u64,
    pub gas_refund: u64,
    pub output_data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct FailureCallResult {
    pub error_code: EthCallResult,
    pub message: String,
    pub data: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RevertCallResult {
    pub trace: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageOverride {
    State(HashMap<B256, B256>),
    StateDiff(HashMap<B256, B256>),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateOverrideObject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<U256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<U64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Bytes>,
    #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
    pub storage_override: Option<StorageOverride>,
}

pub type StateOverrideSet = HashMap<Address, StateOverrideObject>;

/// Abstraction over EVM execution for RPC eth_call, eth_estimateGas, debug_traceCall, etc.
/// Mock version returns fixed results; future version wraps real EVM (revm or evmone).
#[async_trait]
pub trait EthCallHandler: Send + Sync {
    async fn eth_call(
        &self,
        chain_id: u64,
        transaction: Vec<u8>,
        block_header: Vec<u8>,
        sender: Address,
        seq_num: SeqNum,
        block_id: Option<BlockId>,
        state_override_set: &StateOverrideSet,
        tracer: MonadTracer,
        gas_specified: bool,
    ) -> CallResult;

    async fn eth_trace_block_or_transaction(
        &self,
        chain_id: u64,
        block_header: Vec<u8>,
        seq_num: SeqNum,
        block_id: Option<BlockId>,
        parent_id: Option<BlockId>,
        grandparent_id: Option<BlockId>,
        transaction_index: i64,
        tracer: MonadTracer,
    ) -> CallResult;
}
