use alloy_primitives::{keccak256, Address, B256, U256};
use reth_db_api::cursor::DbCursorRO;
use reth_db_api::tables;
use reth_db_api::transaction::{DbTx, DbTxMut};
use reth_primitives_traits::{Bytecode, StorageEntry};
use reth_provider::DatabaseProviderFactory;
use reth_storage_api::DBProvider;
use reth_trie::StateRoot;
use reth_trie_db::{DatabaseHashedCursorFactory, DatabaseStateRoot, DatabaseTrieCursorFactory, LegacyKeyAdapter};
use tracing::{debug, error};

use monad_execution_engine::statesync::{
    types::SyncUpsertType, StateSyncApplierDb, StateSyncBatch, StateSyncTraversable,
};

use crate::adapter::{RethExecutionDb, RethTraversableDb};
use crate::bridge;

impl StateSyncTraversable for RethExecutionDb {
    fn has_version(&self, target: u64) -> bool {
        target <= self.latest_finalized().0
    }

    fn read_block_header_at(&self, version: u64) -> Option<Vec<u8>> {
        use alloy_rlp::Encodable;
        use monad_types::SeqNum;

        let header = self.reth_read_header(SeqNum(version))?;
        let mut buf = Vec::new();
        header.encode(&mut buf);
        Some(buf)
    }

    fn traverse_state(
        &self,
        prefix: &[u8],
        _from: u64,
        _until: u64,
        emit: &mut dyn FnMut(SyncUpsertType, &[u8]),
    ) -> bool {
        let provider = match self.factory().database_provider_ro() {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "failed to open read provider for traverse_state");
                return false;
            }
        };

        let tx = provider.tx_ref();

        // Traverse accounts
        {
            let mut cursor = match tx.cursor_read::<tables::PlainAccountState>() {
                Ok(c) => c,
                Err(e) => {
                    error!(%e, "failed to open account cursor");
                    return false;
                }
            };

            let mut entry = cursor.first().ok().flatten();
            while let Some((addr, acc)) = entry {
                let hash = keccak256(addr);
                if prefix.is_empty() || hash.as_slice().starts_with(prefix) {
                    let monad_acc = bridge::reth_to_monad_account(&acc);
                    let mut data = Vec::with_capacity(84);
                    data.extend_from_slice(addr.as_slice());
                    data.extend_from_slice(&monad_acc.nonce.to_be_bytes());
                    data.extend_from_slice(&monad_acc.balance.to_be_bytes::<32>());
                    let code_hash = monad_acc.code_hash.unwrap_or([0u8; 32]);
                    data.extend_from_slice(&code_hash);
                    emit(SyncUpsertType::Account, &data);
                }
                entry = cursor.next().ok().flatten();
            }
        }

        // Traverse storage
        {
            let mut cursor = match tx.cursor_dup_read::<tables::PlainStorageState>() {
                Ok(c) => c,
                Err(e) => {
                    error!(%e, "failed to open storage cursor");
                    return false;
                }
            };

            let mut entry: Option<(Address, StorageEntry)> = cursor.first().ok().flatten();
            while let Some((addr, se)) = entry {
                let hash = keccak256(addr);
                if prefix.is_empty() || hash.as_slice().starts_with(prefix) {
                    let mut data = Vec::with_capacity(84);
                    data.extend_from_slice(addr.as_slice());
                    data.extend_from_slice(se.key.as_slice());
                    data.extend_from_slice(&se.value.to_be_bytes::<32>());
                    emit(SyncUpsertType::Storage, &data);
                }
                entry = cursor.next().ok().flatten();
            }
        }

        // Traverse bytecodes
        {
            let mut cursor = match tx.cursor_read::<tables::Bytecodes>() {
                Ok(c) => c,
                Err(e) => {
                    error!(%e, "failed to open bytecodes cursor");
                    return false;
                }
            };

            let mut entry: Option<(B256, Bytecode)> = cursor.first().ok().flatten();
            while let Some((hash, bytecode)) = entry {
                let raw = bytecode.bytes_slice().to_vec();
                let mut data = Vec::with_capacity(32 + raw.len());
                data.extend_from_slice(hash.as_slice());
                data.extend_from_slice(&raw);
                emit(SyncUpsertType::Code, &data);
                entry = cursor.next().ok().flatten();
            }
        }

        true
    }
}

impl StateSyncApplierDb for RethExecutionDb {
    fn get_latest_version(&self) -> u64 {
        self.latest_finalized().0
    }

