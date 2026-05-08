//! Per-peer Unix listening sockets.
//!
//! For every same-group peer the supervisor reports, the server creates one
//! `UnixListener` at `<root>/<peer>/<own>.socket` so clients in `<peer>` can
//! submit work to us. Each listener runs on a dedicated tokio task; accepted
//! connections are forwarded over a shared mpsc channel that the main event
//! loop awaits. On peer removal we abort the task and unlink the socket file.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::paths::SocketLayout;

/// Accepted connection payload sent from a listener task to the main loop.
pub type Accepted = (String, UnixStream);

struct Entry {
    abort: AbortHandle,
    path: PathBuf,
}

/// The set of currently-active per-peer listeners, keyed by peer slug.
pub struct ListenerSet {
    own_slug: String,
    layout: SocketLayout,
    tx: mpsc::UnboundedSender<Accepted>,
    entries: HashMap<String, Entry>,
}

impl ListenerSet {
    pub fn new(
        own_slug: impl Into<String>,
        layout: SocketLayout,
    ) -> (Self, mpsc::UnboundedReceiver<Accepted>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                own_slug: own_slug.into(),
                layout,
                tx,
                entries: HashMap::new(),
            },
            rx,
        )
    }

    #[must_use]
    pub fn own_slug(&self) -> &str {
        &self.own_slug
    }

    #[must_use]
    pub fn layout(&self) -> &SocketLayout {
        &self.layout
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Add a listener for `peer_slug`. Idempotent: an existing listener for the
    /// same peer is dropped first (its socket file unlinked, task aborted)
    /// before binding a new one.
    ///
    /// # Errors
    /// Returns an error if the peer directory cannot be created or the listener
    /// socket cannot be bound at `<peer_dir>/<own_slug>.socket`.
    pub fn add(&mut self, peer_slug: &str) -> io::Result<()> {
        let dir = self.layout.peer_dir(peer_slug);
        std::fs::create_dir_all(&dir)?;
        let path = self.layout.listener_socket(peer_slug, &self.own_slug);

        // Remove any prior entry first.
        self.remove(peer_slug);

        if path.exists() {
            // Stale socket file — remove before bind.
            let _ = std::fs::remove_file(&path);
        }

        let listener = UnixListener::bind(&path)?;
        let slug = peer_slug.to_string();
        let tx = self.tx.clone();
        let task = tokio::spawn(listen_loop(slug.clone(), listener, tx));
        let abort = task.abort_handle();
        self.entries.insert(
            peer_slug.to_string(),
            Entry {
                abort,
                path: path.clone(),
            },
        );
        tracing::info!(peer = peer_slug, path = %path.display(), "added peer listener");
        Ok(())
    }

    /// Remove the listener for `peer_slug` (if any) and unlink its socket file.
    pub fn remove(&mut self, peer_slug: &str) -> bool {
        if let Some(entry) = self.entries.remove(peer_slug) {
            entry.abort.abort();
            let _ = std::fs::remove_file(&entry.path);
            tracing::info!(
                peer = peer_slug,
                path = %entry.path.display(),
                "removed peer listener"
            );
            true
        } else {
            false
        }
    }

    /// Drop all listeners and unlink their socket files. Used on supervisor
    /// disconnect (peers are unknown without supervisor) and on shutdown.
    pub fn clear(&mut self) {
        let slugs: Vec<String> = self.entries.keys().cloned().collect();
        for s in slugs {
            self.remove(&s);
        }
    }
}

impl Drop for ListenerSet {
    fn drop(&mut self) {
        self.clear();
    }
}

async fn listen_loop(slug: String, listener: UnixListener, tx: mpsc::UnboundedSender<Accepted>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if tx.send((slug.clone(), stream)).is_err() {
                    // Receiver dropped — main loop is shutting down.
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(peer = %slug, "accept failed: {e}; stopping listener");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ShortTempDir;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn add_creates_socket_file() {
        let dir = ShortTempDir::new();
        let layout = SocketLayout::new(dir.path());
        let (mut set, _rx) = ListenerSet::new("own", layout.clone());
        set.add("peer-a").unwrap();
        let path = layout.listener_socket("peer-a", "own");
        assert!(
            path.exists(),
            "socket file should exist at {}",
            path.display()
        );
        assert_eq!(set.len(), 1);
    }

    #[tokio::test]
    async fn add_replaces_stale_socket() {
        let dir = ShortTempDir::new();
        let layout = SocketLayout::new(dir.path());
        std::fs::create_dir_all(layout.peer_dir("peer-a")).unwrap();
        std::fs::write(layout.listener_socket("peer-a", "own"), b"stale").unwrap();

        let (mut set, _rx) = ListenerSet::new("own", layout.clone());
        set.add("peer-a").unwrap();
        let path = layout.listener_socket("peer-a", "own");
        // Listener is alive: connecting should succeed.
        let _stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    }

    #[tokio::test]
    async fn remove_unlinks_socket_file() {
        let dir = ShortTempDir::new();
        let layout = SocketLayout::new(dir.path());
        let (mut set, _rx) = ListenerSet::new("own", layout.clone());
        set.add("peer-a").unwrap();
        let path = layout.listener_socket("peer-a", "own");
        assert!(path.exists());
        assert!(set.remove("peer-a"));
        assert!(!path.exists(), "socket file must be unlinked on remove");
        assert!(set.is_empty());
    }

    #[tokio::test]
    async fn drop_clears_all() {
        let dir = ShortTempDir::new();
        let layout = SocketLayout::new(dir.path());
        let path_a;
        let path_b;
        {
            let (mut set, _rx) = ListenerSet::new("own", layout.clone());
            set.add("peer-a").unwrap();
            set.add("peer-b").unwrap();
            path_a = layout.listener_socket("peer-a", "own");
            path_b = layout.listener_socket("peer-b", "own");
            assert!(path_a.exists() && path_b.exists());
        }
        assert!(!path_a.exists());
        assert!(!path_b.exists());
    }

    #[tokio::test]
    async fn accepted_connection_is_forwarded() {
        let dir = ShortTempDir::new();
        let layout = SocketLayout::new(dir.path());
        let (mut set, mut rx) = ListenerSet::new("own", layout.clone());
        set.add("peer-a").unwrap();
        set.add("peer-b").unwrap();

        // Connect to peer-b's socket and send some bytes.
        let path = layout.listener_socket("peer-b", "own");
        let connect = tokio::spawn(async move {
            let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
            s.write_all(b"hi").await.unwrap();
        });

        let (slug, _stream) = rx.recv().await.expect("expected an accept");
        assert_eq!(slug, "peer-b");
        connect.await.unwrap();
    }
}
