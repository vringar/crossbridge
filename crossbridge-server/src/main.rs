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
use crossbridge_server::slug::resolve_slug;

#[derive(Debug, Parser)]
#[command(
    name = "crossbridge-server",
    about = "Per-repo crossbridge server: registers with supervisor, owns one repo's crosslink DB."
)]
struct Cli {
    /// Peer group (e.g. "amd-psp"). Required.
    #[arg(long)]
    group: String,

    /// Repo slug.
    ///
    /// Resolution precedence: this flag > `$CROSSBRIDGE_OWN_SLUG` > derived
    /// from the `origin` remote of `--repo-path`. Pass this (or set the env
    /// var) in a repo with no `origin` remote (fresh local clones, ephemeral
    /// worktrees) where derivation would fail.
    #[arg(long)]
    slug: Option<String>,

    /// Path to the repo root. Defaults to current directory.
    #[arg(long, default_value = ".")]
    repo_path: PathBuf,

    /// Override the runtime socket root. Mainly useful for tests and dev
    /// environments that need an isolated socket tree.
    ///
    /// Resolution precedence: this flag > `$CROSSBRIDGE_SOCKET_ROOT` >
    /// `$XDG_RUNTIME_DIR/crossbridge` > compiled-in `/run/crossbridge`.
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

    let slug = resolve_slug(cli.slug.as_deref(), |k| std::env::var_os(k), &repo_path)?;

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
