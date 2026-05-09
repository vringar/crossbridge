//! Peer discovery via the per-repo socket directory.

use anyhow::{anyhow, Context, Result};
use std::io;
use std::path::Path;

/// Enumerate currently-registered peers by listing `*.socket` files in
/// `socket_dir` (which is `<socket_root>/<own-slug>/` in production; see
/// [`crate::socket_root`]).
///
/// Returns the peer slugs sorted alphabetically with the `.socket` suffix
/// stripped. An existing-but-empty directory yields an empty Vec.
///
/// # Errors
/// Returns an error if the directory does not exist (the caller maps that
/// to the spec's "not registered with crossbridge (no socket dir)" message).
pub fn list_peers(socket_dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(socket_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(anyhow!("not registered with crossbridge (no socket dir)"));
        }
        Err(e) => {
            return Err(e).context(format!(
                "failed to read socket directory {}",
                socket_dir.display()
            ));
        }
    };
    let mut peers = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(slug) = name.strip_suffix(".socket") {
            if !slug.is_empty() {
                peers.push(slug.to_string());
            }
        }
    }
    peers.sort();
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::list_peers;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn empty_dir_yields_empty() {
        let dir = tempdir().unwrap();
        let peers = list_peers(dir.path()).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn missing_dir_errors() {
        let dir = tempdir().unwrap();
        let nonexistent = dir.path().join("nope");
        let err = list_peers(&nonexistent).unwrap_err();
        assert!(
            err.to_string().contains("not registered with crossbridge"),
            "got: {err}"
        );
    }

    #[test]
    fn lists_socket_suffixes_only() {
        let dir = tempdir().unwrap();
        File::create(dir.path().join("firmware.socket")).unwrap();
        File::create(dir.path().join("tools.socket")).unwrap();
        File::create(dir.path().join("readme.txt")).unwrap();
        let peers = list_peers(dir.path()).unwrap();
        assert_eq!(peers, vec!["firmware".to_string(), "tools".to_string()]);
    }

    #[test]
    fn ignores_non_socket_suffix_entries() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested.socket.d")).unwrap();
        File::create(dir.path().join("ghidra.socket")).unwrap();
        let peers = list_peers(dir.path()).unwrap();
        assert_eq!(peers, vec!["ghidra".to_string()]);
    }
}
