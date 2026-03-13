// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Rewrite of C++ statesync_client_context.cpp + statesync_client.cpp.
// StateSyncClientContext: progress tracking, commit, has_reached_target, finalize.
// Closely mirrors the C++ implementation.

use super::protocol::SyncProtocolState;
use super::types::{SyncDone, SyncUpsertType};
use super::{StateSyncApplier, StateSyncApplierDb, StateSyncBatch};
use crate::types::BlockHeader;
use crate::validation::compute_block_hash;

const NUM_PREFIXES: usize = 256;
const INVALID_BLOCK_NUM: u64 = u64::MAX;

pub struct StateSyncClientContext {
    pub db: Box<dyn StateSyncApplierDb>,
    pub protocol: SyncProtocolState,
    /// (progress, old_target) per prefix. C++ initializes with (latest_version, latest_version).
    pub progress: Vec<(u64, u64)>,
    pub target: Option<BlockHeader>,
    pub current: u64,
    /// Prefixes that need re-request after a partial done.
    pub pending_requests: Vec<u64>,
}

impl StateSyncClientContext {
    pub fn new(db: Box<dyn StateSyncApplierDb>) -> Self {
        let latest = db.get_latest_version();
        let initial_progress = if latest == INVALID_BLOCK_NUM {
            INVALID_BLOCK_NUM
        } else {
            latest
        };
        Self {
            db,
            protocol: SyncProtocolState::new(),
            progress: vec![(initial_progress, initial_progress); NUM_PREFIXES],
            target: None,
            current: if latest == INVALID_BLOCK_NUM {
                0
            } else {
                latest + 1
            },
            pending_requests: Vec::new(),
        }
    }

    /// Commit buffered deltas to DB. Corresponds to C++ commit() (L106-196).
    pub fn commit(&mut self) {
        let mut batch = StateSyncBatch::default();

        for (addr, delta_opt) in self.protocol.deltas.drain() {
            match delta_opt {
                Some(delta) => {
                    if let Some(account) = delta.account {
                        batch.accounts.push((addr, Some(account)));
                    }
                    for (key, value) in delta.storage {
                        batch.storage.push((addr, key, value));
                    }
                }
                None => {
                    batch.accounts.push((addr, None));
                }
            }
        }

        for (hash, code_data) in self.protocol.code.drain() {
            batch.code.push((hash, code_data));
        }

        self.db.apply_batch(batch, self.current);
        self.protocol.clear_commit_flag();
    }

    /// Returns list of prefixes that need re-request (drained on call).
    pub fn take_pending_requests(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_requests)
    }
}

impl StateSyncApplier for StateSyncClientContext {
    fn apply_upsert(&mut self, _prefix: u64, upsert_type: SyncUpsertType, data: &[u8]) -> bool {
        let result = super::protocol::handle_upsert(
            &mut self.protocol,
            upsert_type,
            data,
            &*self.db,
        );

        if self.protocol.should_commit() {
            self.commit();

            // Process any deferred retries (e.g., incarnation: account re-creation after deletion).
            // C++ handles this via recursive calls after ctx->commit().
            let retries = std::mem::take(&mut self.protocol.pending_retry);
            for (retry_type, retry_data) in retries {
                super::protocol::handle_upsert(
                    &mut self.protocol,
                    retry_type,
                    &retry_data,
                    &*self.db,
                );
            }

            // Check if retries triggered another commit need
            if self.protocol.should_commit() {
                self.commit();
            }
        }

        result
    }

    fn set_target(&mut self, target_header: &BlockHeader) {
        assert_ne!(target_header.number, INVALID_BLOCK_NUM);
        if let Some(ref old) = self.target {
            assert!(target_header.number >= old.number);
        }

        self.target = Some(target_header.clone());
        let latest = self.db.get_latest_version();
        if target_header.number == latest {
            return;
        }

        // C++ L125-128: send requests for all prefixes
        for i in 0..NUM_PREFIXES {
            self.pending_requests.push(i as u64);
        }
    }

    /// C++ monad_statesync_client_handle_done (statesync_client.cpp L139-156).
    fn handle_done(&mut self, done: SyncDone) {
        assert!(done.success);

        let idx = done.prefix as usize;
        if idx >= NUM_PREFIXES {
            return;
        }

        let (ref mut progress, ref mut old_target) = self.progress[idx];
        assert!(done.n > *progress || *progress == INVALID_BLOCK_NUM);
        *progress = done.n;
        *old_target = self.target.as_ref().map_or(0, |t| t.number);

        // C++ L149-151: re-request if not yet at target
        let target_number = self.target.as_ref().map_or(INVALID_BLOCK_NUM, |t| t.number);
        if *progress != target_number {
            self.pending_requests.push(done.prefix);
        }

        if self.has_reached_target() {
            self.commit();
        }
    }

    /// Check if all 256 prefixes have reached the target.
    /// C++ monad_statesync_client_has_reached_target (statesync_client.cpp L67-81).
    fn has_reached_target(&self) -> bool {
        let target_number = match &self.target {
            Some(t) if t.number != INVALID_BLOCK_NUM => t.number,
            _ => return false,
        };

        self.progress
            .iter()
            .all(|(progress, _)| *progress == target_number)
    }

    /// Finalize sync session.
    /// C++ monad_statesync_client_finalize (statesync_client.cpp L158-222).
    fn finalize(&mut self) -> bool {
        let target = match &self.target {
            Some(t) => t.clone(),
            None => return false,
        };
        assert_ne!(target.number, INVALID_BLOCK_NUM);

        // C++ L162: deltas should be empty after commit
        assert!(self.protocol.deltas.is_empty());

        // C++ L163-166: check for orphaned buffered storage
        if !self.protocol.buffered.is_empty() {
            tracing::error!(
                orphaned_count = self.protocol.buffered.len(),
                "finalize failed: orphaned buffered storage"
            );
            return false;
        }

        // C++ L170-177: verify all seen code exists in DB
        for hash in &self.protocol.seen_code {
            if !self.db.code_exists(hash) {
                tracing::error!(
                    code_hash = alloy_primitives::hex::encode(hash),
                    "finalize failed: missing code"
                );
                return false;
            }
        }

        // C++ L179-214: validate header hash chain
        // Verify each header's hash matches the expected parent_hash
        let num_headers = std::cmp::min(target.number, 256);
        let mut expected_hash = target.parent_hash;
        for i in 0..num_headers {
            let version = target.number - i - 1;
            let idx = version as usize % 256;
            if let Some(ref hdr) = self.protocol.headers[idx] {
                let hash = compute_block_hash(hdr);
                if hash != expected_hash {
                    tracing::error!(
                        version,
                        expected = %expected_hash,
                        actual = %hash,
                        "finalize failed: header hash chain mismatch"
                    );
                    return false;
                }
                expected_hash = hdr.parent_hash;
            } else {
                tracing::warn!(version, "header not available for hash chain verification");
                break;
            }
        }

        // C++ L221: verify state root matches target
        let state_root = self.db.state_root();
        if state_root != target.state_root.0 {
            tracing::error!(
                expected = alloy_primitives::hex::encode(target.state_root.0),
                actual = alloy_primitives::hex::encode(state_root),
                "finalize failed: state root mismatch"
            );
            return false;
        }

        // C++ L216: update finalized version
        self.db.finalize_statesync(target.number)
    }
}
