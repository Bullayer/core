// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::{Address, B256};

/// Corresponds to C++ monad_sync_request (48 bytes).
#[derive(Clone, Debug)]
pub struct StateSyncRequest {
    pub prefix: u64,
    pub prefix_bytes: u8,
    pub target: u64,
    pub from: u64,
    pub until: u64,
    pub old_target: u64,
}

/// Corresponds to C++ monad_sync_done (24 bytes).
#[derive(Clone, Debug)]
pub struct SyncDone {
    pub success: bool,
    pub prefix: u64,
    pub n: u64,
}

/// Deletion entry: address + optional storage key.
#[derive(Clone, Debug)]
pub struct Deletion {
    pub address: Address,
    pub key: Option<B256>,
}

/// Corresponds to C++ monad_sync_type upsert types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncUpsertType {
    Code,
    Account,
    Storage,
    AccountDelete,
    StorageDelete,
    Header,
}
