// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Decorator pattern: wraps ExecutionDb to intercept commit/finalize for deletion tracking.
// Mirrors C++ monad_statesync_server_context (statesync_server_context.cpp).

use std::sync::{Arc, RwLock};

use alloy_consensus::{Header, ReceiptEnvelope, TxEnvelope};
use alloy_primitives::{Address, B256, Bytes};
use monad_eth_types::EthAccount;
use monad_types::{BlockId, SeqNum};
use revm::database::states::BundleState;

use crate::traits::ExecutionDb;

use super::deletion_tracker::{extract_deletions_from_bundle, FinalizedDeletions, ProposedDeletions};
use super::server::handle_sync_request;
use super::{
    StateSyncError, StateSyncProvider, StateSyncRequest, StateSyncResult, StateSyncTraversable,
};

/// ExecutionDb decorator that tracks deletions on commit/finalize.
pub struct StateSyncServerDb {
    inner: Box<dyn ExecutionDb>,
    proposed_deletions: ProposedDeletions,
    finalized_deletions: Arc<RwLock<FinalizedDeletions>>,
}

impl StateSyncServerDb {
    pub fn new(
        inner: Box<dyn ExecutionDb>,
        finalized_deletions: Arc<RwLock<FinalizedDeletions>>,
    ) -> Self {
        Self {
            inner,
            proposed_deletions: ProposedDeletions::new(),
            finalized_deletions,
        }
    }

    pub fn finalized_deletions(&self) -> Arc<RwLock<FinalizedDeletions>> {
        Arc::clone(&self.finalized_deletions)
    }
}

impl ExecutionDb for StateSyncServerDb {
    fn has_executed(&self, block_id: &BlockId, seq_num: SeqNum) -> bool {
        self.inner.has_executed(block_id, seq_num)
    }

    fn get_latest_finalized_version(&self) -> SeqNum {
        self.inner.get_latest_finalized_version()
    }

    fn read_account(&self, address: &Address) -> Option<EthAccount> {
        self.inner.read_account(address)
    }

    fn read_storage(&self, address: &Address, slot: &B256) -> B256 {
        self.inner.read_storage(address, slot)
    }

    fn read_code(&self, code_hash: &B256) -> Option<Bytes> {
        self.inner.read_code(code_hash)
    }

    fn read_eth_header(&self) -> Header {
        self.inner.read_eth_header()
    }

    fn set_block_and_prefix(&mut self, seq_num: SeqNum, block_id: BlockId) {
        self.inner.set_block_and_prefix(seq_num, block_id);
    }

    fn commit(
        &mut self,
        block_id: BlockId,
        header: &Header,
        bundle: BundleState,
        receipts: &[ReceiptEnvelope],
        transactions: &[TxEnvelope],
    ) {
        let deletions = extract_deletions_from_bundle(&bundle);
        self.proposed_deletions
            .push(SeqNum(header.number), block_id, deletions);
        self.inner
            .commit(block_id, header, bundle, receipts, transactions);
    }

    fn finalize(&mut self, seq_num: SeqNum, block_id: BlockId) {
        self.proposed_deletions.on_finalize(
            &mut self.finalized_deletions.write().unwrap(),
            seq_num,
            block_id,
        );
        self.inner.finalize(seq_num, block_id);
    }

    fn update_voted_metadata(&mut self, seq_num: SeqNum, block_id: BlockId) {
        self.inner.update_voted_metadata(seq_num, block_id);
    }

    fn update_proposed_metadata(&mut self, seq_num: SeqNum, block_id: BlockId) {
        self.inner.update_proposed_metadata(seq_num, block_id);
    }

    fn update_verified_block(&mut self, seq_num: SeqNum) {
        self.inner.update_verified_block(seq_num);
    }
}

/// Combines StateSyncTraversable + FinalizedDeletions for live mode serving.
pub struct LiveStateSyncProvider {
    db: Arc<dyn StateSyncTraversable>,
    deletions: Arc<RwLock<FinalizedDeletions>>,
}

impl LiveStateSyncProvider {
    pub fn new(
        db: Arc<dyn StateSyncTraversable>,
        deletions: Arc<RwLock<FinalizedDeletions>>,
    ) -> Self {
        Self { db, deletions }
    }
}

impl StateSyncProvider for LiveStateSyncProvider {
    fn handle_sync_request(
        &self,
        request: &StateSyncRequest,
    ) -> Result<StateSyncResult, StateSyncError> {
        let deletions = self.deletions.read().unwrap();
        handle_sync_request(request, &*self.db, &deletions)
    }
}
