// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use monad_consensus_types::block::ConsensusFullBlock;
use monad_crypto::certificate_signature::{CertificateSignaturePubKey, CertificateSignatureRecoverable};
use monad_eth_types::EthExecutionProtocol;
use monad_types::{BlockId, SeqNum};
use monad_validator::signature_collection::SignatureCollection;

#[derive(Clone, Debug)]
pub enum ExecutionCommand<ST, SCT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    Propose {
        block_id: BlockId,
        block: ConsensusFullBlock<ST, SCT, EthExecutionProtocol>,
    },
    Finalize {
        seq_num: SeqNum,
        block_id: BlockId,
        block: ConsensusFullBlock<ST, SCT, EthExecutionProtocol>,
    },
    Shutdown,
}
