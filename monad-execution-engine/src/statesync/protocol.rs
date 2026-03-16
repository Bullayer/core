// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Rewrite of C++ statesync_protocol.cpp.
// Pure protocol logic: build_sync_request, handle_upsert, account_update, storage_update.
// Closely mirrors the C++ implementation.

use std::collections::{HashMap, HashSet};

use alloy_primitives::U256;
use tiny_keccak::{Hasher, Keccak};

use super::types::{StateSyncRequest, SyncUpsertType};
use alloy_consensus::Header;

use monad_eth_types::EthAccount;

const COMMIT_INTERVAL: u64 = 1 << 20;

/// State maintained during sync protocol processing.
pub struct SyncProtocolState {
    pub deltas: HashMap<[u8; 20], Option<AccountDelta>>,
    pub buffered: HashMap<[u8; 20], HashMap<[u8; 32], [u8; 32]>>,
    pub code: HashMap<[u8; 32], Vec<u8>>,
    pub seen_code: HashSet<[u8; 32]>,
    pub headers: [Option<Header>; 256],
    pub n_upserts: u64,
    pub needs_commit: bool,
    /// Upserts deferred until after the next commit (incarnation case).
    /// C++ handles this via recursive calls after ctx->commit().
    pub pending_retry: Vec<(SyncUpsertType, Vec<u8>)>,
}

#[derive(Clone, Debug)]
pub struct AccountDelta {
    pub account: Option<EthAccount>,
    pub storage: HashMap<[u8; 32], [u8; 32]>,
}

impl SyncProtocolState {
    pub fn new() -> Self {
        Self {
            deltas: HashMap::new(),
            buffered: HashMap::new(),
            code: HashMap::new(),
            seen_code: HashSet::new(),
            headers: std::array::from_fn(|_| None),
            n_upserts: 0,
            needs_commit: false,
            pending_retry: Vec::new(),
        }
    }

    pub fn should_commit(&self) -> bool {
        self.needs_commit || (self.n_upserts > 0 && self.n_upserts % COMMIT_INTERVAL == 0)
    }

    pub fn clear_commit_flag(&mut self) {
        self.needs_commit = false;
    }
}

impl Default for SyncProtocolState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a sync request for a given prefix and target.
/// Corresponds to C++ StatesyncProtocolV1::send_request (L164-181).
pub fn build_sync_request(
    prefix: u64,
    prefix_bytes: u8,
    target: u64,
    progress: u64,
    old_target: u64,
) -> StateSyncRequest {
    let from = if progress == u64::MAX {
        0
    } else {
        progress + 1
    };

    let until = if from >= target * 99 / 100 {
        target
    } else {
        target * 99 / 100
    };

    StateSyncRequest {
        prefix,
        prefix_bytes,
        target,
        from,
        until,
        old_target,
    }
}

