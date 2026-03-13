// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::B256;

use crate::types::{ConsensusBody, ConsensusHeader};

/// Commands sent from consensus (via Ledger) to the ExecutionEngine.
#[derive(Clone, Debug)]
pub enum ExecutionCommand {
    Propose {
        block_id: B256,
        header: ConsensusHeader,
        body: ConsensusBody,
    },
    Vote {
        block_number: u64,
        block_id: B256,
    },
    Finalize {
        block_number: u64,
        block_id: B256,
        verified_blocks: Vec<u64>,
    },
    Shutdown,
}
