use std::path::Path;
use std::sync::Arc;

use alloy_consensus::{Header, ReceiptEnvelope, TxEnvelope};
use alloy_primitives::{Address, B256, Bytes};
use reth_chainspec::ChainSpec;
use reth_db::{mdbx::DatabaseArguments, ClientVersion, DatabaseEnv};
use reth_db_api::cursor::DbCursorRO;
use reth_db_api::tables;
use reth_db_api::transaction::{DbTx, DbTxMut};
use reth_db_api::models::StoredBlockBodyIndices;
use reth_ethereum_engine_primitives::EthEngineTypes;
use reth_execution_types::ExecutionOutcome;
use reth_node_types::{AnyNodeTypesWithEngine, NodeTypesWithDBAdapter};
use reth_provider::{
    providers::{ProviderFactory, StaticFileProvider},
    DatabaseProviderFactory, OriginalValuesKnown, StateWriter, StateWriteConfig,
};
use reth_storage_api::{
    AccountReader, DBProvider, HistoryWriter, StateProvider,
};
use reth_trie::{HashedPostState, KeccakKeyHasher};
use revm::database::states::BundleState;
use tracing::{debug, error, info, warn};

use monad_crypto::certificate_signature::{
    CertificateSignaturePubKey, CertificateSignatureRecoverable,
};
use monad_eth_types::EthAccount;
use monad_execution_engine::traits::ExecutionDb;
use monad_execution_engine::types::{CodeMap, StateDeltas};
use monad_types::{BlockId, SeqNum};
use monad_validator::signature_collection::SignatureCollection;

use crate::bridge;
use crate::error::{DbError, Result};
use crate::overlay::{Overlay, StagedBlock};

/// Concrete `NodeTypes` for our chain: Ethereum primitives + engine types + ChainSpec + EthStorage.
pub(crate) type BullayerNodeTypes = AnyNodeTypesWithEngine<
    reth_ethereum_primitives::EthPrimitives,
    EthEngineTypes,
    ChainSpec,
    reth_storage_api::EthStorage,
    EthEngineTypes,
>;

pub(crate) type BullayerTypes = NodeTypesWithDBAdapter<BullayerNodeTypes, DatabaseEnv>;

/// reth-backed implementation of [`ExecutionDb`].
///
/// Layer 2 (in-memory [`Overlay`]) holds proposed-but-not-finalized blocks.
/// Layer 1 (reth ProviderFactory → MDBX + RocksDB) holds finalized state.
///
/// Reads walk the overlay first; on miss they fall through to reth.
/// `commit` writes to the overlay; `finalize` flushes the winning chain
/// to the database via reth's StateWriter.
pub struct RethExecutionDb {
    factory: ProviderFactory<BullayerTypes>,
    overlay: Overlay,

    current_seq: SeqNum,
    current_block_id: BlockId,

    latest_finalized: SeqNum,
    latest_verified: SeqNum,
}

impl RethExecutionDb {
    /// Open or create a reth-backed database at `path`.
    ///
    /// Uses a test runtime — prefer [`open_with_runtime`] for production.
    pub fn open(path: &Path, chain_spec: Arc<ChainSpec>) -> Result<Self> {
        Self::open_with_runtime(path, chain_spec, reth_tasks::Runtime::test())
    }

    /// Open or create a reth-backed database at `path` with a caller-supplied
    /// [`reth_tasks::Runtime`] for parallel I/O.
    pub fn open_with_runtime(
        path: &Path,
        chain_spec: Arc<ChainSpec>,
        runtime: reth_tasks::Runtime,
    ) -> Result<Self> {
        let factory = Self::create_factory(path, chain_spec, runtime)?;
        Self::from_factory(factory)
    }

    /// Create from an already-initialised [`ProviderFactory`].
    /// Used by [`crate::node::open_reth_node`] to share a single factory.
    pub(crate) fn from_factory(factory: ProviderFactory<BullayerTypes>) -> Result<Self> {
        let latest_finalized = Self::mdbx_last_block_number(&factory);

        info!(latest_finalized = latest_finalized.0, "opened RethExecutionDb");

        let zero_block_id = BlockId(monad_types::Hash([0u8; 32]));

        // Recover current_block_id from the canonical header at latest_finalized.
        let current_block_id = if latest_finalized.0 > 0 {
            let provider = factory.database_provider_ro()
                .map_err(|e| DbError::Internal(format!("ro for block id: {e}")))?;
            provider
                .tx_ref()
                .get::<tables::CanonicalHeaders>(latest_finalized.0)
                .ok()
                .flatten()
                .map(|hash| BlockId(monad_types::Hash(hash.0)))
                .unwrap_or(zero_block_id)
        } else {
            zero_block_id
        };

        Ok(Self {
            factory,
            overlay: Overlay::new(),
            current_seq: latest_finalized,
            current_block_id,
            latest_finalized,
            latest_verified: latest_finalized,
        })
    }

