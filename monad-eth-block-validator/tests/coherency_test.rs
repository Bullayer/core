// Copyright (C) 2025 Category Labs, Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::collections::BTreeMap;

use alloy_consensus::{
    constants::EMPTY_WITHDRAWALS, proofs::calculate_transaction_root, transaction::Recovered,
    TxEnvelope, EMPTY_OMMER_ROOT_HASH,
};
use monad_chain_config::{
    revision::{ChainRevision, MonadChainRevision},
    ChainConfig, MonadChainConfig, MONAD_DEVNET_CHAIN_ID,
};
use monad_consensus_types::{
    block::{BlockPolicy, ConsensusBlockHeader},
    block_validator::BlockValidator,
    checkpoint::RootInfo,
    metrics::Metrics,
    payload::{ConsensusBlockBody, ConsensusBlockBodyId, ConsensusBlockBodyInner, RoundSignature},
    quorum_certificate::QuorumCertificate,
};
use monad_crypto::{certificate_signature::CertificateKeyPair, NopKeyPair, NopSignature};
use monad_eth_block_policy::{EthBlockPolicy, EthValidatedBlock};
use monad_eth_block_validator::EthBlockValidator;
use monad_eth_types::{EthBlockBody, EthExecutionProtocol, EthHeader, ProposedEthHeader};
use monad_state_backend::NopStateBackend;
use monad_testutil::signing::MockSignatures;
use monad_types::{Epoch, NodeId, Round, SeqNum, GENESIS_BLOCK_ID, GENESIS_ROUND, GENESIS_SEQ_NUM};

type TestBlockPolicy = EthBlockPolicy<
    NopSignature,
    MockSignatures<NopSignature>,
    MonadChainConfig,
    MonadChainRevision,
>;

#[test]
fn sanity_check_coherency() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_line_number(true)
        .with_file(true)
        .with_test_writer()
        .try_init();

    let (_, seq_num, block_policy, chain_config) = genesis_setup();

    let txs = BTreeMap::from([(seq_num, vec![])]);
    let state_backend = NopStateBackend {
        ..Default::default()
    };

    test_runner(chain_config, block_policy, state_backend, txs);
}

fn test_runner(
    chain_config: MonadChainConfig,
    block_policy: TestBlockPolicy,
    state_backend: NopStateBackend,
    txs: BTreeMap<SeqNum, Vec<Recovered<TxEnvelope>>>,
) {
    let validated_blocks = create_test_blocks(&chain_config, &block_policy, txs);

    let root_info = RootInfo {
        round: GENESIS_ROUND,
        seq_num: GENESIS_SEQ_NUM,
        epoch: Epoch(0),
        block_id: GENESIS_BLOCK_ID,
        timestamp_ns: 0,
    };

    if let Some((block_under_test, extending)) = validated_blocks.split_last() {
        block_policy
            .check_coherency(
                block_under_test,
                extending.iter().collect(),
                root_info,
                &state_backend,
                &chain_config,
            )
            .unwrap();
    } else {
        panic!("test did nothing, are inputs correct?");
    }
}

fn genesis_setup() -> (Round, SeqNum, TestBlockPolicy, MonadChainConfig) {
    let round = GENESIS_ROUND + Round(1);
    let seq_num = GENESIS_SEQ_NUM + SeqNum(1);

    let block_policy = TestBlockPolicy::new(GENESIS_SEQ_NUM, 3);

    let chain_config = MonadChainConfig::new(MONAD_DEVNET_CHAIN_ID, None).unwrap();

    (round, seq_num, block_policy, chain_config)
}

