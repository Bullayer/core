// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Benchmarks for RethExecutionDb: read path (overlay vs reth), commit, finalize,
// and StateSync traverse/apply_batch.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_consensus::Header;
use alloy_primitives::{Address, B256, U256};
use std::hint::black_box;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use monad_eth_types::EthAccount;
use monad_execution_engine::statesync::{StateSyncApplierDb, StateSyncBatch, StateSyncTraversable};
use monad_execution_engine::traits::ExecutionDb;
use monad_execution_engine::types::{AccountDelta, CodeMap, StateDeltas, StorageDelta};
use monad_types::{BlockId, SeqNum};
use reth_chainspec::DEV;

use monad_execution_db::RethExecutionDb;
use monad_execution_db::bridge::state_deltas_to_bundle;

fn deltas_to_bundle(deltas: &StateDeltas) -> revm::database::states::BundleState {
    state_deltas_to_bundle(deltas, &CodeMap::new())
}

fn deltas_to_bundle_with_code(deltas: &StateDeltas, code: &CodeMap) -> revm::database::states::BundleState {
    state_deltas_to_bundle(deltas, code)
}

fn tmp_db() -> (tempfile::TempDir, RethExecutionDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = RethExecutionDb::open(dir.path(), Arc::clone(&*DEV)).expect("open db");
    (dir, db)
}

fn make_block_id(n: u8) -> BlockId {
    BlockId(monad_types::Hash([n; 32]))
}

fn make_account(nonce: u64, balance: u128) -> EthAccount {
    EthAccount {
        nonce,
        balance: U256::from(balance),
        code_hash: None,
        is_delegated: false,
    }
}

fn make_header(number: u64) -> Header {
    Header {
        number,
        gas_limit: 30_000_000,
        ..Default::default()
    }
}

fn make_state_deltas(
    addr: Address,
    old: Option<EthAccount>,
    new: Option<EthAccount>,
    storage: Vec<(B256, B256, B256)>,
) -> StateDeltas {
    let mut storage_deltas = HashMap::new();
    for (slot, old_val, new_val) in storage {
        storage_deltas.insert(
            slot,
            StorageDelta {
                old_value: old_val,
                new_value: new_val,
            },
        );
    }
    let mut deltas = StateDeltas::new();
    deltas.insert(
        addr,
        AccountDelta {
            old_account: old,
            new_account: new,
            storage: storage_deltas,
            ..Default::default()
        },
    );
    deltas
}

