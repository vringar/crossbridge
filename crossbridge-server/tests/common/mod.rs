//! Shared helpers for integration tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Tempdir whose path is short enough to bind a Unix socket inside it (Linux
/// caps `bind()` paths at 108 bytes). Mirrors
/// `crossbridge_server::test_util::ShortTempDir` but lives in `tests/` so
/// integration tests can share it.
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
