// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.

use alloy_primitives::B256;

use crate::types::{BlockCache, BlockCacheEntry};

/// Prune BlockCache entries whose block_number < last_finalized - 1.
/// Corresponds to C++ block_cache erase_if logic at L712-719.
pub fn prune_block_cache(cache: &mut BlockCache, last_finalized: u64) {
    if last_finalized <= 1 {
        return;
    }
    cache.retain(|_, entry| entry.block_number >= last_finalized - 1);
}

/// Insert a new entry into the block cache.
pub fn insert_block_cache(
    cache: &mut BlockCache,
    block_id: B256,
    entry: BlockCacheEntry,
) -> bool {
    use std::collections::hash_map::Entry;
    match cache.entry(block_id) {
        Entry::Occupied(_) => false,
        Entry::Vacant(e) => {
            e.insert(entry);
            true
        }
    }
}
