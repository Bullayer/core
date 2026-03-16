// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use monad_consensus_types::block::ConsensusFullBlock;
use monad_crypto::certificate_signature::{CertificateSignaturePubKey, CertificateSignatureRecoverable};
use monad_eth_types::EthExecutionProtocol;
use monad_types::{SeqNum, GENESIS_BLOCK_ID};
use monad_validator::signature_collection::SignatureCollection;

use crate::block_hash::BlockHashChain;
use crate::traits::{BlockExecutor, ExecutionDb, ExecutionError};
use crate::types::{Block, BlockExecOutput, CodeMap, StateDeltas};
use crate::validation::{compute_block_hash, validate_live_execution_outputs};

fn static_validate_consensus<ST, SCT>(
    block: &ConsensusFullBlock<ST, SCT, EthExecutionProtocol>,
) -> Result<(), ExecutionError>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    if block.get_seq_num() == SeqNum(0) {
        return Err(ExecutionError::ValidationError("seq_num must be > 0".to_string()));
    }
    Ok(())
}

pub fn propose_block<ST, SCT>(
    block: &ConsensusFullBlock<ST, SCT, EthExecutionProtocol>,
    block_hash_chain: &mut BlockHashChain,
    db: &mut dyn ExecutionDb,
    executor: &dyn BlockExecutor,
    is_first_block: bool,
) -> Result<BlockExecOutput, ExecutionError>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    let block_id = block.get_id();
    let seq_num = block.get_seq_num();
    let parent_id = block.get_parent_id();

    let block_hash_buffer = block_hash_chain.find_chain(&parent_id);

    static_validate_consensus(block)?;
    // static_validate_block

    let parent_block_id = if is_first_block { GENESIS_BLOCK_ID } else { parent_id };
    db.set_block_and_prefix(seq_num - SeqNum(1), parent_block_id);

    let body = &block.body().execution_body;
    let eth_block = Block {
        header: block.get_execution_inputs().clone(),
        transactions: body.transactions.to_vec(),
        ommers: body.ommers.to_vec(),
        withdrawals: body.withdrawals.to_vec(),
    };

    let exec_output = executor.execute_block(
        &eth_block,
        db,
        &block_hash_buffer,
    )?;

    let state_deltas = StateDeltas::new();
    let code = CodeMap::new();
    db.commit(
        block_id,
        &exec_output.eth_header,
        &state_deltas,
        &code,
        &exec_output.receipts,
        &eth_block.transactions,
    );

    let output_header = db.read_eth_header();

    validate_live_execution_outputs(&eth_block.header, &output_header)?;

    let eth_block_hash = compute_block_hash(&output_header);

    block_hash_chain.propose(
        eth_block_hash,
        seq_num,
        block_id,
        parent_id,
    );

    Ok(BlockExecOutput {
        eth_header: output_header,
        eth_block_hash,
        transactions: eth_block.transactions,
        receipts: exec_output.receipts,
    })
}
