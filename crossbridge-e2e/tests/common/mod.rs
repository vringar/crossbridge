//! Shared helpers for the workspace-level end-to-end tests.
//!
//! - `ShortTempDir`: a tempdir under `/tmp/xt-…` whose path is short enough
//!   to fit a Unix socket `sun_path` (~108 bytes on Linux). Mirrors the
//!   helper from `crossbridge-server/tests/common/mod.rs`.
//! - `ChildGuard`: a `Drop`-based wrapper around spawned binaries so the
//!   supervisor and both servers are killed and reaped on success or panic.
//! - `RepoFixture`: lays down a minimal git repo with `origin` configured
//!   and an empty crosslink DB at `.crosslink/issues.db`. Modeled on the
//!   fixture from `crossbridge-client/tests/end_to_end.rs`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Tempdir whose path is short enough to bind a Unix socket inside it.
pub struct ShortTempDir {
    path: PathBuf,
}

impl ShortTempDir {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/xt-{}-{}-{}", std::process::id(), nanos, n));
        std::fs::create_dir_all(&path).expect("creating short tempdir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ShortTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Owns a spawned child process. On drop it sends SIGKILL and reaps the
/// process so a panicking test never leaks the supervisor or server
/// processes.
pub struct ChildGuard {
    child: Option<Child>,
    label: &'static str,
}

impl ChildGuard {
    pub fn new(label: &'static str, child: Child) -> Self {
        Self {
            child: Some(child),
            label,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            // try_wait first in case it already exited (avoid spurious
            // "no such process" log noise from kill()).
            if matches!(c.try_wait(), Ok(Some(_))) {
                return;
            }
            if let Err(e) = c.kill() {
                eprintln!("ChildGuard({}): kill failed: {e}", self.label);
            }
            if let Err(e) = c.wait() {
                eprintln!("ChildGuard({}): wait failed: {e}", self.label);
            }
        }
    }
}

/// Synthetic git + crosslink repo fixture. The repo lives at
/// `<root>/<slug>/` with a remote pointing at
/// `git@example.com:org/<slug>.git` so `derive_own_slug` resolves
/// deterministically.
pub struct RepoFixture {
    pub root: PathBuf,
}

impl RepoFixture {
    pub fn new(parent: &Path, slug: &str) -> Self {
        let root = parent.join(slug);
        std::fs::create_dir_all(&root).expect("creating repo root");

        run_git(&root, &["init", "-q", "-b", "main"]);
        run_git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                &format!("git@example.com:org/{slug}.git"),
            ],
        );
        run_git(&root, &["config", "safe.directory", "*"]);

        let crosslink_dir = root.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).expect("creating .crosslink dir");
        // Initialize the schema by opening and dropping a connection.
        let db = crosslink::db::Database::open(&crosslink_dir.join("issues.db"))
            .expect("opening crosslink DB");
        drop(db);

        Self { root }
    }

    pub fn db(&self) -> crosslink::db::Database {
        crosslink::db::Database::open(&self.root.join(".crosslink").join("issues.db"))
            .expect("re-opening crosslink DB")
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Path to a workspace-built binary, derived from the running test
/// binary's location. We can't use `env!("CARGO_BIN_EXE_<name>")` here
/// because that macro is only populated for binaries declared in the
/// same package as the test; cross-package binaries in a workspace need
/// either unstable artifact-dependencies or this kind of path lookup.
///
/// The current test binary lives at
/// `<target>/<profile>/deps/<name>-<hash>`, so the workspace binaries
/// it depends on (declared as `dev-dependencies` so cargo builds them
/// before the test) live at `<target>/<profile>/<name>`.
pub fn cargo_bin(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // strip the test binary file name
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(name);
    assert!(
        path.exists(),
        "expected workspace binary at {} (dev-dependency on {} should have built it)",
        path.display(),
        name,
    );
    path
}

/// Block until `predicate` returns true, sleeping `interval` between checks.
/// Panics with `msg` once `timeout` elapses.
pub fn wait_until<F: FnMut() -> bool>(
    timeout: Duration,
    interval: Duration,
    msg: &str,
    mut predicate: F,
) {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timeout waiting for {msg} (after {timeout:?})");
        }
        std::thread::sleep(interval);
    }
}
