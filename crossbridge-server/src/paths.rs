//! Filesystem paths for the supervisor register socket and per-peer listener
//! sockets under the active socket root.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crossbridge_protocol::default_socket_root;

/// Resolve the server's runtime root with this precedence:
/// 1. `flag` (e.g. `--runtime-root /custom/run`)
/// 2. otherwise [`default_socket_root`] (`$CROSSBRIDGE_SOCKET_ROOT` >
///    `$XDG_RUNTIME_DIR/crossbridge` > compiled-in `/run/crossbridge`)
///
/// `env_lookup` is parameterized so tests can inject env values without
/// touching the global process environment.
pub fn resolve_runtime_root<F>(flag: Option<&Path>, env_lookup: F) -> PathBuf
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    default_socket_root(env_lookup)
}

/// Filename of the supervisor register socket.
pub const REGISTER_SOCKET_NAME: &str = "register.socket";

/// Layout helper: locates the supervisor register socket and per-peer listener
/// sockets under a chosen root. Use [`resolve_runtime_root`] to pick the root
/// from CLI/env, then construct the layout via [`SocketLayout::new`].
#[derive(Debug, Clone)]
pub struct SocketLayout {
    root: PathBuf,
}

impl SocketLayout {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path to the supervisor's register socket: `<root>/register.socket`.
    #[must_use]
    pub fn register_socket(&self) -> PathBuf {
        self.root.join(REGISTER_SOCKET_NAME)
    }

    /// Path to the directory holding listening sockets that target `peer_slug`:
    /// `<root>/<peer_slug>/`. Each repo server in `peer_slug`'s peer group puts
    /// one listening socket in this directory.
    #[must_use]
    pub fn peer_dir(&self, peer_slug: &str) -> PathBuf {
        self.root.join(peer_slug)
    }

    /// Path to *our* listening socket inside `peer_slug`'s directory:
    /// `<root>/<peer_slug>/<own_slug>.socket`. Clients in `peer_slug` connect
    /// here to submit work to us.
    #[must_use]
    pub fn listener_socket(&self, peer_slug: &str, own_slug: &str) -> PathBuf {
        self.peer_dir(peer_slug).join(format!("{own_slug}.socket"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbridge_protocol::{DEFAULT_SOCKET_ROOT, SOCKET_ROOT_ENV, XDG_RUNTIME_DIR_ENV};

    #[test]
    fn layout_paths_under_custom_root() {
        let l = SocketLayout::new("/tmp/xb");
        assert_eq!(
            l.register_socket(),
            PathBuf::from("/tmp/xb/register.socket")
        );
        assert_eq!(l.peer_dir("repo-a"), PathBuf::from("/tmp/xb/repo-a"));
        assert_eq!(
            l.listener_socket("repo-a", "repo-b"),
            PathBuf::from("/tmp/xb/repo-a/repo-b.socket")
        );
    }

    #[test]
    fn resolve_runtime_root_flag_only_wins() {
        let flag = PathBuf::from("/custom/run");
        let resolved = resolve_runtime_root(Some(&flag), |_| None);
        assert_eq!(resolved, flag);
    }

    #[test]
    fn resolve_runtime_root_crossbridge_env_used_when_no_flag() {
        let resolved =
            resolve_runtime_root(None, |k| (k == SOCKET_ROOT_ENV).then(|| "/srv/run".into()));
        assert_eq!(resolved, PathBuf::from("/srv/run"));
    }

    #[test]
    fn resolve_runtime_root_falls_back_to_xdg_runtime_dir() {
        let resolved = resolve_runtime_root(None, |k| {
            (k == XDG_RUNTIME_DIR_ENV).then(|| "/run/user/1000".into())
        });
        assert_eq!(resolved, PathBuf::from("/run/user/1000/crossbridge"));
    }

    #[test]
    fn resolve_runtime_root_flag_overrides_env() {
        let flag = PathBuf::from("/custom/run");
        let resolved = resolve_runtime_root(Some(&flag), |_| Some(OsString::from("/srv/run")));
        assert_eq!(resolved, flag);
    }

    #[test]
    fn resolve_runtime_root_neither_falls_back_to_default() {
        let resolved = resolve_runtime_root(None, |_| None);
        assert_eq!(resolved, PathBuf::from(DEFAULT_SOCKET_ROOT));
    }
}
