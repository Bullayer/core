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

use std::time::Duration;

use clap::Parser;
use monad_node_config::MonadNodeConfig;
use monad_pprof::start_pprof_server;
use monad_rpc::{cli::Cli, txpool::EthTxPoolBridge, MONAD_RPC_VERSION};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{
    fmt::{format::FmtSpan, Layer as FmtLayer},
    layer::SubscriberExt,
    EnvFilter, Layer, Registry,
};

#[cfg(all(not(target_env = "msvc"), feature = "jemallocator"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(feature = "jemallocator")]
#[allow(non_upper_case_globals)]
#[export_name = "malloc_conf"]
pub static malloc_conf: &[u8] = b"prof:true,prof_active:true,lg_prof_sample:19\0";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    let args = Cli::parse();

    let node_config: MonadNodeConfig = toml::from_str(&std::fs::read_to_string(&args.node_config)?)
        .expect("node toml parse error");

    let s = Registry::default()
        .with(
            FmtLayer::default()
                .json()
                .with_span_events(FmtSpan::NONE)
                .with_current_span(false)
                .with_span_list(false)
                .with_writer(std::io::stdout)
                .with_ansi(false)
                .with_filter(EnvFilter::from_default_env()),
        )
        .with(monad_tracing_timing::TimingsLayer::new());
    tracing::subscriber::set_global_default(s).expect("failed to set logger");

    if !args.pprof.is_empty() {
        let pprof_addr = args.pprof.clone();
        tokio::spawn(async move {
            let server = match start_pprof_server(pprof_addr) {
                Ok(server) => server,
                Err(err) => {
                    error!("failed to start pprof server: {}", err);
                    return;
                }
            };
            if let Err(err) = server.await {
                error!("pprof server faiiled: {}", err);
            }
        });
    }

    MONAD_RPC_VERSION.map(|v| info!("starting monad-rpc with version {}", v));

    let (txpool_bridge_client, _txpool_bridge_handle) = if let Some(ipc_path) = &args.ipc_path {
        let mut print_message_timer = tokio::time::interval(Duration::from_secs(60));
        let mut retry_timer = tokio::time::interval(Duration::from_secs(1));
        let (txpool_bridge_client, _txpool_bridge_handle) = loop {
            tokio::select! {
                _ = print_message_timer.tick() => {
                    info!("Waiting for statesync to complete");
                }
                _= retry_timer.tick() => {
                    match EthTxPoolBridge::start(ipc_path).await  {
                        Ok((client, handle)) => {
                            info!("Statesync complete, starting RPC server");
                            break (client, handle)
                        },
                        Err(e) => {
                            debug!("caught error: {e}, retrying");
                        },
                    }
                },
            }
        };
        (Some(txpool_bridge_client), Some(_txpool_bridge_handle))
    } else {
        warn!(
            "--ipc-path is not set, tx pool will be disabled. This means that the node will not be able to send transactions."
        );
        (None, None)
    };

    rayon::ThreadPoolBuilder::new()
        .thread_name(|i| format!("monad-rpc-rn-{i}"))
        .num_threads(args.compute_threadpool_size)
        .build_global()
        .unwrap();

    let node_name = node_config.node_name;
    let chain_id = node_config.chain_id;

    monad_rpc::start_rpc_server(args, node_name, chain_id, txpool_bridge_client).await
}
