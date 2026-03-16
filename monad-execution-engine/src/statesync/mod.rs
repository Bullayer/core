// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

pub mod client_context;
pub mod deletion_tracker;
pub mod protocol;
pub mod server;
pub mod types;
pub mod version;

use alloy_consensus::Header;

use self::types::{SyncDone, SyncUpsertType};

/// High-level trait: Live mode handler for peer sync requests.
/// Implemented by composing StateSyncTraversable + FinalizedDeletions.
pub trait StateSyncProvider: Send + Sync {
    fn handle_sync_request(
        &self,
        request: &StateSyncRequest,
    ) -> Result<StateSyncResult, StateSyncError>;
}

/// High-level trait: Sync mode applier for received peer upserts.
/// Implemented by StateSyncClientContext, internally using StateSyncApplierDb.
pub trait StateSyncApplier: Send + Sync {
    fn apply_upsert(&mut self, prefix: u64, upsert_type: SyncUpsertType, data: &[u8]) -> bool;
    fn set_target(&mut self, target_header: &Header);
    fn handle_done(&mut self, done: SyncDone);
    fn has_reached_target(&self) -> bool;
    fn finalize(&mut self) -> bool;
}

/// Bottom-level DB trait for server-side TrieDB traversal (mock with in-memory).
pub trait StateSyncTraversable: Send + Sync {
    fn has_version(&self, target: u64) -> bool;
    fn read_block_header_at(&self, version: u64) -> Option<Vec<u8>>;
    fn traverse_state(
        &self,
        prefix: &[u8],
        from: u64,
        until: u64,
        emit: &mut dyn FnMut(SyncUpsertType, &[u8]),
    ) -> bool;
}

/// Bottom-level DB trait for client-side state writes (mock with in-memory).
pub trait StateSyncApplierDb: Send + Sync {
    fn get_latest_version(&self) -> u64;
    fn read_account(&self, addr: &[u8; 20]) -> Option<monad_eth_types::EthAccount>;
    fn read_storage(&self, addr: &[u8; 20], key: &[u8; 32]) -> [u8; 32];
    fn apply_batch(&mut self, updates: StateSyncBatch, version: u64);
    fn finalize_statesync(&mut self, target: u64) -> bool;
    fn state_root(&self) -> [u8; 32];
    fn code_exists(&self, hash: &[u8; 32]) -> bool;
}

pub use self::types::StateSyncRequest;

#[derive(Debug, Clone)]
pub struct StateSyncResult {
    pub upserts: Vec<(SyncUpsertType, Vec<u8>)>,
    pub done: SyncDone,
}

#[derive(Debug, thiserror::Error)]
pub enum StateSyncError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("version not available: {0}")]
    VersionNotAvailable(u64),
    #[error("internal error: {0}")]
    InternalError(String),
}

#[derive(Debug, Clone, Default)]
pub struct StateSyncBatch {
    pub accounts: Vec<([u8; 20], Option<monad_eth_types::EthAccount>)>,
    pub storage: Vec<([u8; 20], [u8; 32], [u8; 32])>,
    pub code: Vec<([u8; 32], Vec<u8>)>,
}
