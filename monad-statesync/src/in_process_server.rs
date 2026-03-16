// Copyright (C) 2025 Category Labs, Inc.
//
// Licensed under the GNU General Public License v3.0.
//
// Replaces ipc.rs: receives peer requests, calls StateSyncProvider trait, returns Response.
// Used in Live mode after process merge.

use std::sync::Arc;

use futures::channel::oneshot;
use monad_crypto::certificate_signature::PubKey;
use monad_execution_engine::statesync::{StateSyncProvider, StateSyncRequest as EngineRequest};
use monad_executor_glue::{
    StateSyncNetworkMessage, StateSyncResponse, StateSyncUpsertType,
    StateSyncUpsertV1, SELF_STATESYNC_VERSION,
};
use monad_types::NodeId;

/// In-process statesync server that replaces UDS-based StateSyncIpc.
/// Calls StateSyncProvider trait directly instead of sending requests over Unix socket.
pub(crate) struct InProcessStateSyncServer<PT: PubKey> {
    pub request_tx: tokio::sync::mpsc::Sender<(NodeId<PT>, StateSyncNetworkMessage)>,
    pub response_rx:
        tokio::sync::mpsc::Receiver<(NodeId<PT>, StateSyncNetworkMessage, oneshot::Sender<()>)>,
}

impl<PT: PubKey> InProcessStateSyncServer<PT> {
    pub fn new(provider: Arc<dyn StateSyncProvider>) -> Self {
        let (request_tx, mut request_rx) =
            tokio::sync::mpsc::channel::<(NodeId<PT>, StateSyncNetworkMessage)>(10);
        let (response_tx, response_rx) = tokio::sync::mpsc::channel(100);

        tokio::spawn(async move {
            while let Some((from, message)) = request_rx.recv().await {
                match message {
                    StateSyncNetworkMessage::Request(request) => {
                        let engine_request = EngineRequest {
                            prefix: request.prefix,
                            prefix_bytes: request.prefix_bytes,
                            target: request.target,
                            from: request.from,
                            until: request.until,
                            old_target: request.old_target,
                        };

                        let response = match provider.handle_sync_request(&engine_request) {
                            Ok(result) => {
                                let upserts = result
                                    .upserts
                                    .into_iter()
                                    .map(|(upsert_type, data)| {
                                        let ut = convert_upsert_type(upsert_type);
                                        StateSyncUpsertV1::new(ut, data.into())
                                    })
                                    .collect();

                                StateSyncNetworkMessage::Response(StateSyncResponse {
                                    version: SELF_STATESYNC_VERSION,
                                    nonce: 0,
                                    response_index: 0,
                                    request,
                                    response: upserts,
                                    response_n: result.done.n,
                                })
                            }
                            Err(e) => {
                                tracing::error!(?e, "statesync provider error");
                                continue;
                            }
                        };

                        let (completion_sender, _completion_receiver) = oneshot::channel();
                        if response_tx
                            .send((from, response, completion_sender))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    StateSyncNetworkMessage::Completion(_) => {
                        // In-process: completions are no-ops since there is no TCP batching
                    }
                    _ => {
                        tracing::warn!("unexpected message type in InProcessStateSyncServer");
                    }
                }
            }
        });

        Self {
            request_tx,
            response_rx,
        }
    }

    pub fn pending_request_len(&self) -> usize {
        0
    }

    pub fn is_servicing_request(&self) -> bool {
        false
    }

    pub fn num_syncdone_success(&self) -> usize {
        0
    }

    pub fn num_syncdone_failed(&self) -> usize {
        0
    }

    pub fn total_service_time_us(&self) -> usize {
        0
    }
}

fn convert_upsert_type(
    t: monad_execution_engine::statesync::types::SyncUpsertType,
) -> StateSyncUpsertType {
    match t {
        monad_execution_engine::statesync::types::SyncUpsertType::Code => {
            StateSyncUpsertType::Code
        }
        monad_execution_engine::statesync::types::SyncUpsertType::Account => {
            StateSyncUpsertType::Account
        }
        monad_execution_engine::statesync::types::SyncUpsertType::Storage => {
            StateSyncUpsertType::Storage
        }
        monad_execution_engine::statesync::types::SyncUpsertType::AccountDelete => {
            StateSyncUpsertType::AccountDelete
        }
        monad_execution_engine::statesync::types::SyncUpsertType::StorageDelete => {
            StateSyncUpsertType::StorageDelete
        }
        monad_execution_engine::statesync::types::SyncUpsertType::Header => {
            StateSyncUpsertType::Header
        }
    }
}
