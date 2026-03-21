// Tool to generate a fresh genesis forkpoint RLP/TOML file.
// Usage: cargo run --example gen-genesis-forkpoint -- <output_path.rlp>
// Example:
//   cargo run -p monad-node --example gen-genesis-forkpoint \
//     -- docker/devnet/monad/config/forkpoint.genesis.rlp

use std::{path::PathBuf, process};

use monad_node_config::{ExecutionProtocolType, SignatureCollectionType, SignatureType};
use monad_state::Forkpoint;
use monad_consensus_types::checkpoint::Checkpoint;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <output_path>", args[0]);
        eprintln!("  output_path must end with .rlp or .toml");
        process::exit(1);
    }
    let out_path = PathBuf::from(&args[1]);

    let genesis: Forkpoint<SignatureType, SignatureCollectionType, ExecutionProtocolType> =
        Forkpoint::genesis();
    let checkpoint: &Checkpoint<SignatureType, SignatureCollectionType, ExecutionProtocolType> =
        &genesis.0;

    let ext = out_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("rlp");

    match ext {
        "rlp" => {
            let bytes = checkpoint.to_rlp_bytes();
            std::fs::write(&out_path, &bytes).expect("failed to write .rlp file");
            println!("Written genesis forkpoint RLP ({} bytes) to {:?}", bytes.len(), out_path);
        }
        "toml" => {
            let toml_str = checkpoint
                .try_to_toml_string()
                .expect("failed to serialize to TOML");
            std::fs::write(&out_path, &toml_str).expect("failed to write .toml file");
            println!("Written genesis forkpoint TOML to {:?}", out_path);
        }
        other => {
            eprintln!("Unknown extension '{}'. Use .rlp or .toml", other);
            process::exit(1);
        }
    }
}