    /// Create a read-write [`ProviderFactory`] rooted at `path`.
    pub(crate) fn create_factory(
        path: &Path,
        chain_spec: Arc<ChainSpec>,
        runtime: reth_tasks::Runtime,
    ) -> Result<ProviderFactory<BullayerTypes>> {
        let db_path = path.join("mdbx");
        std::fs::create_dir_all(&db_path)
            .map_err(|e| DbError::Internal(format!("create dir: {e}")))?;

        let db = reth_db::init_db(
            &db_path,
            DatabaseArguments::new(ClientVersion::default()),
        )
        .map_err(|e| DbError::Internal(format!("init_db: {e}")))?;

        let static_files_path = path.join("static_files");
        std::fs::create_dir_all(&static_files_path)
            .map_err(|e| DbError::Internal(format!("create static_files dir: {e}")))?;

        let static_file_provider = StaticFileProvider::read_write(&static_files_path)
            .map_err(|e| DbError::Internal(format!("static file provider: {e}")))?;

        let rocksdb_path = path.join("rocksdb");
        std::fs::create_dir_all(&rocksdb_path)
            .map_err(|e| DbError::Internal(format!("create rocksdb dir: {e}")))?;

        let rocksdb_provider =
            reth_provider::providers::RocksDBProvider::builder(&rocksdb_path)
                .build()
                .map_err(|e| DbError::Internal(format!("rocksdb provider: {e}")))?;

        let factory = ProviderFactory::<BullayerTypes>::new(
            db,
            chain_spec,
            static_file_provider,
            rocksdb_provider,
            runtime,
        )?;

        Ok(factory)
    }

    /// Read the highest block number from the MDBX `CanonicalHeaders` table.
    ///
    /// Unlike `ProviderFactory::last_block_number()` which reads from static
    /// files, this reads directly from the MDBX table that we actually write to.
    pub(crate) fn mdbx_last_block_number(factory: &ProviderFactory<BullayerTypes>) -> SeqNum {
        let provider = match factory.database_provider_ro() {
            Ok(p) => p,
            Err(e) => {
                warn!(%e, "mdbx_last_block_number: failed to open RO provider");
                return SeqNum(0);
            }
        };
        let mut cursor = match provider.tx_ref().cursor_read::<tables::CanonicalHeaders>() {
            Ok(c) => c,
            Err(e) => {
                warn!(%e, "mdbx_last_block_number: failed to open cursor");
                return SeqNum(0);
            }
        };
        match cursor.last() {
            Ok(Some((block_num, _))) => SeqNum(block_num),
            Ok(None) => SeqNum(0),
            Err(e) => {
                warn!(%e, "mdbx_last_block_number: cursor.last() failed");
                SeqNum(0)
            }
        }
    }

    /// Initialise the genesis block (block 0) if the database is empty.
    pub fn init_genesis(&self, header: &Header, state_deltas: &StateDeltas, code: &CodeMap) -> Result<()> {
        // Check whether block 0 already exists in MDBX, not via static files
        let has_genesis = self.factory.database_provider_ro()
            .ok()
            .and_then(|p| p.tx_ref().get::<tables::CanonicalHeaders>(0).ok().flatten())
            .is_some();

        if has_genesis {
            debug!("genesis already initialized, skipping");
            return Ok(());
        }

        let bundle = bridge::state_deltas_to_bundle(state_deltas, code);

        // Compute hashed state before moving bundle into ExecutionOutcome
        let hashed = HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle.state.iter());

        let outcome = ExecutionOutcome::new(
            bundle,
            Vec::<Vec<reth_ethereum_primitives::Receipt>>::new(),
            0,
            Vec::new(),
        );

        let provider_rw = self.factory.database_provider_rw()?;

        // Write headers + body indices FIRST (write_state may need them).
        let block_hash = header.hash_slow();
        let tx = provider_rw.tx_ref();
        tx.put::<tables::CanonicalHeaders>(0, block_hash)?;
        tx.put::<tables::HeaderNumbers>(block_hash, 0)?;
        tx.put::<tables::Headers>(0, header.clone())?;
        tx.put::<tables::BlockBodyIndices>(0, StoredBlockBodyIndices {
            first_tx_num: 0,
            tx_count: 0,
        })?;

        provider_rw.write_state(
            &outcome,
            OriginalValuesKnown::Yes,
            StateWriteConfig {
                write_receipts: false,
                ..Default::default()
            },
        )?;

        provider_rw.write_hashed_state(&hashed.into_sorted())?;

