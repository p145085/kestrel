//! The Kestrel IRC server.
//!
//! All protocol behaviour lives in `kestreld-core`, which is sans-io. This
//! binary is the part that owns sockets and the clock: it accepts connections,
//! frames lines, and drives the state machine, which is deliberately the only
//! place where a bug can be a networking bug rather than a protocol one.

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::warn;

use kestreld::config::Config;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kestreld=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--print-config") => {
            println!("{}", Config::default().to_toml()?);
            Ok(())
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("kestreld {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            let path = args
                .first()
                .map_or_else(|| PathBuf::from("kestreld.toml"), PathBuf::from);
            let config = if path.exists() {
                Config::load(&path)?
            } else {
                warn!(path = %path.display(), "no configuration file; using defaults");
                Config::default()
            };
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("building the async runtime")?
                .block_on(kestreld::run(config))
        }
    }
}

const USAGE: &str = "\
kestreld — the Kestrel IRC server

USAGE:
    kestreld [CONFIG]          Run, reading CONFIG (default: kestreld.toml)
    kestreld --print-config    Print a default configuration file
    kestreld --version
    kestreld --help

Set KESTRELD_LOG or RUST_LOG to control logging, e.g. KESTRELD_LOG=kestreld=debug";
