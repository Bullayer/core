// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Complete rewrite of C++ BlockHashBufferFinalized / BlockHashBufferProposal / BlockHashChain.
// Closely mirrors block_hash_buffer.hpp / block_hash_buffer.cpp.

use std::collections::VecDeque;

use alloy_primitives::B256;

use crate::traits::BlockHashBuffer;

const HASH_BUFFER_SIZE: usize = 256;

/// 256-slot circular buffer storing finalized block hashes.
/// Corresponds to C++ BlockHashBufferFinalized.
///
/// C++ semantics: n_ = "next slot to write" = highest_written + 1.
/// set(n, h) requires n == n_ (strict sequential), sets n_ = n + 1.
/// get(n) requires n < n_ && n + N >= n_.
#[derive(Clone, Debug)]
pub struct BlockHashBufferFinalized {
    hashes: [B256; HASH_BUFFER_SIZE],
    n: u64,
}

impl BlockHashBufferFinalized {
    pub fn new() -> Self {
        Self {
            hashes: [B256::ZERO; HASH_BUFFER_SIZE],
            n: 0,
        }
    }

    /// Initialize with finalized block number. The buffer will be empty
    /// (all zeros) but n_ set so that future init can fill hashes.
    pub fn with_initial(n: u64) -> Self {
        Self {
            hashes: [B256::ZERO; HASH_BUFFER_SIZE],
            n,
        }
    }

    /// Sequential write: C++ asserts (!n_ || n == n_), sets n_ = n + 1.
    pub fn set(&mut self, block_number: u64, hash: B256) {
        assert!(
            self.n == 0 || block_number == self.n,
            "BlockHashBufferFinalized::set: n_={}, n={}",
            self.n,
            block_number
        );
        self.hashes[block_number as usize % HASH_BUFFER_SIZE] = hash;
        self.n = block_number + 1;
    }

    /// n() returns the "next to write" position (C++: n_).
    pub fn n(&self) -> u64 {
        self.n
    }

    /// Initialize buffer from DB: load up to 256 historical block hashes.
    /// Corresponds to C++ init_block_hash_buffer_from_triedb.
    pub fn init_from_db(&mut self, db: &dyn crate::traits::ExecutionDb, block_number: u64) {
        let start = if block_number < HASH_BUFFER_SIZE as u64 {
            0
        } else {
            block_number - HASH_BUFFER_SIZE as u64
        };
        // In mock phase, we can't load real hashes from DB.
        // Set n to block_number so the buffer is "primed" for future writes.
        self.n = block_number;
        let _ = (start, db);
    }
}

impl BlockHashBuffer for BlockHashBufferFinalized {
    fn get(&self, block_number: u64) -> B256 {
        // C++: assert(n < n_ && n + N >= n_)
        if block_number >= self.n {
            return B256::ZERO;
        }
        // n + N >= n_ ↔ n + N > n_ - 1 ↔ NOT (n + N < n_)
        if self.n > (HASH_BUFFER_SIZE as u64)
            && (block_number + HASH_BUFFER_SIZE as u64) < self.n
        {
            return B256::ZERO;
        }
        self.hashes[block_number as usize % HASH_BUFFER_SIZE]
    }
}

impl Default for BlockHashBufferFinalized {
    fn default() -> Self {
        Self::new()
    }
}

/// Proposal-level hash buffer that layers deltas on top of a parent buffer.
/// Corresponds to C++ BlockHashBufferProposal.
///
/// C++ stores deltas in reverse order: deltas_[0] = newest, deltas_[last] = oldest.
/// Lookup: idx = n_ - block_number - 1; if idx < deltas_.size() return deltas_[idx], else fallback.
#[derive(Clone, Debug)]
pub struct BlockHashBufferProposal {
    n: u64,
    deltas: Vec<B256>,
    finalized_n: u64,
}

impl BlockHashBufferProposal {
    /// Construct from finalized buffer (C++ constructor from BlockHashBufferFinalized).
    /// n_ = buf.n() + 1, deltas_ = {hash}
    pub fn from_finalized(hash: B256, finalized_n: u64) -> Self {
        Self {
            n: finalized_n + 1,
            deltas: vec![hash],
            finalized_n,
        }
    }

    /// Construct from parent proposal (C++ constructor from BlockHashBufferProposal).
    /// n_ = parent.n_ + 1, deltas_ = [hash] ++ parent.deltas_, resize to n_ - buf_.n()
    pub fn from_proposal(hash: B256, parent: &BlockHashBufferProposal) -> Self {
        let n = parent.n + 1;
        // C++ asserts: n_ > 0 && n_ > buf_->n()
        assert!(
            n > 0 && n > parent.finalized_n,
            "BlockHashBufferProposal::from_proposal: n={}, finalized_n={}",
            n,
            parent.finalized_n
        );
        let mut deltas = Vec::with_capacity((n - parent.finalized_n) as usize);
        deltas.push(hash);
        deltas.extend_from_slice(&parent.deltas);
        deltas.resize((n - parent.finalized_n) as usize, B256::ZERO);
        Self {
            n,
            deltas,
            finalized_n: parent.finalized_n,
        }
    }

