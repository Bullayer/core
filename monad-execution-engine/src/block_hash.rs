// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Complete rewrite of C++ BlockHashBufferFinalized / BlockHashBufferProposal / BlockHashChain.
// Closely mirrors block_hash_buffer.hpp / block_hash_buffer.cpp.

use std::collections::VecDeque;

use alloy_primitives::B256;
use monad_types::{BlockId, SeqNum};

use crate::traits::{BlockHashBuffer, ExecutionDb};
use crate::validation::compute_block_hash;

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
    n: SeqNum,
}

impl BlockHashBufferFinalized {
    pub fn new() -> Self {
        Self {
            hashes: [B256::ZERO; HASH_BUFFER_SIZE],
            n: SeqNum(0),
        }
    }

    /// Initialize with finalized seq_num. The buffer will be empty
    /// (all zeros) but n_ set so that future init can fill hashes.
    pub fn with_initial(n: SeqNum) -> Self {
        Self {
            hashes: [B256::ZERO; HASH_BUFFER_SIZE],
            n,
        }
    }

    /// Sequential write: C++ asserts (!n_ || n == n_), sets n_ = n + 1.
    pub fn set(&mut self, seq_num: SeqNum, hash: B256) {
        assert!(
            self.n == SeqNum(0) || seq_num == self.n,
            "BlockHashBufferFinalized::set: n_={}, n={}",
            self.n,
            seq_num
        );
        self.hashes[seq_num.0 as usize % HASH_BUFFER_SIZE] = hash;
        self.n = seq_num + SeqNum(1);
    }

    /// n() returns the "next to write" position (C++: n_).
    pub fn n(&self) -> SeqNum {
        self.n
    }

    /// Initialize buffer from DB: load up to 256 historical block hashes.
    /// Corresponds to C++ init_block_hash_buffer_from_triedb.
    pub fn init_from_db(&mut self, db: &mut dyn ExecutionDb, seq_num: SeqNum) {
        let start = if seq_num.0 < HASH_BUFFER_SIZE as u64 {
            SeqNum(0)
        } else {
            seq_num - SeqNum(HASH_BUFFER_SIZE as u64)
        };

        self.n = start;
        for n in start.0..seq_num.0 {
            let sn = SeqNum(n);
            let dummy_block_id = BlockId(monad_types::Hash([0u8; 32]));
            db.set_block_and_prefix(sn, dummy_block_id);
            let header = db.read_eth_header();
            if header.number == n {
                let hash = compute_block_hash(&header);
                self.set(sn, hash);
            } else {
                self.set(sn, B256::ZERO);
            }
        }
    }
}

