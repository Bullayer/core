use std::collections::HashMap;

use alloy_consensus::{Header, ReceiptEnvelope, TxEnvelope};
use alloy_primitives::{Address, B256};
use monad_eth_types::EthAccount;
use monad_types::{BlockId, SeqNum};
use revm::database::states::BundleState;

/// A single proposed (uncommitted) block's state changes, held in memory until
/// finalized.
pub struct StagedBlock {
    pub seq: SeqNum,
    pub parent_seq: SeqNum,
    pub parent_id: BlockId,

    pub header: Header,
    pub receipts: Vec<ReceiptEnvelope>,
    pub transactions: Vec<TxEnvelope>,

    /// Flattened new-state for fast overlay reads.
    pub accounts: HashMap<Address, Option<EthAccount>>,
    pub storage: HashMap<(Address, B256), B256>,

    /// Full revm BundleState preserving old/new values for reth's StateWriter.
    pub bundle: BundleState,
}

impl StagedBlock {
    pub fn from_bundle(
        seq: SeqNum,
        parent_seq: SeqNum,
        parent_id: BlockId,
        header: &Header,
        bundle: BundleState,
        receipts: &[ReceiptEnvelope],
        transactions: &[TxEnvelope],
    ) -> Self {
        let mut accounts = HashMap::new();
        let mut storage = HashMap::new();

        for (address, account) in &bundle.state {
            match &account.info {
                Some(info) => {
                    let code_hash = if info.code_hash == alloy_consensus::constants::KECCAK_EMPTY {
                        None
                    } else {
                        Some(info.code_hash.0)
                    };
                    accounts.insert(*address, Some(EthAccount {
                        nonce: info.nonce,
                        balance: info.balance,
                        code_hash,
                        is_delegated: false,
                    }));
                }
                None => {
                    if account.original_info.is_some() {
                        accounts.insert(*address, None);
                    }
                }
            }

            for (slot, storage_slot) in &account.storage {
                let key = B256::from(slot.to_be_bytes());
                let value = B256::from(storage_slot.present_value.to_be_bytes());
                storage.insert((*address, key), value);
            }
        }

        Self {
            seq,
            parent_seq,
            parent_id,
            accounts,
            storage,
            header: header.clone(),
            receipts: receipts.to_vec(),
            transactions: transactions.to_vec(),
            bundle,
        }
    }
}

/// In-memory overlay that holds proposed-but-not-yet-finalized blocks.
///
/// BFT consensus may have multiple concurrent proposals at the same height
/// (forks).  The overlay stores each proposed block keyed by `(SeqNum,
/// BlockId)` and supports chain-following reads that walk from a given tip
/// back to the finalized base.
pub struct Overlay {
    blocks: HashMap<(SeqNum, BlockId), StagedBlock>,
}

impl Overlay {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
        }
    }

    pub fn insert(&mut self, seq: SeqNum, block_id: BlockId, staged: StagedBlock) {
        self.blocks.insert((seq, block_id), staged);
    }

    pub fn contains(&self, seq: SeqNum, block_id: &BlockId) -> bool {
        self.blocks.contains_key(&(seq, *block_id))
    }

    /// Read an account by walking the proposed chain from `(seq, block_id)`
    /// back toward the finalized base.
    ///
    /// Returns:
    ///   `Some(Some(acc))` — account found in overlay (exists)
    ///   `Some(None)`      — account confirmed deleted in overlay
    ///   `None`            — overlay has no info, caller should read from reth
    pub fn read_account_in_chain(
        &self,
        seq: SeqNum,
        block_id: BlockId,
        address: &Address,
        finalized_seq: SeqNum,
    ) -> Option<Option<EthAccount>> {
        let mut cur_seq = seq;
        let mut cur_id = block_id;

        loop {
            if let Some(staged) = self.blocks.get(&(cur_seq, cur_id)) {
                if let Some(acc_opt) = staged.accounts.get(address) {
                    return Some(*acc_opt);
                }
                if cur_seq <= finalized_seq {
                    return None;
                }
                cur_seq = staged.parent_seq;
                cur_id = staged.parent_id;
            } else {
                return None;
            }
        }
    }

    /// Read a storage slot by walking the proposed chain.
    pub fn read_storage_in_chain(
        &self,
        seq: SeqNum,
        block_id: BlockId,
        address: &Address,
        slot: &B256,
        finalized_seq: SeqNum,
    ) -> Option<B256> {
        let mut cur_seq = seq;
        let mut cur_id = block_id;

        loop {
            if let Some(staged) = self.blocks.get(&(cur_seq, cur_id)) {
                if let Some(val) = staged.storage.get(&(*address, *slot)) {
                    return Some(*val);
                }
                if cur_seq <= finalized_seq {
                    return None;
                }
                cur_seq = staged.parent_seq;
                cur_id = staged.parent_id;
            } else {
                return None;
            }
        }
    }

    /// Read bytecode by walking the proposed chain, checking each block's
    /// `bundle.contracts` map.
    pub fn read_code_in_chain(
        &self,
        seq: SeqNum,
        block_id: BlockId,
        code_hash: &B256,
        finalized_seq: SeqNum,
    ) -> Option<revm::bytecode::Bytecode> {
        let mut cur_seq = seq;
        let mut cur_id = block_id;

        loop {
            if let Some(staged) = self.blocks.get(&(cur_seq, cur_id)) {
                if let Some(bytecode) = staged.bundle.contracts.get(code_hash) {
                    return Some(bytecode.clone());
                }
                if cur_seq <= finalized_seq {
                    return None;
                }
                cur_seq = staged.parent_seq;
                cur_id = staged.parent_id;
            } else {
                return None;
            }
        }
    }

    /// Read header from the overlay for the exact `(seq, block_id)`.
    pub fn read_header(&self, seq: SeqNum, block_id: &BlockId) -> Option<&Header> {
        self.blocks.get(&(seq, *block_id)).map(|s| &s.header)
    }

    /// Collect the chain of staged blocks from `finalized_seq + 1` up to
    /// `(target_seq, target_id)`, in order from oldest to newest.
    pub fn collect_chain(
        &self,
        target_seq: SeqNum,
        target_id: BlockId,
        finalized_seq: SeqNum,
    ) -> Vec<&StagedBlock> {
        let mut chain = Vec::new();
        let mut cur_seq = target_seq;
        let mut cur_id = target_id;

        loop {
            if cur_seq <= finalized_seq {
                break;
            }
            match self.blocks.get(&(cur_seq, cur_id)) {
                Some(staged) => {
                    chain.push(staged);
                    cur_seq = staged.parent_seq;
                    cur_id = staged.parent_id;
                }
                None => break,
            }
        }

        chain.reverse();
        chain
    }

    /// Remove all staged blocks at or below the finalized sequence number.
    pub fn gc(&mut self, finalized_seq: SeqNum) {
        self.blocks.retain(|&(seq, _), _| seq > finalized_seq);
    }
}
