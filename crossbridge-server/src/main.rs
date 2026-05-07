//! `crossbridge-server` binary entry point.
//!
//! See `.design/server.md` for the full specification. The bulk of the logic
//! lives in `lib.rs` modules; this file is just the CLI + tokio runtime
//! bootstrap.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use crossbridge_server::paths::{resolve_runtime_root, SocketLayout};
use crossbridge_server::run::{self, ServerConfig};
use crossbridge_server::slug;

#[derive(Debug, Parser)]
#[command(
    name = "crossbridge-server",
    about = "Per-repo crossbridge server: registers with supervisor, owns one repo's crosslink DB."
)]
struct Cli {
    /// Peer group (e.g. "amd-psp"). Required.
    #[arg(long)]
    group: String,

    /// Repo slug. If omitted, derived from the origin remote of `--repo-path`.
    #[arg(long)]
    slug: Option<String>,

    /// Path to the repo root. Defaults to current directory.
    #[arg(long, default_value = ".")]
    repo_path: PathBuf,

    /// Override the runtime socket root (default `/run/crossbridge`). Mainly
    /// useful for tests and dev environments where `/run/crossbridge` is not
    /// writable.
    ///
    /// Resolution precedence: this flag > `CROSSBRIDGE_SOCKET_ROOT` env var
    /// > compiled-in default (`/run/crossbridge`).
    #[arg(long)]
    runtime_root: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crossbridge_server=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let repo_path = cli
        .repo_path
        .canonicalize()
        .with_context(|| format!("resolving --repo-path {}", cli.repo_path.display()))?;

    let slug = match cli.slug {
        Some(s) => s,
        None => slug::derive_from_repo(&repo_path)
            .with_context(|| format!("deriving slug from {}", repo_path.display()))?,
    };

    let runtime_root = resolve_runtime_root(cli.runtime_root.as_deref(), |k| std::env::var_os(k));

    let cfg = ServerConfig {
        slug: slug.clone(),
        group: cli.group.clone(),
        repo_path: repo_path.clone(),
        layout: SocketLayout::new(runtime_root.clone()),
    };

    tracing::info!(
        slug = %slug,
        group = %cli.group,
        repo_path = %repo_path.display(),
        runtime_root = %runtime_root.display(),
        "starting crossbridge-server"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    runtime.block_on(run::run(cfg))
}
