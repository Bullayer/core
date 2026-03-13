// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Integration tests: verify consensus -> execution runloop -> state_backend + statesync + ethcall + events.

use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use tokio::sync::broadcast;

use monad_execution_engine::command::ExecutionCommand;
use monad_execution_engine::engine::ExecutionEngine;
use monad_execution_engine::ethcall::{CallResult, MonadTracer, StateOverrideSet};
use monad_execution_engine::event_source::ChannelEventSource;
use monad_execution_engine::events::ExecutionEvent;
use monad_execution_engine::mock::db::InMemoryExecutionDb;
use monad_execution_engine::mock::ethcall::MockEthCallHandler;
use monad_execution_engine::mock::executor::MockBlockExecutor;
use monad_execution_engine::mock::recovery::MockSignatureRecovery;
use monad_execution_engine::mock::statesync::MockStateSyncProvider;
use monad_execution_engine::statesync::{StateSyncProvider, StateSyncRequest};
use monad_execution_engine::types::{
    BlockHeader, ChainConfig, ConsensusBody, ConsensusHeader, Transaction,
};

fn make_chain_config() -> ChainConfig {
    ChainConfig {
        chain_id: U256::from(1),
    }
}

fn make_consensus_header(seqno: u64, block_number: u64, parent_id: B256) -> ConsensusHeader {
    ConsensusHeader {
        seqno,
        parent_id,
        block_body_id: B256::ZERO,
        block_round: seqno,
        epoch: 0,
        timestamp_ns: 1_000_000_000 * seqno as u128,
        author: [0u8; 33],
        execution_inputs: BlockHeader {
            number: block_number,
            timestamp: seqno,
            gas_limit: 30_000_000,
            ..Default::default()
        },
        delayed_execution_results: Vec::new(),
        base_fee_trend: 0,
        base_fee_moment: 0,
    }
}

fn make_body(num_txs: usize) -> ConsensusBody {
    let transactions = (0..num_txs)
        .map(|i| Transaction {
            hash: B256::from([i as u8; 32]),
            data: vec![0u8; 100],
        })
        .collect();

    ConsensusBody {
        transactions,
        ommers: Vec::new(),
        withdrawals: Vec::new(),
    }
}

/// Full data flow: send Propose -> collect BlockProposed event
/// Then send Finalize -> collect BlockFinalized event
#[tokio::test]
async fn test_propose_execute_finalize_flow() {
    let chain = make_chain_config();
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());
    let recovery = Box::new(MockSignatureRecovery::new());

    let (engine, mut event_rx) = ExecutionEngine::start(chain, db, executor, recovery);
    let cmd_tx = engine.command_sender();

    let block_id = B256::from([0x01; 32]);
    let header = make_consensus_header(1, 1, B256::ZERO);
    let body = make_body(2);

    cmd_tx
        .send(ExecutionCommand::Propose {
            block_id,
            header,
            body,
        })
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out waiting for BlockProposed")
        .unwrap();

    match event {
        ExecutionEvent::BlockProposed {
            block_number,
            block_id: bid,
            header: h,
            ..
        } => {
            assert_eq!(block_number, 1);
            assert_eq!(bid, block_id);
            assert_eq!(h.gas_used, 2 * 21000);
        }
        other => panic!("expected BlockProposed, got {:?}", other),
    }

    // Finalize the block
    cmd_tx
        .send(ExecutionCommand::Finalize {
            block_number: 1,
            block_id,
            verified_blocks: vec![1],
        })
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out waiting for BlockFinalized")
        .unwrap();

    match event {
        ExecutionEvent::BlockFinalized {
            block_number,
            block_id: bid,
        } => {
            assert_eq!(block_number, 1);
            assert_eq!(bid, block_id);
        }
        other => panic!("expected BlockFinalized, got {:?}", other),
    }

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out waiting for BlockVerified")
        .unwrap();

    match event {
        ExecutionEvent::BlockVerified { block_number } => {
            assert_eq!(block_number, 1);
        }
        other => panic!("expected BlockVerified, got {:?}", other),
    }

    engine.shutdown().await;
}

/// Multiple sequential blocks: propose and finalize blocks 1..=3
#[tokio::test]
async fn test_multiple_blocks_sequential() {
    let chain = make_chain_config();
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());
    let recovery = Box::new(MockSignatureRecovery::new());

    let (engine, mut event_rx) = ExecutionEngine::start(chain, db, executor, recovery);
    let cmd_tx = engine.command_sender();

    let mut parent_id = B256::ZERO;

    for i in 1..=3u64 {
        let block_id = B256::from([i as u8; 32]);
        let header = make_consensus_header(i, i, parent_id);
        let body = make_body(1);

        cmd_tx
            .send(ExecutionCommand::Propose {
                block_id,
                header,
                body,
            })
            .await
            .unwrap();

        // Drain BlockProposed
        let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        match &event {
            ExecutionEvent::BlockProposed { block_number, .. } => assert_eq!(*block_number, i),
            other => panic!("expected BlockProposed, got {:?}", other),
        }

        cmd_tx
            .send(ExecutionCommand::Finalize {
                block_number: i,
                block_id,
                verified_blocks: vec![i],
            })
            .await
            .unwrap();

        // Drain BlockFinalized
        let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event,
            ExecutionEvent::BlockFinalized { block_number, .. } if block_number == i
        ));

        // Drain BlockVerified
        let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event,
            ExecutionEvent::BlockVerified { block_number } if block_number == i
        ));

        parent_id = block_id;
    }

    engine.shutdown().await;
}

