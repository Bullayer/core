// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Rewrite of C++ statesync_server_context.cpp deletion tracking.
// Pure data structure logic, no DB dependency.
// Closely mirrors C++ FinalizedDeletions with circular buffer semantics.

use std::collections::VecDeque;

use monad_types::{BlockId, SeqNum};

use crate::types::StateDeltas;

use super::types::Deletion;

const MAX_ENTRIES: usize = 43200;
const MAX_DELETIONS: usize = 2_000_000;
const INVALID_SEQ_NUM: SeqNum = SeqNum::MAX;

/// Entry in the entries ring buffer.
struct FinalizedEntry {
    seq_num: SeqNum,
    idx: usize,
    size: usize,
}

/// Finalized deletions stored in a circular buffer.
/// Closely mirrors C++ FinalizedDeletions with fixed-size arrays and circular pointers.
pub struct FinalizedDeletions {
    start_seq_num: SeqNum,
    end_seq_num: SeqNum,
    entries: Vec<Option<FinalizedEntry>>,
    deletions: Vec<Deletion>,
    free_start: usize,
    free_end: usize,
}

impl FinalizedDeletions {
    pub fn new() -> Self {
        let mut deletions = Vec::with_capacity(MAX_DELETIONS);
        deletions.resize(
            MAX_DELETIONS,
            Deletion {
                address: alloy_primitives::Address::ZERO,
                key: None,
            },
        );

        Self {
            start_seq_num: INVALID_SEQ_NUM,
            end_seq_num: INVALID_SEQ_NUM,
            entries: (0..MAX_ENTRIES).map(|_| None).collect(),
            deletions,
            free_start: 0,
            free_end: MAX_DELETIONS,
        }
    }

    fn free_deletions(&self) -> usize {
        self.free_end - self.free_start
    }

    fn set_entry(&mut self, i: usize, seq_num: SeqNum, deletions: &[Deletion]) {
        let offset = self.free_start;
        let count = deletions.len();
        for (j, deletion) in deletions.iter().enumerate() {
            self.deletions[(offset + j) % MAX_DELETIONS] = deletion.clone();
        }
        self.free_start += count;
        self.entries[i] = Some(FinalizedEntry {
            seq_num,
            idx: offset,
            size: count,
        });
    }

    fn clear_entry(&mut self, i: usize) {
        if let Some(entry) = &self.entries[i] {
            if entry.seq_num == INVALID_SEQ_NUM {
                return;
            }
            assert_eq!(
                entry.seq_num, self.start_seq_num,
                "clear_entry: expected start_seq_num={}, got={}",
                self.start_seq_num, entry.seq_num
            );
            self.free_end += entry.size;
            self.start_seq_num = self.start_seq_num + SeqNum(1);
        }
        self.entries[i] = None;
    }

    /// Write deletions for a finalized block.
    /// C++ FinalizedDeletions::write (statesync_server_context.cpp L172-213).
    pub fn write(&mut self, seq_num: SeqNum, deletions: Vec<Deletion>) {
        assert_ne!(seq_num, INVALID_SEQ_NUM);
        assert!(
            self.end_seq_num == INVALID_SEQ_NUM
                || self.end_seq_num + SeqNum(1) == seq_num,
            "write: expected sequential seq_num, end={}, got={}",
            self.end_seq_num,
            seq_num
        );

        self.end_seq_num = seq_num;

        if deletions.len() > MAX_DELETIONS {
            tracing::warn!(
                seq_num = seq_num.0,
                size = deletions.len(),
                "dropping deletions due to excessive size"
            );
            for i in 0..MAX_ENTRIES {
                self.clear_entry(i);
            }
            self.start_seq_num = INVALID_SEQ_NUM;
            assert_eq!(self.free_deletions(), MAX_DELETIONS);
            return;
        }

        if self.start_seq_num == INVALID_SEQ_NUM {
            self.start_seq_num = self.end_seq_num;
        }

        let target_idx = self.end_seq_num.0 as usize % MAX_ENTRIES;
        self.clear_entry(target_idx);

        while self.free_deletions() < deletions.len() {
            assert!(
                self.start_seq_num < self.end_seq_num,
                "no space but start >= end"
            );
            let idx = self.start_seq_num.0 as usize % MAX_ENTRIES;
            self.clear_entry(idx);
        }

        self.set_entry(target_idx, self.end_seq_num, &deletions);
    }

    /// Iterate deletions for a specific seq_num.
    /// Returns false if seq_num not found (C++ returns false).
    pub fn for_each<F>(&self, seq_num: SeqNum, mut f: F) -> bool
    where
        F: FnMut(&Deletion),
    {
        let idx = seq_num.0 as usize % MAX_ENTRIES;
        if let Some(entry) = &self.entries[idx] {
            if entry.seq_num == seq_num {
                for i in 0..entry.size {
                    let deletion_idx = (entry.idx + i) % MAX_DELETIONS;
                    f(&self.deletions[deletion_idx]);
                }
                return true;
            }
        }
        false
    }
}