    fn read_account(&self, addr: &[u8; 20]) -> Option<monad_eth_types::EthAccount> {
        let address = Address::from_slice(addr);
        self.reth_read_account(&address)
    }

    fn read_storage(&self, addr: &[u8; 20], key: &[u8; 32]) -> [u8; 32] {
        let address = Address::from_slice(addr);
        let slot = B256::from_slice(key);
        self.reth_read_storage(&address, &slot).0
    }

    fn apply_batch(&mut self, updates: StateSyncBatch, _version: u64) {
        let provider_rw = match self.factory().database_provider_rw() {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "failed to open rw provider for apply_batch");
                return;
            }
        };

        let tx = provider_rw.tx_ref();

        for (addr_bytes, maybe_acc) in &updates.accounts {
            let address = Address::from_slice(addr_bytes);
            if let Some(acc) = maybe_acc {
                let reth_acc = bridge::monad_to_reth_account(acc);
                if let Err(e) = tx.put::<tables::PlainAccountState>(address, reth_acc) {
                    error!(%e, ?address, "failed to put account");
                }
                // Also write hashed account for trie
                let hashed_addr = keccak256(address);
                if let Err(e) = tx.put::<tables::HashedAccounts>(hashed_addr, reth_acc) {
                    error!(%e, ?address, "failed to put hashed account");
                }
            } else {
                let _ = tx.delete::<tables::PlainAccountState>(address, None);
                let hashed_addr = keccak256(address);
                let _ = tx.delete::<tables::HashedAccounts>(hashed_addr, None);
            }
        }

        for (addr_bytes, key, value) in &updates.storage {
            let address = Address::from_slice(addr_bytes);
            let entry = StorageEntry {
                key: B256::from_slice(key),
                value: U256::from_be_bytes(*value),
            };
            if let Err(e) = tx.put::<tables::PlainStorageState>(address, entry) {
                error!(%e, ?address, "failed to put storage");
            }
            // Also write hashed storage for trie
            let hashed_addr = keccak256(address);
            let hashed_key = keccak256(B256::from_slice(key));
            let hashed_entry = StorageEntry {
                key: hashed_key,
                value: U256::from_be_bytes(*value),
            };
            if let Err(e) = tx.put::<tables::HashedStorages>(hashed_addr, hashed_entry) {
                error!(%e, ?address, "failed to put hashed storage");
            }
        }

        for (hash, bytecode) in &updates.code {
            let code_hash = B256::from_slice(hash);
            let code = Bytecode::new_raw(bytecode.clone().into());
            if let Err(e) = tx.put::<tables::Bytecodes>(code_hash, code) {
                error!(%e, "failed to put bytecode");
            }
        }

        if let Err(e) = DBProvider::commit(provider_rw) {
            error!(%e, "failed to commit apply_batch");
        }
    }

    fn finalize_statesync(&mut self, target: u64) -> bool {
        self.set_latest_finalized(monad_types::SeqNum(target));
        debug!(target_version = target, "finalize_statesync");
        true
    }

    fn state_root(&self) -> [u8; 32] {
        match self.factory().database_provider_ro() {
            Ok(provider) => {
                let tx = provider.tx_ref();
                type DbStateRoot<'a, TX> = StateRoot<
                    DatabaseTrieCursorFactory<&'a TX, LegacyKeyAdapter>,
                    DatabaseHashedCursorFactory<&'a TX>,
                >;
                match <DbStateRoot<'_, _> as DatabaseStateRoot<'_, _>>::from_tx(tx).root() {
                    Ok(root) => root.0,
                    Err(e) => {
                        error!(%e, "state_root computation failed");
                        [0u8; 32]
                    }
                }
            }
            Err(e) => {
                error!(%e, "failed to open provider for state_root");
                [0u8; 32]
            }
        }
    }

    fn code_exists(&self, hash: &[u8; 32]) -> bool {
        let code_hash = B256::from_slice(hash);
        match self.factory().database_provider_ro() {
            Ok(provider) => provider
                .tx_ref()
                .get::<tables::Bytecodes>(code_hash)
                .ok()
                .flatten()
                .is_some(),
            Err(_) => false,
        }
    }

    fn write_block_header(&mut self, version: u64, header_rlp: &[u8]) {
        self.write_block_headers(&[(version, header_rlp.to_vec())]);
    }

    fn write_block_headers(&mut self, headers: &[(u64, Vec<u8>)]) {
        use alloy_rlp::Decodable;
        use alloy_consensus::Header;

        if headers.is_empty() {
            return;
        }

        let provider_rw = match self.factory().database_provider_rw() {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "failed to open rw provider for write_block_headers");
                return;
            }
        };

        let tx = provider_rw.tx_ref();
        for (version, rlp) in headers {
            let header = match Header::decode(&mut &rlp[..]) {
                Ok(h) => h,
                Err(e) => {
                    error!(%e, version, "failed to RLP-decode header");
                    continue;
                }
            };

            let block_hash = alloy_consensus::Sealable::hash_slow(&header);
            if let Err(e) = tx.put::<tables::CanonicalHeaders>(*version, block_hash) {
                error!(%e, version, "failed to put canonical header");
                continue;
            }
            if let Err(e) = tx.put::<tables::HeaderNumbers>(block_hash, *version) {
                error!(%e, version, "failed to put header number");
                continue;
            }
            if let Err(e) = tx.put::<tables::Headers>(*version, header) {
                error!(%e, version, "failed to put header");
            }
        }

        if let Err(e) = DBProvider::commit(provider_rw) {
            error!(%e, "failed to commit write_block_headers");
        }
    }
}

