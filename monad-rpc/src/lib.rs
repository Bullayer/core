// Copyright (C) 2025 Category Labs, Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

pub mod chainstate;
pub mod cli;
pub mod comparator;
pub mod docs;
pub mod event;
pub mod handlers;
pub mod middleware;
pub mod txpool;
pub mod types;
pub mod websocket;

#[cfg(test)]
pub mod tests;

pub const MONAD_RPC_VERSION: Option<&str> = option_env!("MONAD_VERSION");

use std::{sync::Arc, time::Duration};

use actix_web::{web, App, HttpServer};
use monad_archive::archive_reader::{redact_mongo_url, ArchiveReader};
use monad_ethcall::EthCallExecutor;
use monad_execution_engine::events::ExecutionEvent;
use tracing::{debug, error, info, warn};
use tracing_actix_web::TracingLogger;

use self::{
    chainstate::{buffer::ChainStateBuffer, ChainState},
    cli::Cli,
    comparator::RpcComparator,
    event::EventServer,
    handlers::{
        resources::{MonadJsonRootSpanBuilder, MonadRpcResources},
        rpc_handler,
    },
    middleware::{DecompressionGuard, Metrics, TimingMiddleware},
    txpool::EthTxPoolBridgeClient,
};
use monad_triedb_utils::triedb_env::TriedbEnv;

