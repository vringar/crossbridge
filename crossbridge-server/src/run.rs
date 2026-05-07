//! Top-level run loop: registers with the supervisor, manages peer listeners,
//! dispatches client requests to [`crate::handler`], and reconnects on stream
//! loss with exponential backoff.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use crossbridge_protocol::Notification;
use crosslink::db::Database;
use tokio::signal;
use tokio::sync::mpsc;

use crate::handler;
use crate::listeners::ListenerSet;
use crate::paths::SocketLayout;
use crate::supervisor::{connect_and_register_with_backoff, read_notification, Registration};

/// Runtime configuration for [`run`].
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub slug: String,
    pub group: String,
    pub repo_path: PathBuf,
    pub layout: SocketLayout,
}

impl ServerConfig {
    pub fn db_path(&self) -> PathBuf {
        self.repo_path.join(".crosslink").join("issues.db")
    }
}

/// Run the server forever. Returns `Ok(())` only on a clean Ctrl+C; any other
/// exit is an error.
pub async fn run(cfg: ServerConfig) -> Result<()> {
    if !cfg.db_path().exists() {
        return Err(anyhow!(
            "crosslink DB not found at {} (run `crosslink init` in {} first)",
            cfg.db_path().display(),
            cfg.repo_path.display()
        ));
    }

    let db = Database::open(&cfg.db_path())
        .with_context(|| format!("opening crosslink DB at {}", cfg.db_path().display()))?;

    let (mut listeners, mut accepted_rx) = ListenerSet::new(cfg.slug.clone(), cfg.layout.clone());
    let register_socket = cfg.layout.register_socket();

    // Outer loop: (re)connect to the supervisor; each iteration owns one
    // supervisor stream until it dies.
    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => {
                tracing::info!("ctrl-c received, shutting down");
                listeners.clear();
                return Ok(());
            }
            registration = connect_and_register_with_backoff(
                &register_socket,
                &cfg.slug,
                &cfg.group,
            ) => {
                let registration = registration?;
                if let Err(e) = serve_one_session(
                    &cfg,
                    &db,
                    &mut listeners,
                    &mut accepted_rx,
                    registration,
                ).await {
                    tracing::warn!("supervisor session ended: {e:#}");
                }
                // Session ended: drop all peer listeners and reconnect.
                listeners.clear();
            }
        }
    }
}

/// Drive one supervisor session: install the initial peer listeners from the
/// `RegisterAck`, then loop on supervisor notifications and client connections
/// until either the supervisor stream dies or the user hits Ctrl-C.
///
/// On Ctrl-C, returns `Ok(())` and the caller is expected to *not* reconnect.
async fn serve_one_session(
    cfg: &ServerConfig,
    db: &Database,
    listeners: &mut ListenerSet,
    accepted_rx: &mut mpsc::UnboundedReceiver<crate::listeners::Accepted>,
    registration: Registration,
) -> Result<()> {
    let Registration { mut stream, peers } = registration;
    for peer in &peers {
        if let Err(e) = listeners.add(peer) {
            tracing::warn!(peer, "failed to add listener: {e}");
        }
    }
    tracing::info!(
        peer_count = listeners.len(),
        "session ready, awaiting client connections"
    );

    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => {
                tracing::info!("ctrl-c received, shutting down");
                listeners.clear();
                // Closing the supervisor stream signals our departure; the
                // supervisor will fan out PeerLeft to surviving peers.
                drop(stream);
                std::process::exit(0);
            }
            notif = read_notification(&mut stream) => {
                match notif {
                    Ok(Some(Notification::PeerJoined { slug })) => {
                        if let Err(e) = listeners.add(&slug) {
                            tracing::warn!(peer = %slug, "failed to add listener on PeerJoined: {e}");
                        }
                    }
                    Ok(Some(Notification::PeerLeft { slug })) => {
                        listeners.remove(&slug);
                    }
                    Ok(None) => {
                        tracing::warn!("supervisor stream closed (EOF)");
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
            Some((peer_slug, mut conn)) = accepted_rx.recv() => {
                if let Err(e) = handler::handle_connection(
                    &mut conn, &peer_slug, db, &cfg.repo_path,
                ).await {
                    tracing::warn!(peer = %peer_slug, "handler error: {e:#}");
                }
            }
        }
    }
}
