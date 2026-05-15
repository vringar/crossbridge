//! Wire protocol for crossbridge.
//!
//! Defines the message types exchanged between supervisor, repo servers, and
//! clients, plus length-prefixed framing helpers (sync `std::io` and async
//! `tokio::io`). See `.design/protocol.md` for the full specification.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

mod framing;

pub use framing::{
    read_message, read_message_sync, write_message, write_message_sync, MAX_FRAME_SIZE,
};

/// Last-resort fallback runtime root, used when neither
/// [`SOCKET_ROOT_ENV`] nor [`XDG_RUNTIME_DIR_ENV`] is set in the environment.
/// In normal per-user deployments crossbridge runs under
/// `$XDG_RUNTIME_DIR/crossbridge`; this constant exists so the binaries still
/// have a defined location in degenerate environments (no logind session,
/// minimal sandboxes, etc.).
pub const DEFAULT_SOCKET_ROOT: &str = "/run/crossbridge";

/// Environment variable that overrides the resolved socket root for all three
/// crossbridge binaries (supervisor, server, client). When set and no
/// binary-specific CLI flag is provided, the binary uses this directory as
/// its runtime root.
pub const SOCKET_ROOT_ENV: &str = "CROSSBRIDGE_SOCKET_ROOT";

/// Standard XDG environment variable pointing at the per-user runtime
/// directory (typically `/run/user/$UID`). Crossbridge uses
/// `$XDG_RUNTIME_DIR/crossbridge` as the default socket root.
pub const XDG_RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";

/// Environment variable that overrides the binary's own slug derivation when
/// no `--slug` CLI flag is provided. Useful in repos with no `origin` remote
/// (fresh local clones, ephemeral worktrees) where deriving from the git/jj
/// remote would fail. When set to a non-UTF-8 value the env is ignored and
/// the binary falls through to the next resolution step.
pub const OWN_SLUG_ENV: &str = "CROSSBRIDGE_OWN_SLUG";

/// Read [`OWN_SLUG_ENV`] from `env_lookup` and return its UTF-8 value, or
/// `None` if the variable is unset, empty, or non-UTF-8.
///
/// `env_lookup` is parameterized so tests can inject env values without
/// touching the global process environment.
pub fn own_slug_from_env<F>(env_lookup: F) -> Option<String>
where
    F: Fn(&str) -> Option<OsString>,
{
    let raw = env_lookup(OWN_SLUG_ENV)?;
    let s = raw.into_string().ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve the default crossbridge socket root, ignoring any per-binary CLI
/// flag.
///
/// Precedence:
/// 1. `$CROSSBRIDGE_SOCKET_ROOT` ([`SOCKET_ROOT_ENV`]) if set
/// 2. `$XDG_RUNTIME_DIR/crossbridge` ([`XDG_RUNTIME_DIR_ENV`]) if set
/// 3. Compiled-in fallback [`DEFAULT_SOCKET_ROOT`]
///
/// `env_lookup` is parameterized so tests can inject env values without
/// touching the global process environment.
pub fn default_socket_root<F>(env_lookup: F) -> PathBuf
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(root) = env_lookup(SOCKET_ROOT_ENV) {
        return PathBuf::from(root);
    }
    if let Some(xdg) = env_lookup(XDG_RUNTIME_DIR_ENV) {
        return PathBuf::from(xdg).join("crossbridge");
    }
    PathBuf::from(DEFAULT_SOCKET_ROOT)
}

/// Errors produced by framing helpers and message (de)serialization.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("postcard error: {0}")]
    Postcard(#[from] postcard::Error),

    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: usize, max: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

// --- Supervisor ↔ repo server -------------------------------------------------

/// Repo server → supervisor: identifies the server on the persistent stream.
///
/// Sent once immediately after connecting to `<socket_root>/register.socket`
/// (see [`default_socket_root`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Register {
    pub slug: String,
    pub group: String,
}

/// Supervisor → repo server: response to `Register`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegisterResponse {
    /// Registration accepted. `peers` lists slugs of currently registered
    /// same-group servers (excluding self).
    Ack { peers: Vec<String> },
    /// Registration rejected (e.g. slug already taken).
    Nack { reason: String },
}

/// Supervisor → repo server: ongoing peer membership change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Notification {
    PeerJoined { slug: String },
    PeerLeft { slug: String },
}

/// Outer envelope for messages traveling supervisor → repo server on the
/// persistent stream. Postcard variant tagging discriminates the inner type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupervisorMessage {
    RegisterResponse(RegisterResponse),
    Notification(Notification),
}

// --- Client ↔ repo server -----------------------------------------------------

/// Client → repo server: a single request over a per-peer Unix socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientRequest {
    Submit(SubmitIssue),
    Answer(SubmitAnswer),
}

/// Submit a new issue from `source_slug` (identified by `source_uuid`) to the
/// peer repo server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitIssue {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub source_slug: String,
    pub source_uuid: String,
    pub attachments: Vec<Attachment>,
}

/// Submit an answer back to a previously-received issue identified by
/// `source_uuid`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitAnswer {
    pub source_uuid: String,
    pub comments: Vec<AnswerComment>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerComment {
    pub content: String,
    /// e.g. "result", "note".
    pub kind: String,
}

/// Inline binary payload materialized as a git commit on the receiving side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub data: Vec<u8>,
}

/// Repo server → client: response to a `ClientRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerResponse {
    Ok { issue_id: i64 },
    Error { message: String },
}

#[cfg(test)]
mod tests;