/// Start the RPC server with a pre-created txpool bridge client.
/// This function handles triedb, archive readers, ethcall executor,
/// event ring, websocket, and the HTTP RPC server.
pub async fn start_rpc_server(
    args: Cli,
    node_name: String,
    chain_id: u64,
    txpool_bridge_client: Option<EthTxPoolBridgeClient>,
    execution_event_rx: Option<tokio::sync::broadcast::Receiver<ExecutionEvent>>,
) -> std::io::Result<()> {
    let concurrent_requests_limiter = Arc::new(tokio::sync::Semaphore::new(
        args.eth_call_max_concurrent_requests as usize,
    ));

    let triedb_env = args.triedb_path.clone().as_deref().map(|path| {
        TriedbEnv::new(
            path,
            args.triedb_node_lru_max_mem,
            args.triedb_max_buffered_read_requests as usize,
            args.triedb_max_async_read_concurrency as usize,
            args.triedb_max_buffered_traverse_requests as usize,
            args.triedb_max_async_traverse_concurrency as usize,
            args.max_finalized_block_cache_len as usize,
            args.max_voted_block_cache_len as usize,
        )
    });

    info!("Initializing archive readers for historical data access");

    let aws_archive_reader = match (
        &args.s3_bucket,
        &args.region,
        &args.archive_url,
        &args.archive_api_key,
    ) {
        (Some(s3_bucket), Some(region), Some(archive_url), Some(archive_api_key)) => {
            info!(
                s3_bucket,
                region, archive_url, "Initializing AWS archive reader"
            );
            match ArchiveReader::init_aws_reader(
                s3_bucket.clone(),
                Some(region.clone()),
                archive_url,
                archive_api_key,
                5,
            )
            .await
            {
                Ok(reader) => {
                    info!("AWS archive reader initialized successfully");
                    Some(reader)
                }
                Err(e) => {
                    warn!(error = %e, "Unable to initialize AWS archive reader");
                    None
                }
            }
        }
        _ => {
            debug!("AWS archive reader configuration not provided, skipping initialization");
            None
        }
    };

    let archive_reader = match (&args.mongo_db_name, &args.mongo_url) {
        (Some(db_name), Some(url)) => {
            info!(
                "Initializing MongoDB archive reader  with connection: {}, database: {}",
                redact_mongo_url(url),
                db_name
            );
            match ArchiveReader::init_mongo_reader(
                url.clone(),
                db_name.clone(),
                monad_archive::prelude::Metrics::none(),
                args.mongo_max_time_get_millis.map(Duration::from_millis),
            )
            .await
            {
                Ok(mongo_reader) => {
                    let has_aws_fallback = aws_archive_reader.is_some();
                    info!(
                        has_aws_fallback,
                        "MongoDB archive reader initialized successfully"
                    );
                    Some(mongo_reader.with_fallback(
                        aws_archive_reader,
                        args.mongo_failure_threshold,
                        args.mongo_failure_timeout_millis.map(Duration::from_millis),
                    ))
                }
                Err(e) => {
                    warn!(error = %e, "Unable to initialize MongoDB archive reader");
                    if aws_archive_reader.is_some() {
                        info!("Falling back to AWS archive reader");
                    }
                    aws_archive_reader
                }
            }
        }
        _ => {
            if aws_archive_reader.is_some() {
                info!("MongoDB configuration not provided, using AWS archive reader only");
            } else {
                info!("No archive readers configured, historical data access will be limited");
            }
            aws_archive_reader
        }
    };

    let low_pool_config = monad_ethcall::PoolConfig {
        num_threads: args.eth_call_executor_threads,
        num_fibers: args.eth_call_executor_fibers,
        timeout_sec: args.eth_call_executor_queuing_timeout,
        queue_limit: args.eth_call_max_concurrent_requests,
    };
    let high_pool_config = monad_ethcall::PoolConfig {
        num_threads: args.eth_call_high_executor_threads,
        num_fibers: args.eth_call_high_executor_fibers,
        timeout_sec: args.eth_call_high_executor_queuing_timeout,
        queue_limit: args.eth_call_high_max_concurrent_requests,
    };
    let block_pool_config = monad_ethcall::PoolConfig {
        num_threads: args.eth_trace_block_executor_threads,
        num_fibers: args.eth_trace_block_executor_fibers,
        timeout_sec: args.eth_trace_block_executor_queuing_timeout,
        queue_limit: args.eth_trace_block_max_concurrent_requests,
    };
    let tx_exec_num_fibers = args.eth_trace_tx_executor_fibers;

    let eth_call_executor = args.triedb_path.clone().as_deref().map(|path| {
        Arc::new(EthCallExecutor::new(
            low_pool_config,
            high_pool_config,
            block_pool_config,
            tx_exec_num_fibers,
            args.eth_call_executor_node_lru_max_mem,
            path,
        ))
    });

    let with_metrics = args.otel_endpoint.map(|otel_endpoint| {
        Metrics::new_with_otel_endpoint(
            otel_endpoint,
            node_name.clone(),
            std::time::Duration::from_secs(5),
        )
    });

    let decompression_guard = DecompressionGuard::new(args.max_request_size);

    let (events_client, events_for_cache) = if let Some(event_rx) = execution_event_rx {
        let events_client = EventServer::start(event_rx);

        let events_for_cache = events_client
            .subscribe()
            .expect("Failed to subscribe to event server");

        (Some(events_client), Some(events_for_cache))
    } else {
        if args.ws_enabled {
            warn!("execution event channel is not provided but is required for websockets");
        }

        (None, None)
    };

    let event_buffer = if let Some(mut events_for_cache) = events_for_cache {
        let event_buffer = Arc::new(ChainStateBuffer::new(1024));

        let event_buffer2 = event_buffer.clone();
        tokio::spawn(async move {
            loop {
                match events_for_cache.recv().await {
                    Ok(event) => event_buffer2.insert(event).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(lag_count)) => {
                        warn!(
                            ?lag_count,
                            "event server channel lagged, events will be missing"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        error!("event server closed");
                        break;
                    }
                }
            }
        });

        Some(event_buffer)
    } else {
        None
    };

    let chain_state = triedb_env
        .clone()
        .map(|t| ChainState::new(event_buffer, t, archive_reader.clone()));

    let rpc_comparator: Option<RpcComparator> = args
        .rpc_comparison_endpoint
        .as_ref()
        .map(|endpoint| RpcComparator::new(endpoint.to_string(), node_name));

    let app_state = MonadRpcResources::new(
        txpool_bridge_client,
        triedb_env,
        eth_call_executor,
        args.eth_call_executor_fibers as usize,
        archive_reader,
        chain_id,
        chain_state,
        args.batch_request_limit,
        args.max_response_size,
        args.allow_unprotected_txs,
        concurrent_requests_limiter,
        args.eth_call_max_concurrent_requests as usize,
        args.eth_get_logs_max_block_range,
        args.eth_call_provider_gas_limit,
        args.eth_estimate_gas_provider_gas_limit,
        args.eth_send_raw_transaction_sync_default_timeout_ms,
        args.eth_send_raw_transaction_sync_max_timeout_ms,
        args.dry_run_get_logs_index,
        args.use_eth_get_logs_index,
        args.max_finalized_block_cache_len,
        args.enable_admin_eth_call_statistics,
        with_metrics.clone(),
        rpc_comparator.clone(),
    );

    let ws_server_handle = if let Some(events_client) = events_client {
        let ws_app_data = app_state.clone();
        let conn_limit = websocket::handler::ConnectionLimit::new(args.ws_conn_limit);
        let sub_limit = websocket::handler::SubscriptionLimit(args.ws_sub_per_conn_limit);

        args.ws_enabled.then(|| {
            HttpServer::new(move || {
                App::new()
                    .app_data(web::Data::new(conn_limit.clone()))
                    .app_data(web::Data::new(events_client.clone()))
                    .app_data(web::Data::new(ws_app_data.clone()))
                    .app_data(web::Data::new(sub_limit.clone()))
                    .service(
                        web::resource("/").route(web::get().to(websocket::handler::ws_handler)),
                    )
            })
            .bind((args.rpc_addr.clone(), args.ws_port))
            .expect("Failed to bind WebSocket server")
            .shutdown_timeout(1)
            .workers(args.ws_worker_threads)
        })
    } else {
        None
    };

    let app = match with_metrics {
        Some(metrics) => HttpServer::new(move || {
            App::new()
                .wrap(decompression_guard.clone())
                .wrap(metrics.clone())
                .wrap(TracingLogger::<MonadJsonRootSpanBuilder>::new())
                .wrap(TimingMiddleware)
                .app_data(web::PayloadConfig::default().limit(args.max_request_size))
                .app_data(web::Data::new(app_state.clone()))
                .service(web::resource("/").route(web::post().to(rpc_handler)))
        })
        .bind((args.rpc_addr, args.rpc_port))?
        .shutdown_timeout(1)
        .workers(args.worker_threads)
        .run(),
        None => HttpServer::new(move || {
            App::new()
                .wrap(decompression_guard.clone())
                .wrap(TracingLogger::<MonadJsonRootSpanBuilder>::new())
                .wrap(TimingMiddleware)
                .app_data(web::PayloadConfig::default().limit(args.max_request_size))
                .app_data(web::Data::new(app_state.clone()))
                .service(web::resource("/").route(web::post().to(rpc_handler)))
        })
        .bind((args.rpc_addr, args.rpc_port))?
        .shutdown_timeout(1)
        .workers(args.worker_threads)
        .run(),
    };

    let ws_fut = ws_server_handle.map(|ws| ws.run());

    tokio::select! {
        result = app => {
            let () = result?;
        }

        result = async {
            if let Some(fut) = ws_fut {
                fut.await
            } else {
                futures::future::pending().await
            }
        } => {
            let () = result?;
        }
    }

    Ok(())
}