/// Open DB (cold temp dir) — baseline for startup cost.
fn bench_open_db(c: &mut Criterion) {
    let mut group = c.benchmark_group("open_db");
    group.sample_size(20);
    group.bench_function("open_reth_db", |b| {
        b.iter_batched(
            || tempfile::tempdir().expect("tempdir"),
            |dir| {
                black_box(RethExecutionDb::open(dir.path(), Arc::clone(&*DEV)).expect("open"))
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

/// Read path: after genesis, read_account falls through to reth.
fn bench_read_account_reth(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_account_reth");
    group.sample_size(50);

    group.bench_function("read_one_account", |b| {
        b.iter_batched(
            || {
                let (_dir, db) = tmp_db();
                let addr = Address::from([0xAA; 20]);
                let genesis_acc = make_account(0, 1_000_000);
                db.init_genesis(
                    &make_header(0),
                    &make_state_deltas(addr, None, Some(genesis_acc), vec![]),
                    &CodeMap::new(),
                )
                .expect("genesis");
                (db, addr)
            },
            |(db, addr)| black_box(ExecutionDb::read_account(&db, &addr)),
            BatchSize::SmallInput,
        )
    });

    group.throughput(Throughput::Elements(100));
    group.bench_function("read_100_accounts", |b| {
        let (_dir, db) = tmp_db();
        let mut addrs = Vec::with_capacity(100);
        let mut deltas = StateDeltas::new();
        for i in 0..100u64 {
            let addr = Address::from([i as u8; 20]);
            addrs.push(addr);
            deltas.insert(
                addr,
                AccountDelta {
                    old_account: None,
                    new_account: Some(make_account(i, (i * 1000) as u128)),
                    ..Default::default()
                },
            );
        }
        db.init_genesis(&make_header(0), &deltas, &CodeMap::new())
            .expect("genesis");
        b.iter(|| {
            for addr in &addrs {
                black_box(ExecutionDb::read_account(&db, addr));
            }
        })
    });
    group.finish();
}

/// Read path: read_storage from reth (after genesis with one storage slot).
fn bench_read_storage_reth(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_storage_reth");
    group.sample_size(50);
    group.bench_function("read_one_slot", |b| {
        b.iter_batched(
            || {
                let (_dir, db) = tmp_db();
                let addr = Address::from([0xBB; 20]);
                let slot = B256::from([0x01; 32]);
                let val = B256::from([0xFF; 32]);
                db.init_genesis(
                    &make_header(0),
                    &make_state_deltas(
                        addr,
                        None,
                        Some(make_account(0, 0)),
                        vec![(slot, B256::ZERO, val)],
                    ),
                    &CodeMap::new(),
                )
                .expect("genesis");
                (db, addr, slot)
            },
            |(db, addr, slot)| black_box(ExecutionDb::read_storage(&db, &addr, &slot)),
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

/// Overlay hit: commit one block, then read_account/read_storage from overlay.
fn bench_read_overlay_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_overlay_hit");

    group.bench_function("read_account_overlay", |b| {
        b.iter_batched(
            || {
                let (_dir, mut db) = tmp_db();
                db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
                    .expect("genesis");
                let addr = Address::from([0x11; 20]);
                let block_id = make_block_id(1);
                let deltas =
                    make_state_deltas(addr, None, Some(make_account(1, 5000)), vec![]);
                db.set_block_and_prefix(SeqNum(0), make_block_id(0));
                db.commit(block_id, &make_header(1), deltas_to_bundle(&deltas), &[], &[]);
                db.set_block_and_prefix(SeqNum(1), block_id);
                (db, addr)
            },
            |(db, addr)| black_box(ExecutionDb::read_account(&db, &addr)),
            BatchSize::SmallInput,
        )
    });

    group.bench_function("read_storage_overlay", |b| {
        b.iter_batched(
            || {
                let (_dir, mut db) = tmp_db();
                db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
                    .expect("genesis");
                let addr = Address::from([0x22; 20]);
                let slot = B256::from([0x02; 32]);
                let val = B256::from([0xEE; 32]);
                let block_id = make_block_id(1);
                let deltas = make_state_deltas(
                    addr,
                    None,
                    Some(make_account(0, 0)),
                    vec![(slot, B256::ZERO, val)],
                );
                db.set_block_and_prefix(SeqNum(0), make_block_id(0));
                db.commit(block_id, &make_header(1), deltas_to_bundle(&deltas), &[], &[]);
                db.set_block_and_prefix(SeqNum(1), block_id);
                (db, addr, slot)
            },
            |(db, addr, slot)| black_box(ExecutionDb::read_storage(&db, &addr, &slot)),
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

/// Commit: overlay insert only (no reth write).
fn bench_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("commit");
    group.sample_size(30);

    group.bench_function("commit_one_block", |b| {
        // Important: don't open a new MDBX env per iteration.
        // `libmdbx-rs` spawns a txn-manager background thread for each env open; repeatedly
        // creating temp DBs can exhaust the OS thread limit and cause a panic.
        let (_dir, mut db) = tmp_db();
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");
        let addr = Address::from([0x33; 20]);
        let block_id = make_block_id(1);
        let parent_block_id = make_block_id(0);
        let header = make_header(1);
        let code = CodeMap::new();
        let deltas = make_state_deltas(addr, None, Some(make_account(1, 100)), vec![]);

        let bundle = deltas_to_bundle_with_code(&deltas, &code);
        b.iter(|| {
            db.set_block_and_prefix(SeqNum(0), parent_block_id);
            db.commit(block_id, &header, bundle.clone(), &[], &[]);
            black_box(())
        })
    });

    group.bench_function("commit_block_10_accounts", |b| {
        // Same rationale as `commit_one_block`.
        let (_dir, mut db) = tmp_db();
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");

        let mut deltas = StateDeltas::new();
        for i in 0..10u64 {
            let addr = Address::from([i as u8; 20]);
            deltas.insert(
                addr,
                AccountDelta {
                    old_account: None,
                    new_account: Some(make_account(i, (i * 100) as u128)),
                    ..Default::default()
                },
            );
        }

        let block_id = make_block_id(1);
        let parent_block_id = make_block_id(0);
        let header = make_header(1);
        let code = CodeMap::new();

        let bundle = deltas_to_bundle_with_code(&deltas, &code);
        b.iter(|| {
            db.set_block_and_prefix(SeqNum(0), parent_block_id);
            db.commit(block_id, &header, bundle.clone(), &[], &[]);
            black_box(())
        })
    });
    group.finish();
}

/// Finalize: collect_chain + flush_to_reth (single block).
/// Uses one DB per bench and repeats commit+finalize in a loop to avoid exhausting
/// the OS thread limit (each MDBX env open spawns a txn-manager thread).
fn bench_finalize(c: &mut Criterion) {
    let mut group = c.benchmark_group("finalize");
    group.sample_size(20);

    group.bench_function("finalize_single_block", |b| {
        let (_dir, mut db) = tmp_db();
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");
        let addr = Address::from([0x44; 20]);
        let code = CodeMap::new();
        let mut block_num = 1u64;
        let mut prev_id = make_block_id(0);
        let mut prev_acc: Option<EthAccount> = None;

        b.iter(|| {
            let block_id = make_block_id((block_num % 256) as u8);
            let header = make_header(block_num);
            let acc = make_account(block_num, (block_num * 200) as u128);
            let deltas =
                make_state_deltas(addr, prev_acc, Some(acc), vec![]);
            db.set_block_and_prefix(SeqNum(block_num - 1), prev_id);
            db.commit(block_id, &header, deltas_to_bundle_with_code(&deltas, &code), &[], &[]);
            db.finalize(SeqNum(block_num), block_id);
            prev_id = block_id;
            prev_acc = Some(acc);
            block_num += 1;
            black_box(())
        })
    });

    group.bench_function("finalize_chain_5_blocks", |b| {
        let (_dir, mut db) = tmp_db();
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");
        let addr = Address::from([0x55; 20]);
        let code = CodeMap::new();
        let mut block_num = 1u64;

        b.iter(|| {
            let mut prev_id = make_block_id((block_num.saturating_sub(1) % 256) as u8);
            let mut prev_acc: Option<EthAccount> = None;
            for i in 0..5u64 {
                let seq = block_num + i;
                let acc = make_account(seq, (seq * 1000) as u128);
                let block_id = make_block_id((seq % 256) as u8);
                let deltas =
                    make_state_deltas(addr, prev_acc, Some(acc), vec![]);
                db.set_block_and_prefix(SeqNum(seq - 1), prev_id);
                db.commit(block_id, &make_header(seq), deltas_to_bundle_with_code(&deltas, &code), &[], &[]);
                prev_id = block_id;
                prev_acc = Some(acc);
            }
            let tip_id = make_block_id(((block_num + 4) % 256) as u8);
            db.finalize(SeqNum(block_num + 4), tip_id);
            block_num += 5;
            black_box(())
        })
    });
    group.finish();
}

/// Metadata / hot path: has_executed, get_latest_finalized_version, read_eth_header.
fn bench_metadata(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata");
    group.sample_size(100);

    group.bench_function("has_executed", |b| {
        let (_dir, db) = tmp_db();
        let block_id = make_block_id(1);
        b.iter(|| black_box(db.has_executed(&block_id, SeqNum(1))))
    });

    group.bench_function("get_latest_finalized_version", |b| {
        let (_dir, db) = tmp_db();
        b.iter(|| black_box(db.get_latest_finalized_version()))
    });

    group.bench_function("read_eth_header", |b| {
        // Avoid opening a new MDBX env per iteration.
        // `libmdbx-rs` spawns a background txn-manager thread on env open, so repeatedly
        // creating temp DBs can exhaust OS threads and panic.
        let (_dir, mut db) = tmp_db();
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");
        let block_id = make_block_id(1);
        db.set_block_and_prefix(SeqNum(0), make_block_id(0));
        db.commit(
            block_id,
            &make_header(1),
            deltas_to_bundle(&make_state_deltas(
                Address::from([0x66; 20]),
                None,
                Some(make_account(0, 0)),
                vec![],
            )),
            &[],
            &[],
        );

        b.iter(|| black_box(db.read_eth_header()))
    });
    group.finish();
}

/// StateSync: traverse_state (full scan with empty prefix).
fn bench_traverse_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("statesync_traverse");
    group.sample_size(10);

    group.bench_function("traverse_empty_db", |b| {
        // Avoid opening a new MDBX env per iteration.
        let (_dir, db) = tmp_db();
        let db = db;
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");

        b.iter(|| {
            let mut count = 0u64;
            db.traverse_state(&[], 0, 1, &mut |_, _| count += 1);
            black_box(count)
        })
    });

    group.bench_function("traverse_100_accounts", |b| {
        // Avoid opening a new MDBX env per iteration.
        let (_dir, db) = tmp_db();
        let mut deltas = StateDeltas::new();
        for i in 0..100u64 {
            let addr = Address::from([i as u8; 20]);
            deltas.insert(
                addr,
                AccountDelta {
                    old_account: None,
                    new_account: Some(make_account(i, (i * 100) as u128)),
                    ..Default::default()
                },
            );
        }
        db.init_genesis(&make_header(0), &deltas, &CodeMap::new())
            .expect("genesis");

        b.iter(|| {
            let mut count = 0u64;
            db.traverse_state(&[], 0, 1, &mut |_, _| count += 1);
            black_box(count)
        })
    });
    group.finish();
}

/// StateSync: apply_batch (write accounts + storage + code to reth).
fn bench_apply_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("statesync_apply_batch");
    group.sample_size(20);

    group.bench_function("apply_batch_10_accounts", |b| {
        // Avoid opening a new MDBX env per iteration.
        let (_dir, mut db) = tmp_db();
        db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
            .expect("genesis");

        let mut accounts = Vec::with_capacity(10);
        for i in 0..10u64 {
            let addr = [i as u8; 20];
            accounts.push((addr, Some(make_account(i, (i * 100) as u128))));
        }
        let batch = StateSyncBatch {
            accounts,
            storage: vec![],
            code: vec![],
        };

        b.iter(|| {
            db.apply_batch(batch.clone(), 1);
            black_box(())
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_open_db,
    bench_read_account_reth,
    bench_read_storage_reth,
    bench_read_overlay_hit,
    bench_commit,
    bench_finalize,
    bench_metadata,
    bench_traverse_state,
    bench_apply_batch,
);
criterion_main!(benches);
