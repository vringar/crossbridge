use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crossbridge_supervisor::resolve_register_socket;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "crossbridge-supervisor",
    about = "Per-host crossbridge supervisor: coordinates peer-group socket topology",
    version
)]
struct Cli {
    /// Path to the register socket. The parent directory is used as the base
    /// directory for slug subdirectories and is wiped on startup.
    ///
    /// Resolution precedence: this flag > `CROSSBRIDGE_SOCKET_ROOT` env var
    /// (`<root>/register.socket`) > compiled-in default
    /// (`/run/crossbridge/register.socket`).
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let socket = resolve_register_socket(cli.socket.as_deref(), |k| std::env::var_os(k));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(crossbridge_supervisor::run(&socket))
}
