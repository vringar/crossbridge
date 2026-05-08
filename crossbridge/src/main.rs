mod config;
mod route;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "crossbridge",
    about = "Cross-project coordination bridge for crosslink repositories"
)]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "crossbridge.toml")]
    config: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crossbridge=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = config::Config::load(&cli.config)?;

    tracing::info!(repos = config.repos.len(), "crossbridge starting cycle");

    route::run_cycle(&config);

    tracing::info!("crossbridge cycle complete");
    Ok(())
}
