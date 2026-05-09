//! Library helpers for `crossbridge-client`.
//!
//! Pulled out of `main.rs` so the parsing and discovery logic can be unit
//! tested without spinning up real Unix sockets or git repositories.

pub mod labels;
pub mod peers;
pub mod slug;

/// Resolve the active socket root using the shared crossbridge precedence:
/// `$CROSSBRIDGE_SOCKET_ROOT` > `$XDG_RUNTIME_DIR/crossbridge` > compiled-in
/// `/run/crossbridge`. Per-peer sockets live at
/// `<root>/<own-slug>/<peer-slug>.socket`.
#[must_use]
pub fn socket_root() -> std::path::PathBuf {
    crossbridge_protocol::default_socket_root(|k| std::env::var_os(k))
}