    pub fn n(&self) -> u64 {
        self.n
    }

    pub fn get_with_fallback(&self, block_number: u64, finalized: &BlockHashBufferFinalized) -> B256 {
        if block_number >= self.n {
            return B256::ZERO;
        }
        if self.n > (HASH_BUFFER_SIZE as u64)
            && (block_number + HASH_BUFFER_SIZE as u64) < self.n
        {
            return B256::ZERO;
        }
        let idx = (self.n - block_number - 1) as usize;
        // Guard: if block_number is now finalized (finalized.n() advanced past our
        // snapshot), always fall through to the authoritative finalized buffer.
        // C++ uses a live pointer (buf_->n()), so this happens automatically.
        if idx < self.deltas.len() && block_number >= finalized.n() {
            self.deltas[idx]
        } else {
            finalized.get(block_number)
        }
    }
}

/// Wraps either finalized buffer or proposal buffer for hash lookups.
pub enum BlockHashBufferRef<'a> {
    Finalized(&'a BlockHashBufferFinalized),
    Proposal(&'a BlockHashBufferProposal, &'a BlockHashBufferFinalized),
}

impl BlockHashBuffer for BlockHashBufferRef<'_> {
    fn get(&self, block_number: u64) -> B256 {
        match self {
            BlockHashBufferRef::Finalized(f) => f.get(block_number),
            BlockHashBufferRef::Proposal(p, f) => p.get_with_fallback(block_number, f),
        }
    }
}

#[derive(Clone, Debug)]
struct Proposal {
    block_number: u64,
    block_id: B256,
    #[allow(dead_code)]
    parent_id: B256,
    buf: BlockHashBufferProposal,
}

/// Manages multiple proposal branches on top of a finalized hash buffer.
/// Corresponds to C++ BlockHashChain.
pub struct BlockHashChain {
    finalized: BlockHashBufferFinalized,
    proposals: VecDeque<Proposal>,
}

impl BlockHashChain {
    pub fn new(finalized: BlockHashBufferFinalized) -> Self {
        Self {
            finalized,
            proposals: VecDeque::new(),
        }
    }

    pub fn finalized(&self) -> &BlockHashBufferFinalized {
        &self.finalized
    }

    pub fn finalized_mut(&mut self) -> &mut BlockHashBufferFinalized {
        &mut self.finalized
    }

