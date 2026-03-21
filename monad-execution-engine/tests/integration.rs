// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Integration tests: verify consensus -> execution runloop -> state_backend + statesync + ethcall + events.

use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use monad_consensus_types::{
    block::{ConsensusBlockHeader, ConsensusFullBlock},
    payload::{ConsensusBlockBody, ConsensusBlockBodyInner, RoundSignature},
    quorum_certificate::QuorumCertificate,
};
use monad_crypto::{
    certificate_signature::CertificateKeyPair,
    NopKeyPair, NopSignature,
};
use monad_eth_types::{EthBlockBody, EthExecutionProtocol, ProposedEthHeader};
use monad_testutil::signing::MockSignatures;
use monad_types::{BlockId, Epoch, NodeId, Round, SeqNum};
use tokio::sync::broadcast;

use monad_execution_engine::command::ExecutionCommand;
use monad_execution_engine::engine::ExecutionEngine;
use monad_execution_engine::ethcall::{CallResult, MonadTracer, StateOverrideSet};
use monad_execution_engine::event_source::ChannelEventSource;
use monad_execution_engine::events::ExecutionEvent;
use monad_execution_engine::mock::db::InMemoryExecutionDb;
use monad_execution_engine::mock::ethcall::MockEthCallHandler;
use monad_execution_engine::mock::executor::MockBlockExecutor;
use monad_execution_engine::mock::statesync::MockStateSyncProvider;
use monad_execution_engine::statesync::{StateSyncProvider, StateSyncRequest};
use alloy_consensus::Header;

type TestST = NopSignature;
type TestSCT = MockSignatures<NopSignature>;

fn make_consensus_block(
    seq_num: u64,
    round: u64,
) -> ConsensusFullBlock<TestST, TestSCT, EthExecutionProtocol> {
    let body = ConsensusBlockBody::new(ConsensusBlockBodyInner {
        execution_body: EthBlockBody::default(),
    });

    let keypair = NopKeyPair::from_bytes(&mut [1u8; 32]).unwrap();
    let round_sig = RoundSignature::new(Round(round), &keypair);

    let header = ConsensusBlockHeader::new(
        NodeId::new(keypair.pubkey()),
        Epoch(1),
        Round(round),
        Vec::new(),
        ProposedEthHeader {
            number: seq_num,
            gas_limit: 30_000_000,
            ..Default::default()
        },
        body.get_id(),
        QuorumCertificate::genesis_qc(),
        SeqNum(seq_num),
        1_000_000_000 * seq_num as u128,
        round_sig,
        0,
        0,
        0,
    );

    ConsensusFullBlock::new(header, body).expect("header/body mismatch")
}

/// Full data flow: send Propose -> collect BlockProposed event
/// Then send Finalize -> collect BlockFinalized event
#[tokio::test]
async fn test_propose_execute_finalize_flow() {
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());

    let traversable = std::sync::Arc::new(InMemoryExecutionDb::new());
    let (engine, mut event_rx, _provider) =
        ExecutionEngine::<TestST, TestSCT>::start(db, executor, traversable);
    let cmd_tx = engine.command_sender();

    let block = make_consensus_block(1, 1);
    let block_id = block.get_id();

    cmd_tx
        .send(ExecutionCommand::Propose {
            block_id,
            block: block.clone(),
        })
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out waiting for BlockProposed")
        .unwrap();

    match event {
        ExecutionEvent::BlockProposed {
            seq_num,
            block_id: bid,
            ..
        } => {
            assert_eq!(seq_num, SeqNum(1));
            assert_eq!(bid, block_id);
        }
        other => panic!("expected BlockProposed, got {:?}", other),
    }

    cmd_tx
        .send(ExecutionCommand::Finalize {
            seq_num: SeqNum(1),
            block_id,
            block,
        })
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out waiting for BlockFinalized")
        .unwrap();

    match event {
        ExecutionEvent::BlockFinalized {
            seq_num,
            block_id: bid,
        } => {
            assert_eq!(seq_num, SeqNum(1));
            assert_eq!(bid, block_id);
        }
        other => panic!("expected BlockFinalized, got {:?}", other),
    }

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out waiting for BlockVerified")
        .unwrap();

    match event {
        ExecutionEvent::BlockVerified { seq_num } => {
            assert_eq!(seq_num, SeqNum(1));
        }
        other => panic!("expected BlockVerified, got {:?}", other),
    }

    engine.shutdown().await;
}

/// Multiple sequential blocks: propose and finalize blocks 1..=3
#[tokio::test]
async fn test_multiple_blocks_sequential() {
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());

    let traversable = std::sync::Arc::new(InMemoryExecutionDb::new());
    let (engine, mut event_rx, _provider) =
        ExecutionEngine::<TestST, TestSCT>::start(db, executor, traversable);
    let cmd_tx = engine.command_sender();

    for i in 1..=3u64 {
        let block = make_consensus_block(i, i);
        let block_id = block.get_id();

        cmd_tx
            .send(ExecutionCommand::Propose {
                block_id,
                block: block.clone(),
            })
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        match &event {
            ExecutionEvent::BlockProposed { seq_num, .. } => assert_eq!(*seq_num, SeqNum(i)),
            other => panic!("expected BlockProposed, got {:?}", other),
        }

        cmd_tx
            .send(ExecutionCommand::Finalize {
                seq_num: SeqNum(i),
                block_id,
                block,
            })
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event,
            ExecutionEvent::BlockFinalized { seq_num, .. } if seq_num == SeqNum(i)
        ));

        let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event,
            ExecutionEvent::BlockVerified { seq_num } if seq_num == SeqNum(i)
        ));
    }

    engine.shutdown().await;
}