/// Handle a single upsert received from a peer.
/// Dispatches to the correct handler based on upsert type.
/// Corresponds to C++ StatesyncProtocolV1::handle_upsert (L183-244).
pub fn handle_upsert(
    state: &mut SyncProtocolState,
    upsert_type: SyncUpsertType,
    data: &[u8],
    db: &dyn super::StateSyncApplierDb,
) -> bool {
    let result = match upsert_type {
        SyncUpsertType::Code => {
            // C++ L188-190: code.emplace(keccak256(raw), raw)
            // Note: C++ does NOT add to seen_code here; seen_code is populated
            // only in account_update when code_hash != ZERO.
            let hash = keccak256_bytes(data);
            state.code.insert(hash, data.to_vec());
            true
        }
        SyncUpsertType::Account => {
            // C++ L192-199: decode_account_db(raw)
            let (addr, account) = match decode_account_db(data) {
                Some(v) => v,
                None => return false,
            };
            account_update(state, addr, Some(account), db)
        }
        SyncUpsertType::Storage => {
            // C++ L201-211
            if data.len() < 20 {
                return false;
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&data[..20]);
            let (key, value) = match decode_storage_db(&data[20..]) {
                Some(v) => v,
                None => return false,
            };
            storage_update(state, addr, key, value, db)
        }
        SyncUpsertType::AccountDelete => {
            // C++ L213-217: size must be exactly sizeof(Address)
            if data.len() != 20 {
                return false;
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&data[..20]);
            account_update(state, addr, None, db)
        }
        SyncUpsertType::StorageDelete => {
            // C++ L219-228
            if data.len() < 20 {
                return false;
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&data[..20]);
            let key = match decode_storage_key(&data[20..]) {
                Some(k) => k,
                None => return false,
            };
            storage_update(state, addr, key, [0u8; 32], db)
        }
        SyncUpsertType::Header => {
            // C++ L231-237: returns false on decode failure
            match decode_block_header_from_bytes(data) {
                Some(h) => {
                    let idx = h.number as usize % 256;
                    state.headers[idx] = Some(h);
                    true
                }
                None => false,
            }
        }
    };

    // C++ L239-241: commit every 1<<20 upserts
    state.n_upserts += 1;
    if state.n_upserts % COMMIT_INTERVAL == 0 {
        state.needs_commit = true;
    }

    result
}

/// Core account state merge logic. Corresponds to C++ account_update (L39-94).
fn account_update(
    state: &mut SyncProtocolState,
    addr: [u8; 20],
    account: Option<EthAccount>,
    db: &dyn super::StateSyncApplierDb,
) -> bool {
    // C++ L46-49: track code_hash in seen_code
    if let Some(ref acct) = account {
        if let Some(hash) = acct.code_hash {
            if hash != [0u8; 32] {
                state.seen_code.insert(hash);
            }
        }
    }

    let updated = state.deltas.contains_key(&addr);

    // C++ L55-67: addr is in buffered (received storage before account)
    if state.buffered.contains_key(&addr) {
        assert!(db.read_account(&addr).is_none() && !updated);
        if let Some(acct) = account {
            let buffered_storage = state.buffered.remove(&addr).unwrap();
            state.deltas.insert(
                addr,
                Some(AccountDelta {
                    account: Some(acct),
                    storage: buffered_storage,
                }),
            );
        } else {
            state.buffered.remove(&addr);
        }
        return true;
    }

    if !updated {
        // C++ L68-79: not yet tracked
        if let Some(acct) = account {
            state.deltas.insert(
                addr,
                Some(AccountDelta {
                    account: Some(acct),
                    storage: HashMap::new(),
                }),
            );
        } else if db.read_account(&addr).is_some() {
            state.deltas.insert(addr, None);
        }
    } else {
        let existing = state.deltas.get(&addr);
        match (account, existing) {
            // C++ L81-84: incarnation — existing is None (deleted), new is Some.
            // C++ calls commit() first to flush the deletion, then recursively
            // calls account_update. We defer the re-insert to pending_retry.
            (Some(acct), Some(None)) => {
                state.needs_commit = true;
                // Re-encode the account for retry after commit.
                let mut retry_data = Vec::with_capacity(92);
                retry_data.extend_from_slice(&addr);
                retry_data.extend_from_slice(&acct.nonce.to_be_bytes());
                let balance_bytes: [u8; 32] = acct.balance.to_be_bytes();
                retry_data.extend_from_slice(&balance_bytes);
                retry_data.extend_from_slice(&acct.code_hash.unwrap_or_default());
                state
                    .pending_retry
                    .push((SyncUpsertType::Account, retry_data));
            }
            // C++ L85-86: existing is Some, update account
            (Some(acct), Some(Some(_))) => {
                if let Some(Some(delta)) = state.deltas.get_mut(&addr) {
                    delta.account = Some(acct);
                }
            }
            // C++ L88-90: account deletion
            (None, _) => {
                if db.read_account(&addr).is_some() {
                    state.deltas.insert(addr, None);
                } else {
                    // C++ L92: erase
                    state.deltas.remove(&addr);
                }
            }
            _ => {}
        }
    }
    true
}