impl StateSyncTraversable for RethTraversableDb {
    fn has_version(&self, target: u64) -> bool {
        let last = crate::adapter::RethExecutionDb::mdbx_last_block_number(&self.factory);
        target <= last.0
    }

    fn read_block_header_at(&self, version: u64) -> Option<Vec<u8>> {
        use alloy_rlp::Encodable;

        let provider = self.factory.database_provider_ro().ok()?;
        let header: alloy_consensus::Header = provider.tx_ref().get::<tables::Headers>(version).ok().flatten()?;
        let mut buf = Vec::new();
        header.encode(&mut buf);
        Some(buf)
    }

    fn traverse_state(
        &self,
        prefix: &[u8],
        _from: u64,
        _until: u64,
        emit: &mut dyn FnMut(SyncUpsertType, &[u8]),
    ) -> bool {
        let provider = match self.factory.database_provider_ro() {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "RethTraversableDb: failed to open read provider");
                return false;
            }
        };

        let tx = provider.tx_ref();

        {
            let mut cursor = match tx.cursor_read::<tables::PlainAccountState>() {
                Ok(c) => c,
                Err(e) => { error!(%e, "account cursor"); return false; }
            };
            let mut entry = cursor.first().ok().flatten();
            while let Some((addr, acc)) = entry {
                let hash = keccak256(addr);
                if prefix.is_empty() || hash.as_slice().starts_with(prefix) {
                    let monad_acc = bridge::reth_to_monad_account(&acc);
                    let mut data = Vec::with_capacity(84);
                    data.extend_from_slice(addr.as_slice());
                    data.extend_from_slice(&monad_acc.nonce.to_be_bytes());
                    data.extend_from_slice(&monad_acc.balance.to_be_bytes::<32>());
                    let code_hash = monad_acc.code_hash.unwrap_or([0u8; 32]);
                    data.extend_from_slice(&code_hash);
                    emit(SyncUpsertType::Account, &data);
                }
                entry = cursor.next().ok().flatten();
            }
        }

        {
            let mut cursor = match tx.cursor_dup_read::<tables::PlainStorageState>() {
                Ok(c) => c,
                Err(e) => { error!(%e, "storage cursor"); return false; }
            };
            let mut entry: Option<(Address, StorageEntry)> = cursor.first().ok().flatten();
            while let Some((addr, se)) = entry {
                let hash = keccak256(addr);
                if prefix.is_empty() || hash.as_slice().starts_with(prefix) {
                    let mut data = Vec::with_capacity(84);
                    data.extend_from_slice(addr.as_slice());
                    data.extend_from_slice(se.key.as_slice());
                    data.extend_from_slice(&se.value.to_be_bytes::<32>());
                    emit(SyncUpsertType::Storage, &data);
                }
                entry = cursor.next().ok().flatten();
            }
        }

        {
            let mut cursor = match tx.cursor_read::<tables::Bytecodes>() {
                Ok(c) => c,
                Err(e) => { error!(%e, "bytecodes cursor"); return false; }
            };
            let mut entry: Option<(B256, Bytecode)> = cursor.first().ok().flatten();
            while let Some((hash, bytecode)) = entry {
                let raw = bytecode.bytes_slice().to_vec();
                let mut data = Vec::with_capacity(32 + raw.len());
                data.extend_from_slice(hash.as_slice());
                data.extend_from_slice(&raw);
                emit(SyncUpsertType::Code, &data);
                entry = cursor.next().ok().flatten();
            }
        }

        true
    }
}
