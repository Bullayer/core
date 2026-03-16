// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_consensus::Header;
use alloy_primitives::B256;
use monad_eth_types::{EthHeader, ProposedEthHeader};
use monad_types::SeqNum;

use crate::traits::{BlockHashBuffer, ExecutionError};

pub fn validate_delayed_execution_results(
    block_hash_buffer: &dyn BlockHashBuffer,
    execution_results: &[EthHeader],
) -> bool {
    if execution_results.is_empty() {
        return true;
    }

    let mut expected_seq_num = SeqNum(execution_results[0].0.number);
    for result in execution_results {
        let header = &result.0;
        if expected_seq_num != SeqNum(header.number) {
            tracing::error!(
                expected = expected_seq_num.0,
                got = header.number,
                "validated blocks not increasing"
            );
            return false;
        }

        let block_hash = compute_block_hash(header);
        let stored_hash = block_hash_buffer.get(SeqNum(header.number));
        if block_hash != stored_hash {
            tracing::error!(
                seq_num = header.number,
                computed = %block_hash,
                stored = %stored_hash,
                "delayed execution result mismatch"
            );
            return false;
        }
        expected_seq_num = expected_seq_num + SeqNum(1);
    }
    true
}

pub fn validate_live_execution_outputs(
    input: &ProposedEthHeader,
    output: &Header,
) -> Result<(), ExecutionError> {
    if B256::from(input.ommers_hash) != output.ommers_hash {
        return Err(ExecutionError::WrongOmmersHash);
    }
    if B256::from(input.transactions_root) != output.transactions_root {
        return Err(ExecutionError::WrongMerkleRoot);
    }
    if Some(B256::from(input.withdrawals_root)) != output.withdrawals_root {
        return Err(ExecutionError::WrongMerkleRoot);
    }
    if output.gas_used > output.gas_limit {
        return Err(ExecutionError::GasAboveLimit);
    }
    Ok(())
}

pub fn compute_block_hash(header: &Header) -> B256 {
    header.hash_slow()
    // Sealed::new(header).hash()
}