impl BlockHashBuffer for BlockHashBufferFinalized {
    fn get(&self, seq_num: SeqNum) -> B256 {
        if seq_num >= self.n {
            return B256::ZERO;
        }
        if self.n.0 > (HASH_BUFFER_SIZE as u64)
            && (seq_num.0 + HASH_BUFFER_SIZE as u64) < self.n.0
        {
            return B256::ZERO;
        }
        self.hashes[seq_num.0 as usize % HASH_BUFFER_SIZE]
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
/// Lookup: idx = n_ - seq_num - 1; if idx < deltas_.size() return deltas_[idx], else fallback.
#[derive(Clone, Debug)]
pub struct BlockHashBufferProposal {
    n: SeqNum,
    deltas: Vec<B256>,
    finalized_n: SeqNum,
}

impl BlockHashBufferProposal {
    /// Construct from finalized buffer (C++ constructor from BlockHashBufferFinalized).
    /// n_ = buf.n() + 1, deltas_ = {hash}
    pub fn from_finalized(hash: B256, finalized_n: SeqNum) -> Self {
        Self {
            n: finalized_n + SeqNum(1),
            deltas: vec![hash],
            finalized_n,
        }
    }

    /// Construct from parent proposal (C++ constructor from BlockHashBufferProposal).
    /// n_ = parent.n_ + 1, deltas_ = [hash] ++ parent.deltas_, resize to n_ - buf_.n()
    pub fn from_proposal(hash: B256, parent: &BlockHashBufferProposal) -> Self {
        let n = parent.n + SeqNum(1);
        assert!(
            n > SeqNum(0) && n > parent.finalized_n,
            "BlockHashBufferProposal::from_proposal: n={}, finalized_n={}",
            n,
            parent.finalized_n
        );
        let delta_len = (n - parent.finalized_n).0 as usize;
        let mut deltas = Vec::with_capacity(delta_len);
        deltas.push(hash);
        deltas.extend_from_slice(&parent.deltas);
        deltas.resize(delta_len, B256::ZERO);
        Self {
            n,
            deltas,
            finalized_n: parent.finalized_n,
        }
    }

    pub fn n(&self) -> SeqNum {
        self.n
    }

    pub fn get_with_fallback(&self, seq_num: SeqNum, finalized: &BlockHashBufferFinalized) -> B256 {
        if seq_num >= self.n {
            return B256::ZERO;
        }
        if self.n.0 > (HASH_BUFFER_SIZE as u64)
            && (seq_num.0 + HASH_BUFFER_SIZE as u64) < self.n.0
        {
            return B256::ZERO;
        }
        let idx = (self.n - seq_num - SeqNum(1)).0 as usize;
        // Guard: if seq_num is now finalized (finalized.n() advanced past our
        // snapshot), always fall through to the authoritative finalized buffer.
        // C++ uses a live pointer (buf_->n()), so this happens automatically.
        if idx < self.deltas.len() && seq_num >= finalized.n() {
            self.deltas[idx]
        } else {
            finalized.get(seq_num)
        }
    }
}

/// Wraps either finalized buffer or proposal buffer for hash lookups.
pub enum BlockHashBufferRef<'a> {
    Finalized(&'a BlockHashBufferFinalized),
    Proposal(&'a BlockHashBufferProposal, &'a BlockHashBufferFinalized),
}

impl BlockHashBuffer for BlockHashBufferRef<'_> {
    fn get(&self, seq_num: SeqNum) -> B256 {
        match self {
            BlockHashBufferRef::Finalized(f) => f.get(seq_num),
            BlockHashBufferRef::Proposal(p, f) => p.get_with_fallback(seq_num, f),
        }
    }
}

#[derive(Clone, Debug)]
struct Proposal {
    seq_num: SeqNum,
    block_id: BlockId,
    #[allow(dead_code)]
    parent_id: BlockId,
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
    pub fn find_chain(&self, parent_id: &BlockId) -> BlockHashBufferRef<'_> {
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
        seq_num: SeqNum,
        block_id: BlockId,
        parent_id: BlockId,
    ) {
        let parent_buf = self.proposals.iter().find(|p| p.block_id == parent_id);

        let buf = if let Some(parent) = parent_buf {
            BlockHashBufferProposal::from_proposal(eth_block_hash, &parent.buf)
        } else {
            BlockHashBufferProposal::from_finalized(eth_block_hash, self.finalized.n())
        };

        self.proposals.push_back(Proposal {
            seq_num,
            block_id,
            parent_id,
            buf,
        });
    }

