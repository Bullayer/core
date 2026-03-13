// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};

use crate::types::{BlockHeader, Receipt, Transaction};

/// Block commit states from MonadBFT.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockCommitState {
    Proposed,
    Voted,
    Finalized,
    Verified,
}

/// Execution events emitted by the execution engine.
/// Consumed by EventServer (monad-rpc) and other subscribers.
#[derive(Clone, Debug)]
pub enum ExecutionEvent {
    /// Block execution started (proposed).
    BlockProposed {
        block_number: u64,
        block_id: B256,
        parent_id: B256,
        header: BlockHeader,
        transactions: Vec<Transaction>,
        senders: Vec<Address>,
        receipts: Vec<Receipt>,
        eth_block_hash: B256,
    },

    /// Block has been voted on (finalized in next round).
    BlockVoted {
        block_number: u64,
        block_id: B256,
    },

    /// Block finalized.
    BlockFinalized {
        block_number: u64,
        block_id: B256,
    },

    /// Block execution verified.
    BlockVerified {
        block_number: u64,
    },

    /// Gap in event stream (reset).
    Gap,
}
