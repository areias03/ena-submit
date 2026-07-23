//! `ena-submit` binary entry point.

mod cli;
// Skeleton modules: some public items are consumed by later milestones (input, manifest, webin,
// history). Allow dead code here until those callers land rather than scatter per-item attributes.
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod model;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    // Logs go to stderr; level controlled by RUST_LOG (default: info).
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let cli = cli::Cli::parse();
    cli::run(cli).context("ena-submit failed")
}
