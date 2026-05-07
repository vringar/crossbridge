//! Workspace-level end-to-end integration tests for crossbridge.
//!
//! This crate is intentionally empty as a library — its purpose is to host
//! integration tests under `tests/` that spawn real `crossbridge-supervisor`,
//! `crossbridge-server`, and `crossbridge-client` binaries (via
//! `CARGO_BIN_EXE_*`) and exercise the full bidirectional issue/answer flow
//! across two synthetic repositories.
//!
//! See `.design/e2e-integration-test.md` for the full specification.