        DBProvider::commit(provider_rw)?;
        info!("genesis block initialized");
        Ok(())
    }

    /// Read an account from the finalized state via reth's provider.
    pub(crate) fn reth_read_account(&self, address: &Address) -> Option<EthAccount> {
        let provider = self.factory.latest().ok()?;
        let acc = provider.basic_account(address).ok().flatten()?;
        Some(bridge::reth_to_monad_account(&acc))
    }

    /// Read a storage slot from the finalized state via reth's provider.
    pub(crate) fn reth_read_storage(&self, address: &Address, slot: &B256) -> B256 {
        let provider = match self.factory.latest() {
            Ok(p) => p,
            Err(_) => return B256::ZERO,
        };
        match StateProvider::storage(&*provider, *address, *slot) {
            Ok(Some(val)) => B256::from(val.to_be_bytes()),
            _ => B256::ZERO,
        }
    }

    /// Read a header from the MDBX `Headers` table.
    ///
    /// We bypass reth's `HeaderProvider` because that delegates to the static
    /// file provider, which we do not write to.
    pub(crate) fn reth_read_header(&self, seq: SeqNum) -> Option<Header> {
        let provider = self.factory.database_provider_ro().ok()?;
        provider.tx_ref().get::<tables::Headers>(seq.0).ok().flatten()
    }

    /// Read bytecode from the finalized state via reth's provider.
    pub(crate) fn reth_read_code(&self, code_hash: &B256) -> Option<Bytes> {
        let provider = self.factory.database_provider_ro().ok()?;
        let bytecode = provider.tx_ref().get::<tables::Bytecodes>(*code_hash).ok().flatten()?;
        Some(Bytes::from(bytecode.bytes_slice().to_vec()))
    }

    /// Create a read-only view for StateSyncTraversable.
    /// Shares the same underlying ProviderFactory (MDBX + RocksDB) so that
    /// finalized state flushed by [`Self::finalize`] is immediately visible.
    pub fn clone_traversable(&self) -> Arc<dyn monad_execution_engine::statesync::StateSyncTraversable> {
        Arc::new(RethTraversableDb {
            factory: self.factory.clone(),
        })
    }

    pub fn factory(&self) -> &ProviderFactory<BullayerTypes> {
        &self.factory
    }

    /// Create a [`RethStateBackend`] sharing the same MDBX database.
    ///
    /// Finalized blocks written by [`Self::finalize`] become immediately
    /// visible to the returned backend.
    pub fn clone_state_backend<ST, SCT>(&self) -> crate::state_backend::RethStateBackend<ST, SCT>
    where
        ST: CertificateSignatureRecoverable,
        SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
    {
        crate::state_backend::RethStateBackend::from_factory(self.factory.clone())
    }

    pub(crate) fn latest_finalized(&self) -> SeqNum {
        self.latest_finalized
    }

    pub(crate) fn set_latest_finalized(&mut self, seq: SeqNum) {
        self.latest_finalized = seq;
    }

    /// Flush the overlay chain to reth's storage.
    ///
    /// Performs the full persistence pipeline:
    /// 1. Write plain state + changesets (`write_state`)
    /// 2. Write hashed state for trie (`write_hashed_state`)
    /// 3. Write block headers + body indices
    /// 4. Update history indices
    fn flush_to_reth(&self, chain: &[&StagedBlock]) -> Result<()> {
        if chain.is_empty() {
            return Ok(());
        }

        let first_block = chain[0].seq.0;
        let last_block = chain[chain.len() - 1].seq.0;

        let mut merged_bundle = BundleState::default();
        let mut all_receipts: Vec<Vec<reth_ethereum_primitives::Receipt>> = Vec::new();

        for staged in chain {
            merged_bundle.extend(staged.bundle.clone());
            let block_receipts: Vec<reth_ethereum_primitives::Receipt> = staged
                .receipts
                .iter()
                .map(bridge::receipt_envelope_to_reth)
                .collect();
            all_receipts.push(block_receipts);
        }

        let hashed = HashedPostState::from_bundle_state::<KeccakKeyHasher>(merged_bundle.state.iter());

        let outcome = ExecutionOutcome::new(
            merged_bundle,
            all_receipts,
            first_block,
            Vec::new(),
        );

        let provider_rw = self.factory.database_provider_rw()?;

        // 1. Write block headers + canonical mapping + body indices FIRST.
        //    write_state needs BlockBodyIndices to exist when writing receipts.
        let mut next_tx_num: u64 = provider_rw
            .tx_ref()
            .cursor_read::<tables::BlockBodyIndices>()?
            .last()?
            .map(|(_, indices)| indices.first_tx_num + indices.tx_count as u64)
            .unwrap_or(0);

        let tx = provider_rw.tx_ref();
        for staged in chain {
            let block_num = staged.seq.0;
            let block_hash = staged.header.hash_slow();
            tx.put::<tables::CanonicalHeaders>(block_num, block_hash)?;
            tx.put::<tables::HeaderNumbers>(block_hash, block_num)?;
            tx.put::<tables::Headers>(block_num, staged.header.clone())?;

            let tx_count = staged.transactions.len() as u64;
            tx.put::<tables::BlockBodyIndices>(block_num, StoredBlockBodyIndices {
                first_tx_num: next_tx_num,
                tx_count,
            })?;
            next_tx_num += tx_count;
        }

        // 2. Write plain state (accounts, storage, bytecodes, changesets)
        //    Receipts are NOT written here to avoid static file segment conflicts.
        provider_rw.write_state(
            &outcome,
            OriginalValuesKnown::Yes,
            StateWriteConfig {
                write_receipts: false,
                ..Default::default()
            },
        )?;

        // 3. Write hashed state for trie lookups
        provider_rw.write_hashed_state(&hashed.into_sorted())?;

        // 4. Update history indices for the block range
        provider_rw.update_history_indices(first_block..=last_block)?;

        DBProvider::commit(provider_rw)?;

        Ok(())
    }
}

