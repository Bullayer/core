use std::path::Path;
use std::sync::Arc;

use reth_chainspec::ChainSpec;

use monad_crypto::certificate_signature::{
    CertificateSignaturePubKey, CertificateSignatureRecoverable,
};
use monad_execution_engine::statesync::StateSyncTraversable;
use monad_execution_engine::traits::ExecutionDb;
use monad_validator::signature_collection::SignatureCollection;

use crate::adapter::RethExecutionDb;
use crate::error::Result;
use crate::state_backend::RethStateBackend;

/// Everything needed to plug reth storage into the execution engine and
/// consensus layer.
pub struct RethNodeComponents<ST, SCT>
where
    ST: CertificateSignatureRecoverable,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>>,
{
    pub execution_db: Box<dyn ExecutionDb>,
    pub traversable_db: Arc<dyn StateSyncTraversable>,
    pub state_backend: RethStateBackend<ST, SCT>,
}

/// Open the reth-backed database at `db_path` and return all components
/// required by the node.
///
/// A single [`ProviderFactory`] is created and shared (via clone) across all
/// components so that only one MDBX environment is opened for the path.
pub fn open_reth_node<ST, SCT>(
    db_path: &Path,
    chain_spec: Arc<ChainSpec>,
    runtime: reth_tasks::Runtime,
) -> Result<RethNodeComponents<ST, SCT>>
where
    ST: CertificateSignatureRecoverable + 'static,
    SCT: SignatureCollection<NodeIdPubKey = CertificateSignaturePubKey<ST>> + 'static,
{
    let factory = RethExecutionDb::create_factory(db_path, chain_spec.clone(), runtime)?;

    let exec_db = RethExecutionDb::from_factory(factory.clone())?;
    let traversable_db: Arc<dyn StateSyncTraversable> =
        Arc::new(RethExecutionDb::from_factory(factory.clone())?);
    let state_backend = RethStateBackend::<ST, SCT>::from_factory(factory);

    Ok(RethNodeComponents {
        execution_db: Box::new(exec_db),
        traversable_db,
        state_backend,
    })
}
