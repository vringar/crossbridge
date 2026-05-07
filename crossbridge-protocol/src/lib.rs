//! Wire protocol for crossbridge.
//!
//! Defines the message types exchanged between supervisor, repo servers, and
//! clients, plus length-prefixed framing helpers (sync `std::io` and async
//! `tokio::io`). See `.design/protocol.md` for the full specification.

use serde::{Deserialize, Serialize};

mod framing;

pub use framing::{
    read_message, read_message_sync, write_message, write_message_sync, MAX_FRAME_SIZE,
};

/// Default runtime root for crossbridge sockets (supervisor register socket
/// and per-peer listener sockets).
pub const DEFAULT_SOCKET_ROOT: &str = "/run/crossbridge";

/// Environment variable that overrides [`DEFAULT_SOCKET_ROOT`] for all three
/// crossbridge binaries (supervisor, server, client). When set and no
/// binary-specific CLI flag is provided, the binary uses this directory as
/// its runtime root.
pub const SOCKET_ROOT_ENV: &str = "CROSSBRIDGE_SOCKET_ROOT";

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
/// Sent once immediately after connecting to `/run/crossbridge/register.socket`.
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