impl Default for FinalizedDeletions {
    fn default() -> Self {
        Self::new()
    }
}

/// Proposed (not yet finalized) deletions.
pub struct ProposedDeletions {
    proposals: VecDeque<ProposedEntry>,
}

struct ProposedEntry {
    seq_num: SeqNum,
    block_id: BlockId,
    deletions: Vec<Deletion>,
}

impl ProposedDeletions {
    pub fn new() -> Self {
        Self {
            proposals: VecDeque::new(),
        }
    }

    pub fn push(&mut self, seq_num: SeqNum, block_id: BlockId, deletions: Vec<Deletion>) {
        self.proposals.push_back(ProposedEntry {
            seq_num,
            block_id,
            deletions,
        });
    }

    /// On finalize: find the matching proposal, write to FinalizedDeletions, GC old entries.
    /// C++ on_finalize (statesync_server_context.cpp L77-105).
    pub fn on_finalize(
        &mut self,
        finalized: &mut FinalizedDeletions,
        seq_num: SeqNum,
        block_id: BlockId,
    ) {
        if let Some(pos) = self
            .proposals
            .iter()
            .position(|p| p.block_id == block_id)
        {
            let entry = self.proposals.remove(pos).unwrap();
            assert_eq!(entry.seq_num, seq_num);
            finalized.write(seq_num, entry.deletions);
        }

        self.proposals
            .retain(|p| p.seq_num > seq_num);
    }
}

impl Default for ProposedDeletions {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract deletions from state deltas. Corresponds to C++ on_commit().
pub fn extract_deletions(state_deltas: &StateDeltas) -> Vec<Deletion> {
    let mut deletions = Vec::new();

    for (address, delta) in state_deltas {
        if delta.new_account.is_some() {
            for (key, storage_delta) in &delta.storage {
                if storage_delta.old_value != storage_delta.new_value
                    && storage_delta.new_value == alloy_primitives::B256::ZERO
                {
                    deletions.push(Deletion {
                        address: *address,
                        key: Some(*key),
                    });
                }
            }
        }

        match (&delta.old_account, &delta.new_account) {
            (Some(_), None) => {
                deletions.push(Deletion {
                    address: *address,
                    key: None,
                });
            }
            _ => {}
        }
    }

    deletions
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use monad_crypto::hasher::Hash;

    use super::*;

    fn test_block_id(v: u8) -> BlockId {
        BlockId(Hash([v; 32]))
    }

    #[test]
    fn test_finalized_deletions_write_and_read() {
        let mut fd = FinalizedDeletions::new();
        let addr = Address::from([1u8; 20]);
        let deletions = vec![
            Deletion {
                address: addr,
                key: None,
            },
            Deletion {
                address: addr,
                key: Some(alloy_primitives::B256::from([2u8; 32])),
            },
        ];
        fd.write(SeqNum(100), deletions);

        let mut count = 0;
        assert!(fd.for_each(SeqNum(100), |_| count += 1));
        assert_eq!(count, 2);

        let mut count = 0;
        assert!(!fd.for_each(SeqNum(101), |_| count += 1));
        assert_eq!(count, 0);
    }

    #[test]
    fn test_sequential_writes() {
        let mut fd = FinalizedDeletions::new();
        let addr = Address::from([1u8; 20]);

        fd.write(SeqNum(100), vec![Deletion { address: addr, key: None }]);
        fd.write(SeqNum(101), vec![Deletion { address: addr, key: Some(alloy_primitives::B256::from([2u8; 32])) }]);

        let mut count = 0;
        assert!(fd.for_each(SeqNum(100), |_| count += 1));
        assert_eq!(count, 1);

        count = 0;
        assert!(fd.for_each(SeqNum(101), |_| count += 1));
        assert_eq!(count, 1);
    }

    #[test]
    fn test_proposed_deletions_on_finalize() {
        let mut pd = ProposedDeletions::new();
        let mut fd = FinalizedDeletions::new();

        let block_id = test_block_id(1);
        let addr = Address::from([2u8; 20]);
        pd.push(
            SeqNum(10),
            block_id,
            vec![Deletion {
                address: addr,
                key: None,
            }],
        );

        pd.on_finalize(&mut fd, SeqNum(10), block_id);

        let mut count = 0;
        assert!(fd.for_each(SeqNum(10), |_| count += 1));
        assert_eq!(count, 1);
        assert!(pd.proposals.is_empty());
    }
}
