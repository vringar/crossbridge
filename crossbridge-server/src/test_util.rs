//! Shared test helpers.
//!
//! In particular: a tempdir whose path is short enough to fit a Unix socket
//! `bind()` (which on Linux caps the path at 108 bytes including any suffix
//! you put under it). The default `tempfile::tempdir()` lives under
//! `$TMPDIR`, which in some sandboxed test environments is already > 100
//! characters long — too long to bind a socket inside it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A tempdir under `/tmp/<short-prefix>-<unique>/` that removes itself on drop.
/// The path is intentionally short so test code can append `.../peer/own.socket`
/// without busting `SUN_LEN`.
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