impl ExecutionDb for RethExecutionDb {
    fn has_executed(&self, block_id: &BlockId, seq_num: SeqNum) -> bool {
        self.overlay.contains(seq_num, block_id) || seq_num <= self.latest_finalized
    }

    fn get_latest_finalized_version(&self) -> SeqNum {
        self.latest_finalized
    }

    fn read_account(&self, address: &Address) -> Option<EthAccount> {
        if let Some(result) = self.overlay.read_account_in_chain(
            self.current_seq,
            self.current_block_id,
            address,
            self.latest_finalized,
        ) {
            return result;
        }
        self.reth_read_account(address)
    }

    fn read_storage(&self, address: &Address, slot: &B256) -> B256 {
        if let Some(val) = self.overlay.read_storage_in_chain(
            self.current_seq,
            self.current_block_id,
            address,
            slot,
            self.latest_finalized,
        ) {
            return val;
        }
        self.reth_read_storage(address, slot)
    }

    fn read_code(&self, code_hash: &B256) -> Option<Bytes> {
        if let Some(bytecode) = self.overlay.read_code_in_chain(
            self.current_seq,
            self.current_block_id,
            code_hash,
            self.latest_finalized,
        ) {
            return Some(Bytes::from(bytecode.bytes_slice().to_vec()));
        }
        self.reth_read_code(code_hash)
    }

    fn read_eth_header(&self) -> Header {
        if let Some(h) = self.overlay.read_header(self.current_seq, &self.current_block_id) {
            return h.clone();
        }
        self.reth_read_header(self.current_seq)
            .unwrap_or_default()
    }

    fn set_block_and_prefix(&mut self, seq_num: SeqNum, block_id: BlockId) {
        self.current_seq = seq_num;
        self.current_block_id = block_id;
    }

    fn commit(
        &mut self,
        block_id: BlockId,
        header: &Header,
        bundle: BundleState,
        receipts: &[ReceiptEnvelope],
        transactions: &[TxEnvelope],
    ) {
        let block_seq = SeqNum(header.number);
        let staged = StagedBlock::from_bundle(
            block_seq,
            self.current_seq,
            self.current_block_id,
            header,
            bundle,
            receipts,
            transactions,
        );
        self.overlay.insert(block_seq, block_id, staged);
    }

    fn finalize(&mut self, seq_num: SeqNum, block_id: BlockId) {
        let chain = self
            .overlay
            .collect_chain(seq_num, block_id, self.latest_finalized);

        if chain.is_empty() {
            warn!(?seq_num, ?block_id, "finalize: empty chain");
            return;
        }

        if let Err(e) = self.flush_to_reth(&chain) {
            error!(%e, "failed to flush to reth storage");
            return;
        }

        self.latest_finalized = seq_num;
        self.overlay.gc(seq_num);

        debug!(finalized = seq_num.0, "finalized via reth StateWriter");
    }

    fn update_voted_metadata(&mut self, _seq_num: SeqNum, _block_id: BlockId) {}

    fn update_proposed_metadata(&mut self, _seq_num: SeqNum, _block_id: BlockId) {}

    fn update_verified_block(&mut self, seq_num: SeqNum) {
        self.latest_verified = seq_num;
    }
}

/// Read-only view of the reth database for state sync traversal.
/// Shares the same [`ProviderFactory`] as [`RethExecutionDb`], so
/// finalized data is visible immediately after flush.
pub(crate) struct RethTraversableDb {
    pub(crate) factory: ProviderFactory<BullayerTypes>,
}
