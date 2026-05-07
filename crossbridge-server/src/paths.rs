//! Filesystem paths for the supervisor register socket and per-peer listener
//! sockets under `/run/crossbridge/`.

use std::path::{Path, PathBuf};

/// Default root for crossbridge runtime sockets.
pub const DEFAULT_RUNTIME_ROOT: &str = "/run/crossbridge";

/// Filename of the supervisor register socket.
pub const REGISTER_SOCKET_NAME: &str = "register.socket";

/// Layout helper: locates the supervisor register socket and per-peer listener
/// sockets under a chosen root. The root is configurable purely so tests can
/// avoid touching the real `/run/crossbridge`.
#[derive(Debug, Clone)]
pub struct SocketLayout {
    root: PathBuf,
}

impl SocketLayout {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self { root: root.into() }
    }

    /// Default layout rooted at `/run/crossbridge`.
    pub fn default_root() -> Self {
        Self::new(DEFAULT_RUNTIME_ROOT)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path to the supervisor's register socket: `<root>/register.socket`.
    pub fn register_socket(&self) -> PathBuf {
        self.root.join(REGISTER_SOCKET_NAME)
    }

    /// Path to the directory holding listening sockets that target `peer_slug`:
    /// `<root>/<peer_slug>/`. Each repo server in `peer_slug`'s peer group puts
    /// one listening socket in this directory.
    pub fn peer_dir(&self, peer_slug: &str) -> PathBuf {
        self.root.join(peer_slug)
    }

    /// Path to *our* listening socket inside `peer_slug`'s directory:
    /// `<root>/<peer_slug>/<own_slug>.socket`. Clients in `peer_slug` connect
    /// here to submit work to us.
    pub fn listener_socket(&self, peer_slug: &str, own_slug: &str) -> PathBuf {
        self.peer_dir(peer_slug).join(format!("{own_slug}.socket"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_paths() {
        let l = SocketLayout::default_root();
        assert_eq!(
            l.register_socket(),
            PathBuf::from("/run/crossbridge/register.socket")
        );
        assert_eq!(
            l.peer_dir("repo-a"),
            PathBuf::from("/run/crossbridge/repo-a")
        );
        assert_eq!(
            l.listener_socket("repo-a", "repo-b"),
            PathBuf::from("/run/crossbridge/repo-a/repo-b.socket")
        );
    }

    #[test]
    fn custom_root() {
        let l = SocketLayout::new("/tmp/xb");
        assert_eq!(
            l.register_socket(),
            PathBuf::from("/tmp/xb/register.socket")
        );
        assert_eq!(
            l.listener_socket("a", "b"),
            PathBuf::from("/tmp/xb/a/b.socket")
        );
    }
}