/// ChannelEventSource: verify it wraps broadcast::Receiver correctly
#[tokio::test]
async fn test_channel_event_source() {
    let (tx, rx) = broadcast::channel::<ExecutionEvent>(16);

    let mut source = ChannelEventSource::new(rx);

    tx.send(ExecutionEvent::BlockVoted {
        seq_num: SeqNum(42),
        block_id: monad_types::GENESIS_BLOCK_ID,
    })
    .unwrap();

    let event = source.next_event().await.unwrap();
    match event {
        ExecutionEvent::BlockVoted { seq_num, .. } => assert_eq!(seq_num, SeqNum(42)),
        other => panic!("expected BlockVoted, got {:?}", other),
    }

    drop(tx);
    let event = source.next_event().await;
    assert!(event.is_none());
}

/// MockEthCallHandler: verify it returns mock results
#[tokio::test]
async fn test_mock_ethcall_handler() {
    use monad_execution_engine::ethcall::EthCallHandler;

    let handler = MockEthCallHandler::new();
    let overrides = StateOverrideSet::default();

    let result = handler
        .eth_call(
            1,
            vec![],
            vec![],
            Address::ZERO,
            SeqNum(1),
            None,
            &overrides,
            MonadTracer::NoopTracer,
            false,
        )
        .await;

    match result {
        CallResult::Success(success) => {
            assert_eq!(success.gas_used, 21000);
        }
        other => panic!("expected Success, got {:?}", other),
    }
}

/// MockStateSyncProvider: verify sync request handling
#[tokio::test]
async fn test_mock_statesync_provider() {
    let db = InMemoryExecutionDb::with_finalized(10);
    let provider = MockStateSyncProvider::new(Box::new(db));

    let request = StateSyncRequest {
        prefix: 256,
        prefix_bytes: 2,
        target: 5,
        from: 0,
        until: 5,
        old_target: u64::MAX,
    };

    let result = provider.handle_sync_request(&request);
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(result.done.success);
    assert_eq!(result.done.prefix, 256);
}

/// Shutdown: verify graceful shutdown via command
#[tokio::test]
async fn test_graceful_shutdown() {
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());

    let traversable = std::sync::Arc::new(InMemoryExecutionDb::new());
    let (engine, _event_rx, _provider) =
        ExecutionEngine::<TestST, TestSCT>::start(db, executor, traversable);

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown timed out");
}

/// InMemoryExecutionDb: verify basic state operations
#[tokio::test]
async fn test_in_memory_db_state() {
    use monad_execution_engine::traits::ExecutionDb;
    use monad_types::GENESIS_BLOCK_ID;
    use revm::database::states::{AccountStatus, BundleState, StorageSlot};
    use revm::database::BundleAccount;
    use revm::primitives::HashMap;
    use revm::state::AccountInfo;

    let mut db = InMemoryExecutionDb::new();

    let addr = Address::from([0xAA; 20]);
    let slot = B256::from([0xBB; 32]);
    let value = B256::from([0xCC; 32]);

    assert!(db.read_account(&addr).is_none());
    assert_eq!(db.read_storage(&addr, &slot), B256::ZERO);

    let account_info = AccountInfo {
        balance: U256::from(1000),
        nonce: 1,
        code_hash: alloy_consensus::constants::KECCAK_EMPTY,
        code: None,
        account_id: None,
    };

    let mut storage = revm::database::states::plain_account::StorageWithOriginalValues::default();
    storage.insert(
        U256::from_be_bytes(slot.0),
        StorageSlot::new_changed(U256::ZERO, U256::from_be_bytes(value.0)),
    );

    let mut state: HashMap<Address, BundleAccount> = Default::default();
    state.insert(addr, BundleAccount {
        info: Some(account_info),
        original_info: None,
        storage,
        status: AccountStatus::InMemoryChange,
    });

    let bundle = BundleState {
        state,
        ..Default::default()
    };

    let header = Header {
        number: 1,
        ..Default::default()
    };

    let block_id = BlockId(monad_crypto::hasher::Hash([0x01; 32]));
    db.commit(
        block_id,
        &header,
        bundle,
        &[],
        &[],
    );

    let stored_account = db.read_account(&addr).unwrap();
    assert_eq!(stored_account.nonce, 1);
    assert_eq!(db.read_storage(&addr, &slot), value);

    assert!(db.has_executed(&block_id, SeqNum(1)));
    assert!(!db.has_executed(&GENESIS_BLOCK_ID, SeqNum(1)));
}

/// Multiple subscribers can all receive events from the same engine
#[tokio::test]
async fn test_multiple_event_subscribers() {
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());

    let traversable = std::sync::Arc::new(InMemoryExecutionDb::new());
    let (engine, mut rx1, _provider) =
        ExecutionEngine::<TestST, TestSCT>::start(db, executor, traversable);
    let mut rx2 = engine.subscribe_events();
    let cmd_tx = engine.command_sender();

    let block = make_consensus_block(1, 1);
    let block_id = block.get_id();

    cmd_tx
        .send(ExecutionCommand::Propose {
            block_id,
            block,
        })
        .unwrap();

    let e1 = tokio::time::timeout(Duration::from_secs(5), rx1.recv())
        .await
        .expect("rx1 timed out")
        .unwrap();
    let e2 = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
        .await
        .expect("rx2 timed out")
        .unwrap();

    assert!(matches!(e1, ExecutionEvent::BlockProposed { .. }));
    assert!(matches!(e2, ExecutionEvent::BlockProposed { .. }));

    engine.shutdown().await;
}
