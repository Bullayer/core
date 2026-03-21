use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy_primitives::Address;
use reth_chainspec::ChainSpec;
use reth_db::{mdbx::DatabaseArguments, ClientVersion};
use reth_provider::providers::{ProviderFactory, StaticFileProvider};
use reth_storage_api::AccountReader;
use tracing::{debug, trace, warn};

use monad_crypto::certificate_signature::{
    CertificateSignaturePubKey, CertificateSignatureRecoverable,
};
use monad_eth_types::{EthAccount, EthHeader};
use monad_state_backend::{StateBackend, StateBackendError};
use monad_types::{BlockId, Epoch, SeqNum, Stake};
use monad_validator::signature_collection::{SignatureCollection, SignatureCollectionPubKeyType};

use crate::adapter::{BullayerTypes, RethExecutionDb};
use crate::bridge;

/// Read-only reth-backed [`StateBackend`] used by the consensus layer to query
/// account balances/nonces and execution results.
pub struct RethStateBackend<ST, SCT> {
    factory: ProviderFactory<BullayerTypes>,
    total_lookups: AtomicU64,
    _phantom: PhantomData<fn() -> (ST, SCT)>,
}

impl<ST, SCT> RethStateBackend<ST, SCT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    /// Open a standalone read-only instance at `path`.
    pub fn open(
        path: &Path,
        chain_spec: Arc<ChainSpec>,
        runtime: reth_tasks::Runtime,
    ) -> crate::error::Result<Self> {
        let db_path = path.join("mdbx");
        let db = reth_db::open_db_read_only(
            &db_path,
            DatabaseArguments::new(ClientVersion::default()),
        )
        .map_err(|e| crate::error::DbError::Internal(format!("open_db_read_only: {e}")))?;

        let static_files_path = path.join("static_files");
        let static_file_provider = StaticFileProvider::read_only(&static_files_path, true)
            .map_err(|e| crate::error::DbError::Internal(format!("static file provider: {e}")))?;

        let rocksdb_path = path.join("rocksdb");
        let rocksdb_provider =
            reth_provider::providers::RocksDBProvider::builder(&rocksdb_path)
                .with_read_only(true)
                .build()
                .map_err(|e| crate::error::DbError::Internal(format!("rocksdb provider: {e}")))?;

        let factory = ProviderFactory::<BullayerTypes>::new(
            db,
            chain_spec,
            static_file_provider,
            rocksdb_provider,
            runtime,
        )?;

        debug!("opened RethStateBackend (read-only)");
        Ok(Self {
            factory,
            total_lookups: AtomicU64::new(0),
            _phantom: PhantomData,
        })
    }

    /// Wrap an existing (shared) [`ProviderFactory`].
    pub fn from_factory(factory: ProviderFactory<BullayerTypes>) -> Self {
        Self {
            factory,
            total_lookups: AtomicU64::new(0),
            _phantom: PhantomData,
        }
    }

    /// Read highest block number from MDBX `CanonicalHeaders`, not static files.
    fn read_latest_finalized(&self) -> Option<SeqNum> {
        let seq = RethExecutionDb::mdbx_last_block_number(&self.factory);
        if seq.0 == 0 {
            // Distinguish "genesis exists" from "empty database".
            use reth_db_api::transaction::DbTx;
            use reth_provider::DatabaseProviderFactory;
            let provider = self.factory.database_provider_ro().ok()?;
            let has_genesis = provider
                .tx_ref()
                .get::<reth_db_api::tables::CanonicalHeaders>(0)
                .ok()
                .flatten()
                .is_some();
            if has_genesis {
                return Some(SeqNum(0));
            }
            return None;
        }
        Some(seq)
    }

    fn read_account(&self, seq_num: &SeqNum, address: &Address) -> Option<EthAccount> {
        let latest = self.read_latest_finalized()?;
        if *seq_num > latest {
            return None;
        }
        let provider = self.factory.latest().ok()?;
        let acc = provider.basic_account(address).ok().flatten()?;
        Some(bridge::reth_to_monad_account(&acc))
    }

    fn read_header(&self, seq_num: &SeqNum) -> Option<EthHeader> {
        use reth_db_api::transaction::DbTx;
        use reth_provider::DatabaseProviderFactory;
        let provider = self.factory.database_provider_ro().ok()?;
        let header = provider.tx_ref()
            .get::<reth_db_api::tables::Headers>(seq_num.0)
            .ok()
            .flatten()?;
        Some(EthHeader(header))
    }
}

impl<ST, SCT> StateBackend<ST, SCT> for RethStateBackend<ST, SCT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    fn get_account_statuses<'a>(
        &self,
        _block_id: &BlockId,
        seq_num: &SeqNum,
        _is_finalized: bool,
        addresses: impl Iterator<Item = &'a Address>,
    ) -> std::result::Result<Vec<Option<EthAccount>>, StateBackendError> {
        let latest = self
            .read_latest_finalized()
            .ok_or(StateBackendError::NotAvailableYet)?;

        if *seq_num > latest {
            trace!(?seq_num, ?latest, "block not yet finalized");
            return Err(StateBackendError::NotAvailableYet);
        }

        let results: Vec<Option<EthAccount>> = addresses
            .map(|addr| self.read_account(seq_num, addr))
            .collect();

        self.total_lookups
            .fetch_add(results.len() as u64, Ordering::Relaxed);

        Ok(results)
    }

    fn get_execution_result(
        &self,
        _block_id: &BlockId,
        seq_num: &SeqNum,
        _is_finalized: bool,
    ) -> std::result::Result<EthHeader, StateBackendError> {
        let latest = self
            .read_latest_finalized()
            .ok_or(StateBackendError::NotAvailableYet)?;

        if *seq_num > latest {
            return Err(StateBackendError::NotAvailableYet);
        }

        self.read_header(seq_num)
            .ok_or(StateBackendError::NotAvailableYet)
    }

    fn raw_read_earliest_finalized_block(&self) -> Option<SeqNum> {
        self.read_latest_finalized().map(|_| SeqNum(0))
    }

    fn raw_read_latest_finalized_block(&self) -> Option<SeqNum> {
        self.read_latest_finalized()
    }

    fn read_valset_at_block(
        &self,
        _block_num: SeqNum,
        _requested_epoch: Epoch,
    ) -> Vec<(SCT::NodeIdPubKey, SignatureCollectionPubKeyType<SCT>, Stake)> {
        warn!("read_valset_at_block: not yet implemented");
        Vec::new()
    }

    fn total_db_lookups(&self) -> u64 {
        self.total_lookups.load(Ordering::Relaxed)
    }
}
