//! crossbridge-server: per-repo server binary library.
//!
//! See `.design/server.md` for the full specification. This crate exposes the
//! pieces of the server (slug derivation, supervisor stream lifecycle, peer
//! listener set, request handlers, attachment materialization) as a library so
//! they can be unit-tested independently of the long-running event loop in
//! `main.rs`.

pub mod attachment;
pub mod handler;
pub mod listeners;
pub mod paths;
pub mod run;
pub mod slug;
pub mod supervisor;

#[cfg(test)]
pub(crate) mod test_util;