    /// Find the hash buffer for the chain ending at parent_id.
    /// C++: iterates from begin to end (oldest first).
    pub fn find_chain(&self, parent_id: &B256) -> BlockHashBufferRef<'_> {
        for proposal in &self.proposals {
            if proposal.block_id == *parent_id {
                return BlockHashBufferRef::Proposal(&proposal.buf, &self.finalized);
            }
        }
        BlockHashBufferRef::Finalized(&self.finalized)
    }

    /// Record a new proposal's block hash.
    /// C++: finds parent in proposals (begin to end), creates BlockHashBufferProposal.
    pub fn propose(
        &mut self,
        eth_block_hash: B256,
        block_number: u64,
        block_id: B256,
        parent_id: B256,
    ) {
        // Look for parent proposal (C++ iterates begin to end)
        let parent_buf = self.proposals.iter().find(|p| p.block_id == parent_id);

        let buf = if let Some(parent) = parent_buf {
            BlockHashBufferProposal::from_proposal(eth_block_hash, &parent.buf)
        } else {
            BlockHashBufferProposal::from_finalized(eth_block_hash, self.finalized.n())
        };

        self.proposals.push_back(Proposal {
            block_number,
            block_id,
            parent_id,
            buf,
        });
    }

    /// Finalize a block: write exactly ONE hash to the finalized buffer and prune old proposals.
    /// C++: to_finalize = buf_.n(); buf_.set(to_finalize, winner.buf.get(to_finalize));
    /// Then erase proposals with block_number <= winner's block_number.
    pub fn finalize(&mut self, block_id: B256) {
        let to_finalize = self.finalized.n();

        let winner_pos = self
            .proposals
            .iter()
            .position(|p| p.block_id == block_id);

        // C++: MONAD_ASSERT(winner_it != proposals_.end())
        let pos = winner_pos.expect("BlockHashChain::finalize: block_id not in proposals");

        // C++: MONAD_ASSERT((winner_it->buf.n() - 1) == to_finalize)
        assert_eq!(
            self.proposals[pos].buf.n() - 1,
            to_finalize,
            "BlockHashChain::finalize: proposal buf.n()-1 ({}) != to_finalize ({})",
            self.proposals[pos].buf.n() - 1,
            to_finalize
        );

        let hash = self.proposals[pos]
            .buf
            .get_with_fallback(to_finalize, &self.finalized);
        self.finalized.set(to_finalize, hash);

        let block_number = self.proposals[pos].block_number;

        // C++: erase proposals with block_number <= winner's block_number
        self.proposals
            .retain(|p| p.block_number > block_number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finalized_buffer_sequential_set() {
        let mut buf = BlockHashBufferFinalized::new();
        let hash0 = B256::from([0xAA; 32]);
        let hash1 = B256::from([0xBB; 32]);

        // n starts at 0, first set must be at 0
        buf.set(0, hash0);
        assert_eq!(buf.n(), 1);
        assert_eq!(buf.get(0), hash0);

        buf.set(1, hash1);
        assert_eq!(buf.n(), 2);
        assert_eq!(buf.get(1), hash1);

        // Out of bounds returns ZERO
        assert_eq!(buf.get(2), B256::ZERO);
    }

    #[test]
    fn test_proposal_from_finalized() {
        let mut fin = BlockHashBufferFinalized::new();
        fin.set(0, B256::from([1u8; 32]));
        fin.set(1, B256::from([2u8; 32]));
        // fin.n() == 2

        let hash_2 = B256::from([3u8; 32]);
        let prop = BlockHashBufferProposal::from_finalized(hash_2, fin.n());
        // prop.n == 3, deltas == [hash_2]

        assert_eq!(prop.n(), 3);
        assert_eq!(prop.get_with_fallback(2, &fin), hash_2);
        assert_eq!(prop.get_with_fallback(1, &fin), B256::from([2u8; 32]));
        assert_eq!(prop.get_with_fallback(0, &fin), B256::from([1u8; 32]));
    }

    #[test]
    fn test_proposal_chain() {
        let mut fin = BlockHashBufferFinalized::new();
        fin.set(0, B256::from([1u8; 32]));
        // fin.n() == 1

        let hash_1 = B256::from([0xAA; 32]);
        let prop_a = BlockHashBufferProposal::from_finalized(hash_1, fin.n());
        // prop_a.n == 2, deltas == [hash_1]

        let hash_2 = B256::from([0xBB; 32]);
        let prop_b = BlockHashBufferProposal::from_proposal(hash_2, &prop_a);
        // prop_b.n == 3, deltas == [hash_2, hash_1]

        assert_eq!(prop_b.get_with_fallback(2, &fin), hash_2);
        assert_eq!(prop_b.get_with_fallback(1, &fin), hash_1);
        assert_eq!(prop_b.get_with_fallback(0, &fin), B256::from([1u8; 32]));
    }

    #[test]
    fn test_block_hash_chain_propose_and_finalize() {
        let mut finalized = BlockHashBufferFinalized::new();
        finalized.set(0, B256::from([0xAA; 32]));
        // finalized.n() == 1

        let mut chain = BlockHashChain::new(finalized);

        let block_id_1 = B256::from([1u8; 32]);
        let block_hash_1 = B256::from([0xBB; 32]);
        let parent_id_0 = B256::ZERO; // no parent proposal

        chain.propose(block_hash_1, 1, block_id_1, parent_id_0);

        // find_chain by block_id_1 should return the proposal buffer
        let buf = chain.find_chain(&block_id_1);
        assert_eq!(buf.get(1), block_hash_1);
        assert_eq!(buf.get(0), B256::from([0xAA; 32]));

        // Before finalize: finalized.n() == 1
        assert_eq!(chain.finalized().n(), 1);

        // Finalize block_id_1: writes hash at position 1 (= finalized.n())
        chain.finalize(block_id_1);
        assert_eq!(chain.finalized().n(), 2);
        assert_eq!(chain.finalized().get(1), block_hash_1);
    }

    #[test]
    fn test_finalize_only_writes_one_hash() {
        let mut finalized = BlockHashBufferFinalized::new();
        finalized.set(0, B256::from([0xAA; 32]));
        // finalized.n() == 1

        let mut chain = BlockHashChain::new(finalized);

        // Propose block 1 on top of finalized
        let bid1 = B256::from([1u8; 32]);
        chain.propose(B256::from([0xBB; 32]), 1, bid1, B256::ZERO);

        // Propose block 2 on top of block 1
        let bid2 = B256::from([2u8; 32]);
        chain.propose(B256::from([0xCC; 32]), 2, bid2, bid1);

        // Finalize block 1: should write hash at position 1 only
        chain.finalize(bid1);
        assert_eq!(chain.finalized().n(), 2); // n_ went from 1 to 2

        // Finalize block 2: should write hash at position 2
        chain.finalize(bid2);
        assert_eq!(chain.finalized().n(), 3); // n_ went from 2 to 3
        assert_eq!(chain.finalized().get(2), B256::from([0xCC; 32]));
    }
}