fn create_test_blocks(
    chain_config: &MonadChainConfig,
    block_policy: &TestBlockPolicy,
    txs: BTreeMap<SeqNum, Vec<Recovered<TxEnvelope>>>,
) -> Vec<EthValidatedBlock<NopSignature, MockSignatures<NopSignature>>> {
    let mut blocks = vec![];

    for i in 1..=5 {
        let round = GENESIS_ROUND + Round(i);
        let seq_num = GENESIS_SEQ_NUM + SeqNum(i);
        let tx = txs.get(&seq_num).cloned().unwrap_or_default();

        let validated_block =
            create_test_block_helper(chain_config, block_policy, round, seq_num, tx, &blocks);

        blocks.push(validated_block);
    }

    blocks
}

fn create_block_body_helper(
    txs: Vec<Recovered<TxEnvelope>>,
) -> ConsensusBlockBody<EthExecutionProtocol> {
    ConsensusBlockBody::new(ConsensusBlockBodyInner {
        execution_body: EthBlockBody {
            transactions: txs.iter().map(|tx| tx.inner().to_owned()).collect(),
            ommers: Default::default(),
            withdrawals: Default::default(),
        },
    })
}

fn create_block_header_helper(
    round: Round,
    seq_num: SeqNum,
    timestamp: u128,
    body_id: ConsensusBlockBodyId,
    txns_root: [u8; 32],
    base_fees: (u64, u64, u64),
    chain_params: &monad_chain_config::revision::ChainParams,
) -> ConsensusBlockHeader<NopSignature, MockSignatures<NopSignature>, EthExecutionProtocol> {
    let keypair = NopKeyPair::from_bytes(rand::random::<[u8; 32]>().as_mut_slice()).unwrap();
    let signature = RoundSignature::new(round, &keypair);

    let (base_fee, base_trend, base_moment) = base_fees;

    let exec_results = if seq_num < SeqNum(3) {
        vec![]
    } else {
        vec![EthHeader(alloy_consensus::Header::default())]
    };

    ConsensusBlockHeader::new(
        NodeId::new(keypair.pubkey()),
        Epoch(1),
        round,
        exec_results,
        ProposedEthHeader {
            ommers_hash: EMPTY_OMMER_ROOT_HASH.0,
            transactions_root: txns_root,
            number: seq_num.0,
            gas_limit: chain_params.proposal_gas_limit,
            mix_hash: signature.get_hash().0,
            base_fee_per_gas: base_fee,
            withdrawals_root: EMPTY_WITHDRAWALS.0,
            requests_hash: Some([0_u8; 32]),
            ..Default::default()
        },
        body_id,
        QuorumCertificate::genesis_qc(),
        seq_num,
        timestamp,
        signature,
        base_fee,
        base_trend,
        base_moment,
    )
}

fn create_test_block_helper(
    chain_config: &MonadChainConfig,
    block_policy: &TestBlockPolicy,
    round: Round,
    seq_num: SeqNum,
    txs: Vec<Recovered<TxEnvelope>>,
    blocks: &[EthValidatedBlock<NopSignature, MockSignatures<NopSignature>>],
) -> EthValidatedBlock<NopSignature, MockSignatures<NopSignature>> {
    let body = create_block_body_helper(txs);
    let body_id = body.get_id();
    let txns_root = calculate_transaction_root(&body.execution_body.transactions).0;

    let timestamp = seq_num.0 as u128;
    let base_fees = block_policy
        .compute_base_fee::<EthValidatedBlock<NopSignature, MockSignatures<NopSignature>>>(
            blocks,
            chain_config,
        );
    let header = create_block_header_helper(
        round,
        seq_num,
        timestamp,
        body_id,
        txns_root,
        base_fees,
        chain_config.get_chain_revision(round).chain_params(),
    );

    let validator: EthBlockValidator<NopSignature, MockSignatures<NopSignature>> =
        EthBlockValidator::default();
    BlockValidator::<
        NopSignature,
        MockSignatures<NopSignature>,
        EthExecutionProtocol,
        TestBlockPolicy,
        NopStateBackend,
        MonadChainConfig,
        MonadChainRevision,
    >::validate(
        &validator,
        header,
        body,
        None,
        chain_config,
        &mut Metrics::default(),
    )
    .unwrap()
}