    /// Finalize a block: write exactly ONE hash to the finalized buffer and prune old proposals.
    /// C++: to_finalize = buf_.n(); buf_.set(to_finalize, winner.buf.get(to_finalize));
    /// Then erase proposals with seq_num <= winner's seq_num.
    ///
    /// Returns `true` if the block was found in proposals and finalized,
    /// `false` if the block was not proposed (hash chain state unchanged).
    pub fn finalize(&mut self, block_id: BlockId) -> bool {
        let to_finalize = self.finalized.n();

        let winner_pos = self
            .proposals
            .iter()
            .position(|p| p.block_id == block_id);

        let pos = match winner_pos {
            Some(p) => p,
            None => {
                tracing::warn!(
                    ?block_id,
                    "BlockHashChain::finalize: block_id not in proposals, skipping"
                );
                return false;
            }
        };

        assert_eq!(
            self.proposals[pos].buf.n() - SeqNum(1),
            to_finalize,
            "BlockHashChain::finalize: proposal buf.n()-1 ({}) != to_finalize ({})",
            self.proposals[pos].buf.n() - SeqNum(1),
            to_finalize
        );

        let hash = self.proposals[pos]
            .buf
            .get_with_fallback(to_finalize, &self.finalized);
        self.finalized.set(to_finalize, hash);

        let winner_seq_num = self.proposals[pos].seq_num;

        self.proposals
            .retain(|p| p.seq_num > winner_seq_num);

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monad_crypto::hasher::Hash;

    fn test_block_id(v: u8) -> BlockId {
        BlockId(Hash([v; 32]))
    }

    #[test]
    fn test_finalized_buffer_sequential_set() {
        let mut buf = BlockHashBufferFinalized::new();
        let hash0 = B256::from([0xAA; 32]);
        let hash1 = B256::from([0xBB; 32]);

        buf.set(SeqNum(0), hash0);
        assert_eq!(buf.n(), SeqNum(1));
        assert_eq!(buf.get(SeqNum(0)), hash0);

        buf.set(SeqNum(1), hash1);
        assert_eq!(buf.n(), SeqNum(2));
        assert_eq!(buf.get(SeqNum(1)), hash1);

        assert_eq!(buf.get(SeqNum(2)), B256::ZERO);
    }

    #[test]
    fn test_proposal_from_finalized() {
        let mut fin = BlockHashBufferFinalized::new();
        fin.set(SeqNum(0), B256::from([1u8; 32]));
        fin.set(SeqNum(1), B256::from([2u8; 32]));

        let hash_2 = B256::from([3u8; 32]);
        let prop = BlockHashBufferProposal::from_finalized(hash_2, fin.n());

        assert_eq!(prop.n(), SeqNum(3));
        assert_eq!(prop.get_with_fallback(SeqNum(2), &fin), hash_2);
        assert_eq!(prop.get_with_fallback(SeqNum(1), &fin), B256::from([2u8; 32]));
        assert_eq!(prop.get_with_fallback(SeqNum(0), &fin), B256::from([1u8; 32]));
    }

    #[test]
    fn test_proposal_chain() {
        let mut fin = BlockHashBufferFinalized::new();
        fin.set(SeqNum(0), B256::from([1u8; 32]));

        let hash_1 = B256::from([0xAA; 32]);
        let prop_a = BlockHashBufferProposal::from_finalized(hash_1, fin.n());

        let hash_2 = B256::from([0xBB; 32]);
        let prop_b = BlockHashBufferProposal::from_proposal(hash_2, &prop_a);

        assert_eq!(prop_b.get_with_fallback(SeqNum(2), &fin), hash_2);
        assert_eq!(prop_b.get_with_fallback(SeqNum(1), &fin), hash_1);
        assert_eq!(prop_b.get_with_fallback(SeqNum(0), &fin), B256::from([1u8; 32]));
    }

    #[test]
    fn test_block_hash_chain_propose_and_finalize() {
        let mut finalized = BlockHashBufferFinalized::new();
        finalized.set(SeqNum(0), B256::from([0xAA; 32]));

        let mut chain = BlockHashChain::new(finalized);

        let block_id_1 = test_block_id(1);
        let block_hash_1 = B256::from([0xBB; 32]);
        let parent_id_0 = BlockId(Hash([0u8; 32]));

        chain.propose(block_hash_1, SeqNum(1), block_id_1, parent_id_0);

        let buf = chain.find_chain(&block_id_1);
        assert_eq!(buf.get(SeqNum(1)), block_hash_1);
        assert_eq!(buf.get(SeqNum(0)), B256::from([0xAA; 32]));

        assert_eq!(chain.finalized().n(), SeqNum(1));

        chain.finalize(block_id_1);
        assert_eq!(chain.finalized().n(), SeqNum(2));
        assert_eq!(chain.finalized().get(SeqNum(1)), block_hash_1);
    }

    #[test]
    fn test_finalize_only_writes_one_hash() {
        let mut finalized = BlockHashBufferFinalized::new();
        finalized.set(SeqNum(0), B256::from([0xAA; 32]));

        let mut chain = BlockHashChain::new(finalized);

        let bid1 = test_block_id(1);
        chain.propose(B256::from([0xBB; 32]), SeqNum(1), bid1, BlockId(Hash([0u8; 32])));

        let bid2 = test_block_id(2);
        chain.propose(B256::from([0xCC; 32]), SeqNum(2), bid2, bid1);

        chain.finalize(bid1);
        assert_eq!(chain.finalized().n(), SeqNum(2));

        chain.finalize(bid2);
        assert_eq!(chain.finalized().n(), SeqNum(3));
        assert_eq!(chain.finalized().get(SeqNum(2)), B256::from([0xCC; 32]));
    }
}
