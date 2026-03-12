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

pub use monad_eth_txpool_types::{
    EthTxPoolBridgeEvictionQueue, EthTxPoolBridgeState, EthTxPoolBridgeStateView,
    TxStatusReceiverSender,
};

#[cfg(test)]
mod test {
    use std::{collections::HashSet, time::Duration};

    use alloy_consensus::{transaction::SignerRecoverable, TxEnvelope};
    use monad_eth_testutil::{make_legacy_tx, S1};
    use monad_eth_txpool_types::{
        EthTxPoolBridgeEvictionQueue, EthTxPoolBridgeState, EthTxPoolBridgeStateView,
        EthTxPoolDropReason, EthTxPoolEvent, EthTxPoolEventType, EthTxPoolEvictReason,
        EthTxPoolSnapshot, TxStatus,
    };
    use tokio::time::Instant;

    const TX_EVICT_DURATION_SECONDS: u64 = 15 * 60;
    const BASE_FEE_PER_GAS: u64 = 100_000_000_000;

    fn setup() -> (
        EthTxPoolBridgeState,
        EthTxPoolBridgeStateView,
        EthTxPoolBridgeEvictionQueue,
        TxEnvelope,
    ) {
        let mut eviction_queue = EthTxPoolBridgeEvictionQueue::default();
        let state = EthTxPoolBridgeState::new(
            &mut eviction_queue,
            EthTxPoolSnapshot {
                txs: HashSet::default(),
            },
        );
        let state_view = state.create_view();

        let tx = make_legacy_tx(S1, BASE_FEE_PER_GAS.into(), 100_000, 0, 0);

        (state, state_view, eviction_queue, tx)
    }

    #[tokio::test]
    async fn test_create_view_linked() {
        let (state, state_view, mut eviction_queue, tx) = setup();

        assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);

        state.add_tx(&mut eviction_queue, &tx, tokio::sync::oneshot::channel().0);

