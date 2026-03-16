// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Rewrite of C++ statesync_server.cpp handle_sync_request (L180-489).

use monad_types::SeqNum;
use tiny_keccak::{Hasher, Keccak};

use super::deletion_tracker::FinalizedDeletions;
use super::types::{Deletion, SyncDone, SyncUpsertType};
use super::{StateSyncError, StateSyncRequest, StateSyncResult, StateSyncTraversable};

const INVALID_BLOCK_NUM: u64 = u64::MAX;

/// Validate the incoming sync request.
/// C++ static_validate_request (statesync_server.cpp L63-107).
fn static_validate_request(request: &StateSyncRequest) -> Result<(), StateSyncError> {
    if request.prefix_bytes > 8 {
        return Err(StateSyncError::InvalidRequest(
            "prefix_bytes > 8".to_string(),
        ));
    }
    if request.target == INVALID_BLOCK_NUM {
        return Err(StateSyncError::InvalidRequest(
            "target is INVALID".to_string(),
        ));
    }
    if request.from > request.until {
        return Err(StateSyncError::InvalidRequest(format!(
            "from ({}) > until ({})",
            request.from, request.until,
        )));
    }
    if request.until > request.target {
        return Err(StateSyncError::InvalidRequest(format!(
            "until ({}) > target ({})",
            request.until, request.target,
        )));
    }
    // C++ L96-104: validate old_target == INVALID || old_target <= target
    if request.old_target != INVALID_BLOCK_NUM && request.old_target > request.target {
        return Err(StateSyncError::InvalidRequest(format!(
            "old_target ({}) > target ({})",
            request.old_target, request.target,
        )));
    }
    Ok(())
}

/// Compute keccak256(addr) and extract prefix bytes, then check if they match the request prefix.
/// C++ send_deletion (statesync_server.cpp L130-138): uses keccak256(addr.bytes).
fn deletion_matches_prefix(deletion: &Deletion, prefix: u64, prefix_bytes: u8) -> bool {
    let mut hasher = Keccak::v256();
    let mut hash = [0u8; 32];
    hasher.update(deletion.address.as_slice());
    hasher.finalize(&mut hash);

    let prefix_byte_slice = from_prefix(prefix, prefix_bytes as usize);
    hash.starts_with(&prefix_byte_slice)
}

/// Convert prefix u64 + n_bytes to a byte slice.
/// C++ from_prefix (statesync_server.cpp L109-116).
fn from_prefix(prefix: u64, n_bytes: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(n_bytes);
    for i in 0..n_bytes {
        bytes.push(((prefix >> ((n_bytes - i - 1) * 8)) & 0xff) as u8);
    }
    bytes
}

/// Send deletions for old_target+1..=target.
/// C++ send_deletion (statesync_server.cpp L118-178).
/// Returns false if a block's deletions are missing (for_each returns false).
fn send_deletions(
    request: &StateSyncRequest,
    finalized_deletions: &FinalizedDeletions,
    upserts: &mut Vec<(SyncUpsertType, Vec<u8>)>,
) -> bool {
    // C++ L126-128: skip all deletions when old_target == INVALID
    if request.old_target == INVALID_BLOCK_NUM {
        return true;
    }

    assert!(request.old_target <= request.target);

    for block_num in (request.old_target + 1)..=request.target {
        let found = finalized_deletions.for_each(SeqNum(block_num), |deletion: &Deletion| {
            if !deletion_matches_prefix(deletion, request.prefix, request.prefix_bytes) {
                return;
            }
            match &deletion.key {
                None => {
                    upserts.push((
                        SyncUpsertType::AccountDelete,
                        deletion.address.as_slice().to_vec(),
                    ));
                }
                Some(key) => {
                    let compact_key =
                        super::protocol::encode_bytes32_compact(&key.0);
                    let mut data = Vec::with_capacity(20 + compact_key.len());
                    data.extend_from_slice(deletion.address.as_slice());
                    data.extend_from_slice(&compact_key);
                    upserts.push((SyncUpsertType::StorageDelete, data));
                }
            }
        });
        if !found {
            tracing::info!(
                block_num,
                old_target = request.old_target,
                target = request.target,
                "deletion not found for block"
            );
            return false;
        }
    }
    true
}

/// Handle a sync request from a peer.
/// Validates, sends header + deletions + state traversal upserts.
/// C++ statesync_server_handle_request (statesync_server.cpp L180-489).
pub fn handle_sync_request(
    request: &StateSyncRequest,
    db: &dyn StateSyncTraversable,
    finalized_deletions: &FinalizedDeletions,
) -> Result<StateSyncResult, StateSyncError> {
    static_validate_request(request)?;

    if !db.has_version(request.target) {
        return Err(StateSyncError::VersionNotAvailable(request.target));
    }

    let mut upserts: Vec<(SyncUpsertType, Vec<u8>)> = Vec::new();

    // 1. Send header if applicable
    // C++ L346-374: fails if header root or block header is missing
    if request.prefix < 256 && request.target > request.prefix {
        let header_version = request.target - request.prefix - 1;
        match db.read_block_header_at(header_version) {
            Some(header_data) => {
                upserts.push((SyncUpsertType::Header, header_data));
            }
            None => {
                tracing::info!(
                    header_version,
                    "block header not available for sync request"
                );
                return Err(StateSyncError::InternalError(format!(
                    "block header not available at version {}",
                    header_version
                )));
            }
        }
    }

    // 2. Send deletions for old_target+1..=target
    // C++ L376-378
    if !send_deletions(request, finalized_deletions, &mut upserts) {
        return Err(StateSyncError::InternalError(
            "deletion not found during sync".to_string(),
        ));
    }

    // 3. Traverse state: emit upserts for accounts/storage/code in the prefix range
    // C++ L380-418
    let prefix_bytes = from_prefix(request.prefix, request.prefix_bytes as usize);

    db.traverse_state(
        &prefix_bytes,
        request.from,
        request.until,
        &mut |upsert_type, data| {
            upserts.push((upsert_type, data.to_vec()));
        },
    );

    let done = SyncDone {
        success: true,
        prefix: request.prefix,
        n: request.until,
    };

    Ok(StateSyncResult { upserts, done })
}
