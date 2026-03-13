// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use std::collections::{HashMap, HashSet};

use alloy_primitives::{Address, B256};

use crate::statesync::{StateSyncApplierDb, StateSyncBatch, StateSyncTraversable};
use crate::statesync::types::SyncUpsertType;
use crate::traits::ExecutionDb;
use crate::types::{Account, BlockHeader, CodeMap, Receipt, StateDeltas, Transaction};

/// In-memory implementation of ExecutionDb + StateSyncTraversable + StateSyncApplierDb.
/// Uses HashMaps for state, supports propose/finalize semantics.
pub struct InMemoryExecutionDb {
    accounts: HashMap<Address, Account>,
    storage: HashMap<(Address, B256), B256>,
    code: HashMap<B256, Vec<u8>>,
    headers: HashMap<u64, BlockHeader>,
    executed_blocks: HashSet<(B256, u64)>,
    latest_finalized: u64,
    latest_verified: u64,
    current_block_number: u64,
    current_block_id: B256,
}

impl InMemoryExecutionDb {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            storage: HashMap::new(),
            code: HashMap::new(),
            headers: HashMap::new(),
            executed_blocks: HashSet::new(),
            latest_finalized: 0,
            latest_verified: 0,
            current_block_number: 0,
            current_block_id: B256::ZERO,
        }
    }

    pub fn with_finalized(finalized: u64) -> Self {
        let mut db = Self::new();
        db.latest_finalized = finalized;
        db
    }
}

impl Default for InMemoryExecutionDb {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionDb for InMemoryExecutionDb {
    fn has_executed(&self, block_id: &B256, seq_num: u64) -> bool {
        self.executed_blocks.contains(&(*block_id, seq_num))
    }

    fn get_latest_finalized_version(&self) -> u64 {
        self.latest_finalized
    }

    fn read_account(&self, address: &Address) -> Option<Account> {
        self.accounts.get(address).cloned()
    }

    fn read_storage(&self, address: &Address, slot: &B256) -> B256 {
        self.storage
            .get(&(*address, *slot))
            .copied()
            .unwrap_or(B256::ZERO)
    }

    fn read_eth_header(&self) -> BlockHeader {
        self.headers
            .get(&self.current_block_number)
            .cloned()
            .unwrap_or_default()
    }

    fn set_block_and_prefix(&mut self, block_number: u64, block_id: B256) {
        self.current_block_number = block_number;
        self.current_block_id = block_id;
    }

    fn commit(
        &mut self,
        block_id: B256,
        header: &BlockHeader,
        state_deltas: &StateDeltas,
        code: &CodeMap,
        _receipts: &[Receipt],
        _senders: &[Address],
        _transactions: &[Transaction],
    ) {
        // Apply state deltas
        for (address, delta) in state_deltas {
            if let Some(new_acct) = &delta.new_account {
                self.accounts.insert(*address, new_acct.clone());
            } else if delta.old_account.is_some() && delta.new_account.is_none() {
                self.accounts.remove(address);
            }
            for (slot, storage_delta) in &delta.storage {
                if storage_delta.new_value == B256::ZERO {
                    self.storage.remove(&(*address, *slot));
                } else {
                    self.storage
                        .insert((*address, *slot), storage_delta.new_value);
                }
            }
        }

        // Store code
        for (hash, data) in code {
            self.code.insert(*hash, data.clone());
        }

        // Store header
        self.headers.insert(header.number, header.clone());
        self.executed_blocks.insert((block_id, header.number));
    }

    fn finalize(&mut self, block_number: u64, _block_id: B256) {
        self.latest_finalized = block_number;
    }

    fn update_voted_metadata(&mut self, _block_number: u64, _block_id: B256) {}
    fn update_proposed_metadata(&mut self, _block_number: u64, _block_id: B256) {}
    fn update_verified_block(&mut self, block_number: u64) {
        self.latest_verified = block_number;
    }
}

impl StateSyncTraversable for InMemoryExecutionDb {
    fn has_version(&self, target: u64) -> bool {
        target <= self.latest_finalized
    }

    fn read_block_header_at(&self, version: u64) -> Option<Vec<u8>> {
        self.headers.get(&version).map(|_h| {
            // Return minimal mock header bytes
            vec![0u8; 32]
        })
    }

    fn traverse_state(
        &self,
        prefix: &[u8],
        _from: u64,
        _until: u64,
        emit: &mut dyn FnMut(SyncUpsertType, &[u8]),
    ) -> bool {
        // Mock traversal: emit accounts whose keccak256(addr) starts with prefix
        for (addr, account) in &self.accounts {
            let addr_bytes = addr.as_slice();
            if !prefix.is_empty() {
                let mut hasher = tiny_keccak::Keccak::v256();
                let mut hash = [0u8; 32];
                tiny_keccak::Hasher::update(&mut hasher, addr_bytes);
                tiny_keccak::Hasher::finalize(hasher, &mut hash);
                if !hash.starts_with(prefix) {
                    continue;
                }
            }

            // Encode: addr(20) + nonce(8) + balance(32) + code_hash(32) = 92 bytes
            let mut data = Vec::with_capacity(92);
            data.extend_from_slice(addr_bytes);
            data.extend_from_slice(&account.nonce.to_be_bytes());
            let balance_bytes: [u8; 32] = account.balance.to_be_bytes();
            data.extend_from_slice(&balance_bytes);
            data.extend_from_slice(account.code_hash.as_slice());
            emit(SyncUpsertType::Account, &data);
        }

        for ((addr, key), value) in &self.storage {
            let addr_bytes = addr.as_slice();
            if !prefix.is_empty() && addr_bytes[0] != prefix[0] {
                continue;
            }

            let mut data = Vec::with_capacity(84);
            data.extend_from_slice(addr_bytes);
            data.extend_from_slice(key.as_slice());
            data.extend_from_slice(value.as_slice());
            emit(SyncUpsertType::Storage, &data);
        }

        true
    }
}

impl StateSyncApplierDb for InMemoryExecutionDb {
    fn get_latest_version(&self) -> u64 {
        self.latest_finalized
    }

    fn read_account(&self, addr: &[u8; 20]) -> Option<Account> {
        let address = Address::from_slice(addr);
        self.accounts.get(&address).cloned()
    }

    fn read_storage(&self, addr: &[u8; 20], key: &[u8; 32]) -> [u8; 32] {
        let address = Address::from_slice(addr);
        let slot = B256::from_slice(key);
        ExecutionDb::read_storage(self, &address, &slot).0
    }

    fn apply_batch(&mut self, updates: StateSyncBatch, _version: u64) {
        for (addr, account_opt) in updates.accounts {
            let address = Address::from_slice(&addr);
            match account_opt {
                Some(account) => {
                    self.accounts.insert(address, account);
                }
                None => {
                    self.accounts.remove(&address);
                }
            }
        }

        for (addr, key, value) in updates.storage {
            let address = Address::from_slice(&addr);
            let slot = B256::from_slice(&key);
            let val = B256::from_slice(&value);
            if val == B256::ZERO {
                self.storage.remove(&(address, slot));
            } else {
                self.storage.insert((address, slot), val);
            }
        }

        for (hash, data) in updates.code {
            self.code.insert(B256::from_slice(&hash), data);
        }
    }

    fn finalize_statesync(&mut self, target: u64) -> bool {
        self.latest_finalized = target;
        true
    }

    fn state_root(&self) -> [u8; 32] {
        // Mock: return zero state root
        [0u8; 32]
    }

    fn code_exists(&self, hash: &[u8; 32]) -> bool {
        self.code.contains_key(&B256::from_slice(hash))
    }
}