/// Core storage state merge logic. Corresponds to C++ storage_update (L96-158).
fn storage_update(
    state: &mut SyncProtocolState,
    addr: [u8; 20],
    key: [u8; 32],
    value: [u8; 32],
    db: &dyn super::StateSyncApplierDb,
) -> bool {
    let zero = [0u8; 32];
    let updated = state.deltas.contains_key(&addr);

    // C++ L105-121: addr is in buffered
    if state.buffered.contains_key(&addr) {
        assert!(db.read_account(&addr).is_none() && !updated);
        if value == zero {
            // C++ L108-111: erase from buffered
            if let Some(buf) = state.buffered.get_mut(&addr) {
                buf.remove(&key);
                if buf.is_empty() {
                    state.buffered.remove(&addr);
                }
            }
        } else {
            state
                .buffered
                .entry(addr)
                .or_default()
                .insert(key, value);
        }
        return true;
    }

    if value != zero || db.read_storage(&addr, &key) != zero {
        // C++ L123-153
        if updated {
            match state.deltas.get_mut(&addr) {
                Some(Some(delta)) => {
                    delta.storage.insert(key, value);
                }
                // C++ L130-132: incarnation — deleted account, non-zero value
                Some(None) if value != zero => {
                    state.needs_commit = true;
                    // After commit, re-run (simplified: buffer it)
                    state
                        .buffered
                        .entry(addr)
                        .or_default()
                        .insert(key, value);
                }
                _ => {}
            }
        } else {
            // C++ L136-151
            if let Some(acct) = db.read_account(&addr) {
                state.deltas.insert(
                    addr,
                    Some(AccountDelta {
                        account: Some(acct),
                        storage: HashMap::from([(key, value)]),
                    }),
                );
            } else {
                // C++ L147-149: buffer for later (no account yet)
                assert!(value != zero);
                state
                    .buffered
                    .entry(addr)
                    .or_default()
                    .insert(key, value);
            }
        }
    } else if updated {
        // C++ L154-157: value == zero, erase from delta
        if let Some(Some(delta)) = state.deltas.get_mut(&addr) {
            delta.storage.remove(&key);
        }
    }
    true
}

/// Decode account from TrieDB-style bytes: [addr(20) | nonce(8) | balance(32) | code_hash(32)].
/// For mock phase, we decode as much as available.
fn decode_account_db(data: &[u8]) -> Option<([u8; 20], EthAccount)> {
    if data.len() < 20 {
        return None;
    }
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&data[..20]);

    let rest = &data[20..];
    let nonce = if rest.len() >= 8 {
        u64::from_be_bytes(rest[..8].try_into().unwrap())
    } else {
        0
    };

    let balance = if rest.len() >= 40 {
        U256::from_be_slice(&rest[8..40])
    } else {
        U256::ZERO
    };

    let code_hash = if rest.len() >= 72 {
        Some(rest[40..72].try_into().unwrap())
    } else {
        None
    };

    Some((
        addr,
        EthAccount {
            nonce,
            balance,
            code_hash,
            is_delegated: false,
        },
    ))
}

/// Decode storage key-value from bytes.
fn decode_storage_db(data: &[u8]) -> Option<([u8; 32], [u8; 32])> {
    if data.len() < 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&data[..32]);

    let mut value = [0u8; 32];
    if data.len() >= 64 {
        value.copy_from_slice(&data[32..64]);
    }

    Some((key, value))
}

/// Decode storage key from compact encoding (for delete upserts).
fn decode_storage_key(data: &[u8]) -> Option<[u8; 32]> {
    if data.len() < 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&data[..32]);
    Some(key)
}

fn decode_block_header_from_bytes(_data: &[u8]) -> Option<Header> {
    // TODO: implement proper RLP decoding of block header
    Some(Header::default())
}

fn keccak256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    let mut output = [0u8; 32];
    hasher.update(data);
    hasher.finalize(&mut output);
    output
}
