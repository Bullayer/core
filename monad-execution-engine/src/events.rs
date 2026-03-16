// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_consensus::{Header, ReceiptEnvelope, TxEnvelope};
use alloy_primitives::B256;
use monad_types::{BlockId, SeqNum};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockCommitState {
    Proposed,
    Voted,
    Finalized,
    Verified,
}

#[derive(Clone, Debug)]
pub enum ExecutionEvent {
    /// Block execution started (proposed).
    BlockProposed {
        seq_num: SeqNum,
        block_id: BlockId,
        parent_id: BlockId,
        header: Header,
        transactions: Vec<TxEnvelope>,
        receipts: Vec<ReceiptEnvelope>,
        eth_block_hash: B256,
    },

    /// Block has been voted on (finalized in next round).
    BlockVoted {
        seq_num: SeqNum,
        block_id: BlockId,
    },

    /// Block finalized.
    BlockFinalized {
        seq_num: SeqNum,
        block_id: BlockId,
    },

    /// Block execution verified.
    BlockVerified {
        seq_num: SeqNum,
    },
}
