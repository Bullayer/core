// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::B256;
use alloy_rlp::Encodable;
use tiny_keccak::{Hasher, Keccak};

use crate::traits::{BlockHashBuffer, ExecutionError};
use crate::types::BlockHeader;

/// Validate delayed execution results against the block hash buffer.
pub fn validate_delayed_execution_results(
    block_hash_buffer: &dyn BlockHashBuffer,
    execution_results: &[BlockHeader],
) -> bool {
    if execution_results.is_empty() {
        return true;
    }

    let mut expected_block_number = execution_results[0].number;
    for result in execution_results {
        if expected_block_number != result.number {
            tracing::error!(
                expected = expected_block_number,
                got = result.number,
                "validated blocks not increasing"
            );
            return false;
        }

        let block_hash = compute_block_hash(result);
        let stored_hash = block_hash_buffer.get(result.number);
        if block_hash != stored_hash {
            tracing::error!(
                block_number = result.number,
                computed = %block_hash,
                stored = %stored_hash,
                "delayed execution result mismatch"
            );
            return false;
        }
        expected_block_number = result.number + 1;
    }
    true
}

/// Validate that the execution output matches the proposed header.
pub fn validate_live_execution_outputs(
    input: &BlockHeader,
    output: &BlockHeader,
) -> Result<(), ExecutionError> {
    if input.ommers_hash != output.ommers_hash {
        return Err(ExecutionError::WrongOmmersHash);
    }
    if input.transactions_root != output.transactions_root {
        return Err(ExecutionError::WrongMerkleRoot);
    }
    if input.withdrawals_root != output.withdrawals_root {
        return Err(ExecutionError::WrongMerkleRoot);
    }
    if output.gas_used > output.gas_limit {
        return Err(ExecutionError::GasAboveLimit);
    }
    Ok(())
}

/// Compute keccak256 hash of an RLP-encoded block header.
pub fn compute_block_hash(header: &BlockHeader) -> B256 {
    let mut buf = Vec::new();
    rlp_encode_block_header(header, &mut buf);
    keccak256(&buf)
}

pub fn keccak256(data: &[u8]) -> B256 {
    let mut hasher = Keccak::v256();
    let mut output = [0u8; 32];
    hasher.update(data);
    hasher.finalize(&mut output);
    B256::from(output)
}

/// RLP encoding of a block header for hashing.
/// Follows Ethereum yellow paper Appendix B: all fields are RLP-encoded individually,
/// then wrapped in a list header.
fn rlp_encode_block_header(header: &BlockHeader, buf: &mut Vec<u8>) {
    use alloy_rlp::BufMut;

    let mut fields = Vec::new();
    header.parent_hash.encode(&mut fields);
    header.ommers_hash.encode(&mut fields);
    header.beneficiary.encode(&mut fields);
    header.state_root.encode(&mut fields);
    header.transactions_root.encode(&mut fields);
    header.receipts_root.encode(&mut fields);
    // logs_bloom: RLP encode as bytes (not raw extend)
    header.logs_bloom.as_slice().encode(&mut fields);
    header.difficulty.encode(&mut fields);
    header.number.encode(&mut fields);
    header.gas_limit.encode(&mut fields);
    header.gas_used.encode(&mut fields);
    header.timestamp.encode(&mut fields);
    // extra_data: RLP encode as bytes
    header.extra_data.as_slice().encode(&mut fields);
    header.mix_hash.encode(&mut fields);
    header.nonce.encode(&mut fields);
    if let Some(base_fee) = header.base_fee_per_gas {
        base_fee.encode(&mut fields);
    }
    if let Some(wr) = &header.withdrawals_root {
        wr.encode(&mut fields);
    }
    if let Some(blob_gas_used) = header.blob_gas_used {
        blob_gas_used.encode(&mut fields);
    }
    if let Some(excess_blob_gas) = header.excess_blob_gas {
        excess_blob_gas.encode(&mut fields);
    }
    if let Some(root) = &header.parent_beacon_block_root {
        root.encode(&mut fields);
    }
    if let Some(hash) = &header.requests_hash {
        hash.encode(&mut fields);
    }

    let header_rlp = alloy_rlp::Header {
        list: true,
        payload_length: fields.len(),
    };
    header_rlp.encode(buf);
    buf.put_slice(&fields);
}
