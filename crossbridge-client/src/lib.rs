//! Library helpers for `crossbridge-client`.
//!
//! Pulled out of `main.rs` so the parsing and discovery logic can be unit
//! tested without spinning up real Unix sockets or git repositories.

pub mod labels;
pub mod peers;
pub mod slug;

/// Default supervisor socket root. Per-peer sockets live at
/// `<SOCKET_ROOT>/<own-slug>/<peer-slug>.socket`.
pub const SOCKET_ROOT: &str = "/run/crossbridge";

/// Environment variable that overrides [`SOCKET_ROOT`]. The supervisor
/// spec hard-codes `/run/crossbridge`; the override exists so integration
/// tests can stand up a sandbox socket tree without root.
pub const SOCKET_ROOT_ENV: &str = "CROSSBRIDGE_SOCKET_ROOT";

/// Resolve the active socket root: the env override if set, otherwise the
/// default [`SOCKET_ROOT`].
#[must_use]
pub fn socket_root() -> std::path::PathBuf {
    std::env::var_os(SOCKET_ROOT_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(SOCKET_ROOT))
}
