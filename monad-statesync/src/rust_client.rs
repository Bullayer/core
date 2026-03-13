// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Replaces ffi.rs: receives peer Response, calls StateSyncApplier trait, writes to local state.
// Used in Sync mode after process merge.

use std::sync::Mutex;

use monad_execution_engine::statesync::types::SyncUpsertType;
use monad_execution_engine::statesync::StateSyncApplier;
use monad_executor_glue::{StateSyncResponse, StateSyncUpsertType as GlueUpsertType};

/// Rust-native statesync client that replaces C++ FFI-based ffi::StateSync.
/// Applies received upserts via StateSyncApplier trait instead of C++ FFI calls.
pub(crate) struct RustStateSyncClient {
    applier: Mutex<Box<dyn StateSyncApplier>>,
}

impl RustStateSyncClient {
    pub fn new(applier: Box<dyn StateSyncApplier>) -> Self {
        Self {
            applier: Mutex::new(applier),
        }
    }

    pub fn handle_response(&self, response: &StateSyncResponse) -> bool {
        let mut applier = self.applier.lock().unwrap();

        for upsert in &response.response {
            let upsert_type = convert_upsert_type(upsert.upsert_type);

            if !applier.apply_upsert(response.request.prefix, upsert_type, &upsert.data) {
                tracing::error!(
                    prefix = response.request.prefix,
                    "failed to apply upsert"
                );
                return false;
            }
        }

        true
    }

    pub fn has_reached_target(&self) -> bool {
        self.applier.lock().unwrap().has_reached_target()
    }

    pub fn finalize(&self) -> bool {
        self.applier.lock().unwrap().finalize()
    }
}

fn convert_upsert_type(t: GlueUpsertType) -> SyncUpsertType {
    match t {
        GlueUpsertType::Code => SyncUpsertType::Code,
        GlueUpsertType::Account => SyncUpsertType::Account,
        GlueUpsertType::Storage => SyncUpsertType::Storage,
        GlueUpsertType::AccountDelete => SyncUpsertType::AccountDelete,
        GlueUpsertType::StorageDelete => SyncUpsertType::StorageDelete,
        GlueUpsertType::Header => SyncUpsertType::Header,
    }
}
