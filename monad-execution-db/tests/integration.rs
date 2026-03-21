use std::collections::HashMap;
use std::sync::Arc;

use alloy_consensus::Header;
use alloy_primitives::{Address, B256, U256};
use reth_chainspec::DEV;
use revm::database::states::BundleState;

use monad_eth_types::EthAccount;
use monad_execution_engine::statesync::{StateSyncApplierDb, StateSyncBatch, StateSyncTraversable};
use monad_execution_engine::traits::ExecutionDb;
use monad_execution_engine::types::{AccountDelta, CodeMap, StateDeltas, StorageDelta};
use monad_types::{BlockId, SeqNum};

use monad_execution_db::RethExecutionDb;
use monad_execution_db::bridge::state_deltas_to_bundle;

fn deltas_to_bundle(deltas: &StateDeltas) -> BundleState {
    state_deltas_to_bundle(deltas, &CodeMap::new())
}

fn tmp_db() -> (tempfile::TempDir, RethExecutionDb) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = RethExecutionDb::open(dir.path(), Arc::clone(&*DEV)).expect("open db");
    (dir, db)
}

fn reopen_db(dir: &tempfile::TempDir) -> RethExecutionDb {
    RethExecutionDb::open(dir.path(), Arc::clone(&*DEV)).expect("reopen db")
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

/// Read account via ExecutionDb trait (disambiguate from StateSyncApplierDb)
fn exec_read_account(db: &dyn ExecutionDb, addr: &Address) -> Option<EthAccount> {
    db.read_account(addr)
}

/// Read storage via ExecutionDb trait
fn exec_read_storage(db: &dyn ExecutionDb, addr: &Address, slot: &B256) -> B256 {
    db.read_storage(addr, slot)
}

// ---------------------------------------------------------------------------
// Test 1: Genesis initialization and read-back
// ---------------------------------------------------------------------------
#[test]
fn test_genesis_init_and_read() {
    let (_dir, db) = tmp_db();

    let addr = Address::from([0xAA; 20]);
    let genesis_acc = make_account(0, 1_000_000);
    let header = make_header(0);
    let deltas = make_state_deltas(addr, None, Some(genesis_acc), vec![]);

    db.init_genesis(&header, &deltas, &CodeMap::new()).expect("init_genesis");

    assert_eq!(db.get_latest_finalized_version(), SeqNum(0));

    // After genesis, overlay is empty, so trait read falls through to reth
    let acc = exec_read_account(&db, &addr).expect("account should exist");
    assert_eq!(acc.nonce, 0);
    assert_eq!(acc.balance, U256::from(1_000_000));

    let h = db.read_eth_header();
    assert_eq!(h.number, 0);
}

// ---------------------------------------------------------------------------
// Test 2: Genesis is idempotent (double init is no-op)
// ---------------------------------------------------------------------------
#[test]
fn test_genesis_idempotent() {
    let (_dir, db) = tmp_db();

    let header = make_header(0);
    let deltas = StateDeltas::new();
    let code = CodeMap::new();

    db.init_genesis(&header, &deltas, &code).expect("first init");
    db.init_genesis(&header, &deltas, &code).expect("second init should be no-op");
}

// ---------------------------------------------------------------------------
// Test 3: Commit → overlay read → finalize → persisted read
// ---------------------------------------------------------------------------
#[test]
fn test_commit_finalize_flow() {
    let (_dir, mut db) = tmp_db();

    let header0 = make_header(0);
    db.init_genesis(&header0, &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let addr = Address::from([0x11; 20]);
    let new_acc = make_account(1, 5000);
    let slot = B256::from([0x01; 32]);
    let val = B256::from([0xFF; 32]);

    let header1 = make_header(1);
    let block_id = make_block_id(1);
    let deltas = make_state_deltas(
        addr,
        None,
        Some(new_acc),
        vec![(slot, B256::ZERO, val)],
    );

    // set_block_and_prefix sets parent context (block 0, genesis)
    db.set_block_and_prefix(SeqNum(0), make_block_id(0));
    db.commit(block_id, &header1, deltas_to_bundle(&deltas), &[], &[]);

    // has_executed should be true (block is in overlay)
    assert!(db.has_executed(&block_id, SeqNum(1)));

    // Read from overlay at block 1 perspective
    db.set_block_and_prefix(SeqNum(1), block_id);
    let acc = exec_read_account(&db, &addr).expect("overlay read");
    assert_eq!(acc.nonce, 1);
    assert_eq!(acc.balance, U256::from(5000));

    let stored_val = exec_read_storage(&db, &addr, &slot);
    assert_eq!(stored_val, val);

    // Finalize: flush to reth, GC overlay
    db.finalize(SeqNum(1), block_id);
    assert_eq!(db.get_latest_finalized_version(), SeqNum(1));

    // After finalize + GC, overlay is empty, reads go to reth
    let acc = exec_read_account(&db, &addr).expect("persisted read");
    assert_eq!(acc.nonce, 1);
    assert_eq!(acc.balance, U256::from(5000));

    let stored_val = exec_read_storage(&db, &addr, &slot);
    assert_eq!(stored_val, val);
}

// ---------------------------------------------------------------------------
// Test 4: Overlay chain-walk reads parent block state
// ---------------------------------------------------------------------------
#[test]
fn test_overlay_chain_walk() {
    let (_dir, mut db) = tmp_db();

    let header0 = make_header(0);
    db.init_genesis(&header0, &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let addr = Address::from([0x22; 20]);
    let genesis_block_id = make_block_id(0);
    let block1_id = make_block_id(1);
    let block2_id = make_block_id(2);

    // Block 1: create account with nonce=1
    let acc1 = make_account(1, 100);
    let header1 = make_header(1);
    let deltas1 = make_state_deltas(addr, None, Some(acc1), vec![]);

    db.set_block_and_prefix(SeqNum(0), genesis_block_id);
    db.commit(block1_id, &header1, deltas_to_bundle(&deltas1), &[], &[]);

    // Block 2: update account to nonce=2 (parent is block 1)
    let acc2 = make_account(2, 200);
    let header2 = make_header(2);
    let deltas2 = make_state_deltas(addr, Some(acc1), Some(acc2), vec![]);

    db.set_block_and_prefix(SeqNum(1), block1_id);
    db.commit(block2_id, &header2, deltas_to_bundle(&deltas2), &[], &[]);

    // Read from block 2's perspective: should see nonce=2
    db.set_block_and_prefix(SeqNum(2), block2_id);
    let acc = exec_read_account(&db, &addr).expect("overlay chain walk to block 2");
    assert_eq!(acc.nonce, 2);

    // Read from block 1's perspective: should see nonce=1
    db.set_block_and_prefix(SeqNum(1), block1_id);
    let acc = exec_read_account(&db, &addr).expect("overlay chain walk to block 1");
    assert_eq!(acc.nonce, 1);
}

// ---------------------------------------------------------------------------
// Test 5: Multiple blocks finalized sequentially
// ---------------------------------------------------------------------------
#[test]
fn test_multi_block_finalize() {
    let (_dir, mut db) = tmp_db();

    let header0 = make_header(0);
    db.init_genesis(&header0, &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let addr = Address::from([0x33; 20]);
    let mut prev_block_id = make_block_id(0);
    let mut prev_acc: Option<EthAccount> = None;

    for i in 1..=5u64 {
        let acc = make_account(i, i as u128 * 1000);
        let header = make_header(i);
        let block_id = make_block_id(i as u8);
        let deltas = make_state_deltas(addr, prev_acc, Some(acc), vec![]);

        db.set_block_and_prefix(SeqNum(i - 1), prev_block_id);
        db.commit(block_id, &header, deltas_to_bundle(&deltas), &[], &[]);
        db.finalize(SeqNum(i), block_id);

        assert_eq!(db.get_latest_finalized_version(), SeqNum(i));

        let persisted = exec_read_account(&db, &addr).expect("account persisted");
        assert_eq!(persisted.nonce, i);

        prev_block_id = block_id;
        prev_acc = Some(acc);
    }
}

// ---------------------------------------------------------------------------
// Test 6: Batch finalize (multiple blocks committed then finalized at once)
// ---------------------------------------------------------------------------
#[test]
fn test_batch_finalize() {
    let (_dir, mut db) = tmp_db();

    let header0 = make_header(0);
    db.init_genesis(&header0, &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let addr = Address::from([0x44; 20]);
    let genesis_id = make_block_id(0);
    let block1_id = make_block_id(1);
    let block2_id = make_block_id(2);
    let block3_id = make_block_id(3);

    let acc1 = make_account(1, 100);
    db.set_block_and_prefix(SeqNum(0), genesis_id);
    db.commit(block1_id, &make_header(1), deltas_to_bundle(&make_state_deltas(addr, None, Some(acc1), vec![])), &[], &[]);

    let acc2 = make_account(2, 200);
    db.set_block_and_prefix(SeqNum(1), block1_id);
    db.commit(block2_id, &make_header(2), deltas_to_bundle(&make_state_deltas(addr, Some(acc1), Some(acc2), vec![])), &[], &[]);

    let acc3 = make_account(3, 300);
    db.set_block_and_prefix(SeqNum(2), block2_id);
    db.commit(block3_id, &make_header(3), deltas_to_bundle(&make_state_deltas(addr, Some(acc2), Some(acc3), vec![])), &[], &[]);

    // Finalize all at once via block 3 (collect_chain walks 3→2→1)
    db.finalize(SeqNum(3), block3_id);
    assert_eq!(db.get_latest_finalized_version(), SeqNum(3));

    let persisted = exec_read_account(&db, &addr).expect("account persisted");
    assert_eq!(persisted.nonce, 3);
    assert_eq!(persisted.balance, U256::from(300));
}

// ---------------------------------------------------------------------------
// Test 7: State sync apply_batch + finalize_statesync
// ---------------------------------------------------------------------------
#[test]
fn test_statesync_apply_and_finalize() {
    let (_dir, mut db) = tmp_db();

    let addr_bytes: [u8; 20] = [0x55; 20];
    let acc = make_account(10, 99999);
    let code_hash: [u8; 32] = [0xCC; 32];
    let code_bytes = vec![0x60, 0x00, 0x60, 0x00, 0xF3];

    let storage_key: [u8; 32] = [0xDD; 32];
    let storage_val: [u8; 32] = {
        let mut v = [0u8; 32];
        v[31] = 42;
        v
    };

    let batch = StateSyncBatch {
        accounts: vec![(addr_bytes, Some(acc))],
        storage: vec![(addr_bytes, storage_key, storage_val)],
        code: vec![(code_hash, code_bytes.clone())],
    };

    db.apply_batch(batch, 5);

    // Verify account via trait (overlay empty → falls to reth)
    let address = Address::from_slice(&addr_bytes);
    let read_acc = exec_read_account(&db, &address).expect("account synced");
    assert_eq!(read_acc.nonce, 10);

    // Verify storage
    let read_val = exec_read_storage(&db, &address, &B256::from(storage_key));
    assert_eq!(read_val[31], 42);

    // Verify code exists
    assert!(StateSyncApplierDb::code_exists(&db, &code_hash));

    // finalize_statesync updates latest_finalized
    assert!(db.finalize_statesync(5));
    assert_eq!(db.get_latest_finalized_version(), SeqNum(5));
}

// ---------------------------------------------------------------------------
// Test 8: State sync traversal
// ---------------------------------------------------------------------------
#[test]
fn test_statesync_traverse() {
    let (_dir, mut db) = tmp_db();

    let addr_bytes: [u8; 20] = [0x66; 20];
    let acc = make_account(7, 12345);

    let batch = StateSyncBatch {
        accounts: vec![(addr_bytes, Some(acc))],
        storage: vec![],
        code: vec![],
    };
    db.apply_batch(batch, 1);
    db.finalize_statesync(1);

    assert!(db.has_version(1));
    assert!(!db.has_version(2));

    let mut accounts_found = 0u32;
    db.traverse_state(&[], 0, 1, &mut |upsert_type, _data| {
        if matches!(upsert_type, monad_execution_engine::statesync::types::SyncUpsertType::Account) {
            accounts_found += 1;
        }
    });
    assert!(accounts_found >= 1, "should find at least one account");
}

// ---------------------------------------------------------------------------
// Test 9: Account deletion (destroy)
// ---------------------------------------------------------------------------
#[test]
fn test_account_destroy() {
    let (_dir, mut db) = tmp_db();

    let header0 = make_header(0);
    let addr = Address::from([0x77; 20]);
    let acc = make_account(1, 1000);
    let genesis_deltas = make_state_deltas(addr, None, Some(acc), vec![]);
    db.init_genesis(&header0, &genesis_deltas, &CodeMap::new())
        .expect("genesis");

    // Verify account exists
    let read = exec_read_account(&db, &addr).expect("exists after genesis");
    assert_eq!(read.nonce, 1);

    // Block 1: destroy account
    let header1 = make_header(1);
    let block1_id = make_block_id(1);
    let destroy_deltas = make_state_deltas(addr, Some(acc), None, vec![]);

    db.set_block_and_prefix(SeqNum(0), make_block_id(0));
    db.commit(block1_id, &header1, deltas_to_bundle(&destroy_deltas), &[], &[]);
    db.finalize(SeqNum(1), block1_id);

    // Account should be gone
    let read = exec_read_account(&db, &addr);
    assert!(read.is_none(), "account should be destroyed");
}

// ---------------------------------------------------------------------------
// Test 10: Storage persistence across multiple blocks
// ---------------------------------------------------------------------------
#[test]
fn test_storage_multi_block() {
    let (_dir, mut db) = tmp_db();

    let header0 = make_header(0);
    db.init_genesis(&header0, &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let addr = Address::from([0x88; 20]);
    let slot1 = B256::from([0x01; 32]);
    let slot2 = B256::from([0x02; 32]);
    let val_a = B256::from([0xAA; 32]);
    let val_b = B256::from([0xBB; 32]);

    let acc = make_account(1, 100);

    // Block 1: set slot1 = val_a
    let header1 = make_header(1);
    let block1_id = make_block_id(1);
    let deltas1 = make_state_deltas(addr, None, Some(acc), vec![(slot1, B256::ZERO, val_a)]);
    db.set_block_and_prefix(SeqNum(0), make_block_id(0));
    db.commit(block1_id, &header1, deltas_to_bundle(&deltas1), &[], &[]);
    db.finalize(SeqNum(1), block1_id);

    // Block 2: set slot2 = val_b, slot1 unchanged
    let acc2 = make_account(2, 200);
    let header2 = make_header(2);
    let block2_id = make_block_id(2);
    let deltas2 = make_state_deltas(addr, Some(acc), Some(acc2), vec![(slot2, B256::ZERO, val_b)]);
    db.set_block_and_prefix(SeqNum(1), block1_id);
    db.commit(block2_id, &header2, deltas_to_bundle(&deltas2), &[], &[]);
    db.finalize(SeqNum(2), block2_id);

    // Both slots should be readable
    assert_eq!(exec_read_storage(&db, &addr, &slot1), val_a);
    assert_eq!(exec_read_storage(&db, &addr, &slot2), val_b);
}

// ---------------------------------------------------------------------------
// Test 11a: Verify raw MDBX persistence across open/close
// ---------------------------------------------------------------------------
#[test]
fn test_raw_mdbx_persistence() {
    use reth_db_api::{cursor::DbCursorRO, database::Database, tables, transaction::{DbTx, DbTxMut}};
    use reth_db::{mdbx::DatabaseArguments, ClientVersion};

    let dir = tempfile::tempdir().expect("create tempdir");
    let mdbx_path = dir.path().join("mdbx");
    std::fs::create_dir_all(&mdbx_path).unwrap();

    // Write
    {
        let db = reth_db::init_db(&mdbx_path, DatabaseArguments::new(ClientVersion::default())).unwrap();
        let tx = db.tx_mut().expect("tx_mut");
        tx.put::<tables::CanonicalHeaders>(0, B256::from([0xAA; 32])).unwrap();
        tx.put::<tables::CanonicalHeaders>(1, B256::from([0xBB; 32])).unwrap();
        tx.commit().unwrap();
    }

    // Read in new env
    {
        let db = reth_db::init_db(&mdbx_path, DatabaseArguments::new(ClientVersion::default())).unwrap();
        let tx = db.tx().expect("tx");
        let mut cursor = tx.cursor_read::<tables::CanonicalHeaders>().unwrap();
        let last = cursor.last().unwrap();
        assert_eq!(last.unwrap().0, 1, "block 1 should survive reopen");
    }
}

// ---------------------------------------------------------------------------
// Test 11b: Verify ProviderFactory-level MDBX persistence
// ---------------------------------------------------------------------------
#[test]
fn test_provider_factory_persistence() {
    use reth_db_api::{cursor::DbCursorRO, tables, transaction::{DbTx, DbTxMut}};
    use reth_provider::DatabaseProviderFactory;
    use reth_storage_api::DBProvider;

    let dir = tempfile::tempdir().expect("create tempdir");

    // Write via ProviderFactory
    {
        let db = RethExecutionDb::open(dir.path(), Arc::clone(&*DEV)).expect("open db");
        let factory = db.factory();
        let provider_rw = factory.database_provider_rw().expect("provider_rw");
        provider_rw.tx_ref().put::<tables::CanonicalHeaders>(0, B256::from([0xAA; 32])).unwrap();
        provider_rw.tx_ref().put::<tables::CanonicalHeaders>(1, B256::from([0xBB; 32])).unwrap();
        DBProvider::commit(provider_rw).expect("commit");

        // Verify visible in same session
        let prov_ro = factory.database_provider_ro().expect("ro");
        let mut c = prov_ro.tx_ref().cursor_read::<tables::CanonicalHeaders>().unwrap();
        let last = c.last().unwrap();
        eprintln!("[factory] same-session: {:?}", last.as_ref().map(|(n, _)| n));
        assert_eq!(last.unwrap().0, 1, "visible in same session");
    }

    // Read via new ProviderFactory
    {
        let db2 = RethExecutionDb::open(dir.path(), Arc::clone(&*DEV)).expect("reopen db");
        let factory2 = db2.factory();
        let prov_ro = factory2.database_provider_ro().expect("ro");
        let mut c = prov_ro.tx_ref().cursor_read::<tables::CanonicalHeaders>().unwrap();
        let last = c.last().unwrap();
        eprintln!("[factory] after-reopen: {:?}", last.as_ref().map(|(n, _)| n));
        assert_eq!(last.unwrap().0, 1, "visible after reopen");
    }
}

// ---------------------------------------------------------------------------
// Test 11: Restart recovery — reopen DB and verify state survives
// ---------------------------------------------------------------------------
#[test]
fn test_restart_recovery() {
    let (dir, mut db) = tmp_db();

    let addr = Address::from([0x99; 20]);
    let acc = make_account(5, 50000);
    let slot = B256::from([0x10; 32]);
    let val = B256::from([0x20; 32]);

    let genesis_deltas = make_state_deltas(addr, None, Some(acc), vec![(slot, B256::ZERO, val)]);
    db.init_genesis(&make_header(0), &genesis_deltas, &CodeMap::new())
        .expect("genesis");

    // Commit and finalize block 1
    let acc2 = make_account(6, 60000);
    let deltas1 = make_state_deltas(addr, Some(acc), Some(acc2), vec![]);
    db.set_block_and_prefix(SeqNum(0), make_block_id(0));
    db.commit(make_block_id(1), &make_header(1), deltas_to_bundle(&deltas1), &[], &[]);
    db.finalize(SeqNum(1), make_block_id(1));

    assert_eq!(db.get_latest_finalized_version(), SeqNum(1));

    // Verify block 1 account is readable (could be from overlay or reth)
    let acc_check = exec_read_account(&db, &addr).expect("account after finalize");
    assert_eq!(acc_check.nonce, 6);

    drop(db);
    let db2 = reopen_db(&dir);

    // latest_finalized must survive restart
    assert_eq!(db2.get_latest_finalized_version(), SeqNum(1));

    // Account state must survive restart
    let read = exec_read_account(&db2, &addr).expect("account survives restart");
    assert_eq!(read.nonce, 6);
    assert_eq!(read.balance, U256::from(60000));

    // Storage must survive restart
    assert_eq!(exec_read_storage(&db2, &addr, &slot), val);

    // Header must survive restart
    let h = db2.read_eth_header();
    assert_eq!(h.number, 1);
}

// ---------------------------------------------------------------------------
// Test 12: Non-existent account returns None, non-existent storage returns ZERO
// ---------------------------------------------------------------------------
#[test]
fn test_read_nonexistent() {
    let (_dir, db) = tmp_db();
    db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let ghost_addr = Address::from([0xDE; 20]);
    assert!(exec_read_account(&db, &ghost_addr).is_none());
    assert_eq!(exec_read_storage(&db, &ghost_addr, &B256::from([0x01; 32])), B256::ZERO);
}

// ---------------------------------------------------------------------------
// Test 13: Genesis with state + storage verifies MDBX round-trip
// ---------------------------------------------------------------------------
#[test]
fn test_genesis_with_full_state() {
    let (dir, db) = tmp_db();

    let addr1 = Address::from([0xA1; 20]);
    let addr2 = Address::from([0xA2; 20]);
    let acc1 = make_account(0, 1_000_000);
    let acc2 = EthAccount {
        nonce: 0,
        balance: U256::from(2_000_000),
        code_hash: Some([0xCC; 32]),
        is_delegated: false,
    };

    let slot = B256::from([0x01; 32]);
    let val = B256::from([0xFF; 32]);

    let mut deltas = make_state_deltas(addr1, None, Some(acc1), vec![(slot, B256::ZERO, val)]);
    deltas.insert(addr2, AccountDelta {
        old_account: None,
        new_account: Some(acc2),
        storage: HashMap::new(),
        ..Default::default()
    });

    let mut code = CodeMap::new();
    code.insert(B256::from([0xCC; 32]), vec![0x60, 0x00]);

    db.init_genesis(&make_header(0), &deltas, &code).expect("genesis");

    // Verify both accounts
    let r1 = exec_read_account(&db, &addr1).expect("addr1");
    assert_eq!(r1.balance, U256::from(1_000_000));
    let r2 = exec_read_account(&db, &addr2).expect("addr2");
    assert_eq!(r2.balance, U256::from(2_000_000));

    // Verify storage
    assert_eq!(exec_read_storage(&db, &addr1, &slot), val);

    // Verify survives restart
    drop(db);
    let db2 = reopen_db(&dir);
    let r1 = exec_read_account(&db2, &addr1).expect("addr1 after restart");
    assert_eq!(r1.balance, U256::from(1_000_000));
    let r2 = exec_read_account(&db2, &addr2).expect("addr2 after restart");
    assert_eq!(r2.balance, U256::from(2_000_000));
    assert_eq!(exec_read_storage(&db2, &addr1, &slot), val);
}

// ---------------------------------------------------------------------------
// Test 14: Overlay isolation — uncommitted block is invisible after finalize
// ---------------------------------------------------------------------------
#[test]
fn test_overlay_fork_isolation() {
    let (_dir, mut db) = tmp_db();
    db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let addr = Address::from([0xBB; 20]);
    let genesis_id = make_block_id(0);

    // Fork A: block 1A creates account with nonce=10
    let acc_a = make_account(10, 100);
    let block_a = make_block_id(0xA1);
    db.set_block_and_prefix(SeqNum(0), genesis_id);
    db.commit(block_a, &make_header(1), deltas_to_bundle(&make_state_deltas(addr, None, Some(acc_a), vec![])), &[], &[]);

    // Fork B: block 1B creates account with nonce=20
    let acc_b = make_account(20, 200);
    let block_b = make_block_id(0xB1);
    db.set_block_and_prefix(SeqNum(0), genesis_id);
    db.commit(block_b, &make_header(1), deltas_to_bundle(&make_state_deltas(addr, None, Some(acc_b), vec![])), &[], &[]);

    // Reading from fork A perspective sees nonce=10
    db.set_block_and_prefix(SeqNum(1), block_a);
    assert_eq!(exec_read_account(&db, &addr).unwrap().nonce, 10);

    // Reading from fork B perspective sees nonce=20
    db.set_block_and_prefix(SeqNum(1), block_b);
    assert_eq!(exec_read_account(&db, &addr).unwrap().nonce, 20);

    // Finalize fork A — fork B's data should NOT be persisted
    db.finalize(SeqNum(1), block_a);
    let persisted = exec_read_account(&db, &addr).expect("fork A persisted");
    assert_eq!(persisted.nonce, 10);
}

// ---------------------------------------------------------------------------
// Test 15: has_executed correctly distinguishes finalized vs overlay
// ---------------------------------------------------------------------------
#[test]
fn test_has_executed_semantics() {
    let (_dir, mut db) = tmp_db();
    db.init_genesis(&make_header(0), &StateDeltas::new(), &CodeMap::new())
        .expect("genesis");

    let block1 = make_block_id(1);
    let block_fake = make_block_id(0xFF);

    // Before commit: not executed
    assert!(!db.has_executed(&block1, SeqNum(1)));

    // After commit: in overlay
    db.set_block_and_prefix(SeqNum(0), make_block_id(0));
    db.commit(block1, &make_header(1), BundleState::default(), &[], &[]);
    assert!(db.has_executed(&block1, SeqNum(1)));

    // Fake block at same seq: not executed
    assert!(!db.has_executed(&block_fake, SeqNum(1)));

    // After finalize: still reports executed (via seq_num <= latest_finalized)
    db.finalize(SeqNum(1), block1);
    assert!(db.has_executed(&block1, SeqNum(1)));
    // Any block_id at finalized seq reports true (by design — finalized means canonical)
    assert!(db.has_executed(&block_fake, SeqNum(1)));
}
