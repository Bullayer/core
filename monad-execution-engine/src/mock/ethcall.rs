// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::Address;
use async_trait::async_trait;
use monad_types::{BlockId, SeqNum};

use crate::ethcall::{
    CallResult, EthCallHandler, MonadTracer, StateOverrideSet, SuccessCallResult,
};

pub struct MockEthCallHandler;

impl MockEthCallHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockEthCallHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EthCallHandler for MockEthCallHandler {
    async fn eth_call(
        &self,
        _chain_id: u64,
        _transaction: Vec<u8>,
        _block_header: Vec<u8>,
        _sender: Address,
        _seq_num: SeqNum,
        _block_id: Option<BlockId>,
        _state_override_set: &StateOverrideSet,
        _tracer: MonadTracer,
        _gas_specified: bool,
    ) -> CallResult {
        CallResult::Success(SuccessCallResult {
            gas_used: 21000,
            gas_refund: 0,
            output_data: vec![],
        })
    }

    async fn eth_trace_block_or_transaction(
        &self,
        _chain_id: u64,
        _block_header: Vec<u8>,
        _seq_num: SeqNum,
        _block_id: Option<BlockId>,
        _parent_id: Option<BlockId>,
        _grandparent_id: Option<BlockId>,
        _transaction_index: i64,
        _tracer: MonadTracer,
    ) -> CallResult {
        CallResult::Success(SuccessCallResult {
            gas_used: 0,
            gas_refund: 0,
            output_data: vec![],
        })
    }
}