        assert_eq!(
            state_view.get_status_by_hash(tx.tx_hash()),
            Some(TxStatus::Unknown)
        );
    }

    #[tokio::test]
    async fn test_add_tx() {
        let (state, state_view, mut eviction_queue, tx) = setup();

        assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);

        state.add_tx(&mut eviction_queue, &tx, tokio::sync::oneshot::channel().0);
        assert_eq!(
            state_view.get_status_by_hash(tx.tx_hash()),
            Some(TxStatus::Unknown)
        );
    }

    #[tokio::test]
    async fn test_add_duplicate_tx() {
        let (state, state_view, mut eviction_queue, tx) = setup();

        assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);

        let (tx_status_recv_send0, mut tx_status_recv_recv0) = tokio::sync::oneshot::channel();
        let (tx_status_recv_send1, mut tx_status_recv_recv1) = tokio::sync::oneshot::channel();

        assert!(state.add_tx(&mut eviction_queue, &tx, tx_status_recv_send0));
        assert_eq!(
            state_view.get_status_by_hash(tx.tx_hash()),
            Some(TxStatus::Unknown)
        );

        assert!(state.add_tx(&mut eviction_queue, &tx, tx_status_recv_send1));
        assert_eq!(
            state_view.get_status_by_hash(tx.tx_hash()),
            Some(TxStatus::Unknown)
        );

        let tx_status_recv0 = tx_status_recv_recv0.try_recv().unwrap();
        let tx_status_recv1 = tx_status_recv_recv1.try_recv().unwrap();

        assert!(tx_status_recv0.same_channel(&tx_status_recv1));

        assert!(!tx_status_recv0.has_changed().unwrap());
        assert!(!tx_status_recv1.has_changed().unwrap());

        assert_eq!(tx_status_recv0.borrow().to_owned(), TxStatus::Unknown);
        assert_eq!(tx_status_recv1.borrow().to_owned(), TxStatus::Unknown);

        state.handle_events(
            &mut eviction_queue,
            vec![EthTxPoolEvent {
                tx_hash: *tx.tx_hash(),
                action: EthTxPoolEventType::Commit,
            }],
        );

        assert!(tx_status_recv0.has_changed().unwrap());
        assert!(tx_status_recv1.has_changed().unwrap());

        assert_eq!(tx_status_recv0.borrow().to_owned(), TxStatus::Committed);
        assert_eq!(tx_status_recv1.borrow().to_owned(), TxStatus::Committed);

        let (tx_status_recv_send2, mut tx_status_recv_recv2) = tokio::sync::oneshot::channel();

        assert!(!state.add_tx(&mut eviction_queue, &tx, tx_status_recv_send2));
        assert_eq!(
            state_view.get_status_by_hash(tx.tx_hash()),
            Some(TxStatus::Committed)
        );

        let tx_status_recv2 = tx_status_recv_recv2.try_recv().unwrap();

        assert!(tx_status_recv1.same_channel(&tx_status_recv2));

        assert!(tx_status_recv2.has_changed().unwrap());

        assert_eq!(tx_status_recv2.borrow().to_owned(), TxStatus::Committed);
    }

    #[tokio::test]
    async fn test_snapshot_does_not_update_tx_status_recv() {
        let (state, state_view, mut eviction_queue, tx) = setup();

        assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);

        let (tx_status_recv_send, mut tx_status_recv_recv) = tokio::sync::oneshot::channel();
        state.add_tx(&mut eviction_queue, &tx, tx_status_recv_send);

        let tx_status_recv = tx_status_recv_recv.try_recv().unwrap();
        assert!(!tx_status_recv.has_changed().unwrap());

        state.apply_snapshot(
            &mut eviction_queue,
            EthTxPoolSnapshot {
                txs: HashSet::from_iter([*tx.tx_hash()]),
            },
        );
        assert!(!tx_status_recv.has_changed().unwrap());
    }

    #[tokio::test]
    async fn test_handle_events_and_snapshot() {
        enum TestCases {
            EmptySnapshot,
            Insert,
            InsertSnapshot,
            Drop,
            Commit,
            Evict,
        }

        for test in [
            TestCases::EmptySnapshot,
            TestCases::Insert,
            TestCases::InsertSnapshot,
            TestCases::Drop,
            TestCases::Commit,
            TestCases::Evict,
        ] {
            let (state, state_view, mut eviction_queue, tx) = setup();

            state.add_tx(&mut eviction_queue, &tx, tokio::sync::oneshot::channel().0);
            assert_eq!(
                state_view.get_status_by_hash(tx.tx_hash()),
                Some(TxStatus::Unknown)
            );

            match test {
                TestCases::EmptySnapshot => {
                    state.apply_snapshot(
                        &mut eviction_queue,
                        EthTxPoolSnapshot {
                            txs: HashSet::default(),
                        },
                    );
                    assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);
                }
                TestCases::Insert => {
                    state.handle_events(
                        &mut eviction_queue,
                        vec![EthTxPoolEvent {
                            tx_hash: tx.tx_hash().to_owned(),
                            action: EthTxPoolEventType::Insert {
                                address: tx.recover_signer().unwrap(),
                                owned: true,
                                tx: tx.clone(),
                            },
                        }],
                    );
                    assert_eq!(
                        state_view.get_status_by_hash(tx.tx_hash()),
                        Some(TxStatus::Tracked)
                    );
                }
                TestCases::InsertSnapshot => {
                    state.apply_snapshot(
                        &mut eviction_queue,
                        EthTxPoolSnapshot {
                            txs: HashSet::from_iter(std::iter::once(tx.tx_hash().to_owned())),
                        },
                    );
                    assert_eq!(
                        state_view.get_status_by_hash(tx.tx_hash()),
                        Some(TxStatus::Tracked)
                    );
                }
                TestCases::Drop => {
                    state.handle_events(
                        &mut eviction_queue,
                        vec![EthTxPoolEvent {
                            tx_hash: tx.tx_hash().to_owned(),
                            action: EthTxPoolEventType::Drop {
                                reason: EthTxPoolDropReason::PoolNotReady,
                            },
                        }],
                    );
                    assert_eq!(
                        state_view.get_status_by_hash(tx.tx_hash()),
                        Some(TxStatus::Dropped {
                            reason: EthTxPoolDropReason::PoolNotReady
                        })
                    );
                }
                TestCases::Commit => {
                    state.handle_events(
                        &mut eviction_queue,
                        vec![EthTxPoolEvent {
                            tx_hash: tx.tx_hash().to_owned(),
                            action: EthTxPoolEventType::Commit,
                        }],
                    );
                    assert_eq!(
                        state_view.get_status_by_hash(tx.tx_hash()),
                        Some(TxStatus::Committed)
                    );

                    state.apply_snapshot(
                        &mut eviction_queue,
                        EthTxPoolSnapshot {
                            txs: HashSet::default(),
                        },
                    );
                    assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);
                }
                TestCases::Evict => {
                    state.handle_events(
                        &mut eviction_queue,
                        vec![EthTxPoolEvent {
                            tx_hash: tx.tx_hash().to_owned(),
                            action: EthTxPoolEventType::Evict {
                                reason: EthTxPoolEvictReason::Expired,
                            },
                        }],
                    );
                    assert_eq!(
                        state_view.get_status_by_hash(tx.tx_hash()),
                        Some(TxStatus::Evicted {
                            reason: EthTxPoolEvictReason::Expired
                        })
                    );

                    state.apply_snapshot(
                        &mut eviction_queue,
                        EthTxPoolSnapshot {
                            txs: HashSet::default(),
                        },
                    );
                    assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_cleanup() {
        for add_duplicate_tx in [false, true] {
            let (state, state_view, mut eviction_queue, tx) = setup();

            assert_eq!(eviction_queue.len(), 0);
            assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);

            state.add_tx(&mut eviction_queue, &tx, tokio::sync::oneshot::channel().0);
            assert_eq!(eviction_queue.len(), 1);
            assert_eq!(
                state_view.get_status_by_hash(tx.tx_hash()),
                Some(TxStatus::Unknown)
            );

            state.cleanup(&mut eviction_queue, Instant::now());
            assert_eq!(eviction_queue.len(), 1);
            assert_eq!(
                state_view.get_status_by_hash(tx.tx_hash()),
                Some(TxStatus::Unknown)
            );

            tokio::time::advance(
                Duration::from_secs(TX_EVICT_DURATION_SECONDS)
                    .checked_sub(Duration::from_millis(1))
                    .unwrap(),
            )
            .await;

            state.cleanup(&mut eviction_queue, Instant::now());
            assert_eq!(eviction_queue.len(), 1);
            assert_eq!(
                state_view.get_status_by_hash(tx.tx_hash()),
                Some(TxStatus::Unknown)
            );

            if add_duplicate_tx {
                state.add_tx(&mut eviction_queue, &tx, tokio::sync::oneshot::channel().0);
                assert_eq!(eviction_queue.len(), 1);
                assert_eq!(
                    state_view.get_status_by_hash(tx.tx_hash()),
                    Some(TxStatus::Unknown)
                );

                state.cleanup(&mut eviction_queue, Instant::now());
                assert_eq!(eviction_queue.len(), 1);
                assert_eq!(
                    state_view.get_status_by_hash(tx.tx_hash()),
                    Some(TxStatus::Unknown)
                );
            }

            tokio::time::advance(Duration::from_millis(1)).await;

            state.cleanup(&mut eviction_queue, Instant::now());
            assert_eq!(eviction_queue.len(), 0);
            assert_eq!(state_view.get_status_by_hash(tx.tx_hash()), None);
        }
    }
}