/// ChannelEventSource: verify it wraps broadcast::Receiver correctly
#[tokio::test]
async fn test_channel_event_source() {
    let (tx, rx) = broadcast::channel::<ExecutionEvent>(16);

    let mut source = ChannelEventSource::new(rx);

    tx.send(ExecutionEvent::BlockVoted {
        block_number: 42,
        block_id: B256::ZERO,
    })
    .unwrap();

    let event = source.next_event().await.unwrap();
    match event {
        ExecutionEvent::BlockVoted { block_number, .. } => assert_eq!(block_number, 42),
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
            1,
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
    let chain = make_chain_config();
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());
    let recovery = Box::new(MockSignatureRecovery::new());

    let (engine, _event_rx) = ExecutionEngine::start(chain, db, executor, recovery);

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown timed out");
}

/// Vote command after propose: should emit BlockVoted + BlockFinalized
#[tokio::test]
async fn test_propose_then_vote() {
    let chain = make_chain_config();
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());
    let recovery = Box::new(MockSignatureRecovery::new());

    let (engine, mut event_rx) = ExecutionEngine::start(chain, db, executor, recovery);
    let cmd_tx = engine.command_sender();

    let block_id = B256::from([0xFF; 32]);
    let header = make_consensus_header(1, 1, B256::ZERO);
    let body = make_body(1);

    // First propose the block
    cmd_tx
        .send(ExecutionCommand::Propose {
            block_id,
            header,
            body,
        })
        .await
        .unwrap();

    // Drain BlockProposed
    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out")
        .unwrap();
    assert!(matches!(event, ExecutionEvent::BlockProposed { .. }));

    // Then vote to finalize
    cmd_tx
        .send(ExecutionCommand::Vote {
            block_number: 1,
            block_id,
        })
        .await
        .unwrap();

    // Should get BlockVoted then BlockFinalized
    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out")
        .unwrap();
    assert!(matches!(event, ExecutionEvent::BlockVoted { .. }));

    let event = tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
        .await
        .expect("timed out")
        .unwrap();
    assert!(matches!(event, ExecutionEvent::BlockFinalized { .. }));

    engine.shutdown().await;
}

/// InMemoryExecutionDb: verify basic state operations
#[tokio::test]
async fn test_in_memory_db_state() {
    use monad_execution_engine::traits::ExecutionDb;
    use monad_execution_engine::types::{Account, AccountDelta, StorageDelta};
    use std::collections::HashMap;

    let mut db = InMemoryExecutionDb::new();

    let addr = Address::from([0xAA; 20]);
    let slot = B256::from([0xBB; 32]);
    let value = B256::from([0xCC; 32]);

    assert!(db.read_account(&addr).is_none());
    assert_eq!(db.read_storage(&addr, &slot), B256::ZERO);

    // Commit a block with state changes
    let account = Account {
        nonce: 1,
        balance: U256::from(1000),
        code_hash: B256::ZERO,
        incarnation: 0,
    };

    let mut storage_deltas = HashMap::new();
    storage_deltas.insert(
        slot,
        StorageDelta {
            old_value: B256::ZERO,
            new_value: value,
        },
    );

    let mut state_deltas = HashMap::new();
    state_deltas.insert(
        addr,
        AccountDelta {
            old_account: None,
            new_account: Some(account.clone()),
            storage: storage_deltas,
        },
    );

    let header = BlockHeader {
        number: 1,
        ..Default::default()
    };

    db.commit(
        B256::from([0x01; 32]),
        &header,
        &state_deltas,
        &HashMap::new(),
        &[],
        &[],
        &[],
    );

    let stored_account = db.read_account(&addr).unwrap();
    assert_eq!(stored_account.nonce, 1);
    assert_eq!(db.read_storage(&addr, &slot), value);

    // Verify has_executed
    assert!(db.has_executed(&B256::from([0x01; 32]), 1));
    assert!(!db.has_executed(&B256::from([0x02; 32]), 1));
}

/// Multiple subscribers can all receive events from the same engine
#[tokio::test]
async fn test_multiple_event_subscribers() {
    let chain = make_chain_config();
    let db = Box::new(InMemoryExecutionDb::new());
    let executor = Box::new(MockBlockExecutor::new());
    let recovery = Box::new(MockSignatureRecovery::new());

    let (engine, mut rx1) = ExecutionEngine::start(chain, db, executor, recovery);
    let mut rx2 = engine.subscribe_events();
    let cmd_tx = engine.command_sender();

    let block_id = B256::from([0x01; 32]);
    let header = make_consensus_header(1, 1, B256::ZERO);
    let body = make_body(0);

    cmd_tx
        .send(ExecutionCommand::Propose {
            block_id,
            header,
            body,
        })
        .await
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
