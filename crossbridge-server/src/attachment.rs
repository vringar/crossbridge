//! Materialize `Attachment` payloads as a fresh jj commit in the local repo.
//!
//! Flow:
//! 1. Pick a unique workspace name + path under `<repo>/.crossbridge-tmp/`.
//! 2. `jj workspace add --name <n> <path>` — creates a new working copy.
//! 3. Write each attachment file into `<path>/<filename>`.
//! 4. `jj describe -R <path> -m "..."` — auto-snapshots and sets the message.
//! 5. Read the working-copy commit id with a templated `jj log`.
//! 6. `jj workspace forget --name <n>` — drop the workspace tracking.
//! 7. `rm -rf <path>` — clean up the on-disk directory.
//!
//! On any failure the function still attempts step 6+7, so a partial run does
//! not leak workspaces.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use crossbridge_protocol::Attachment;

/// What [`materialize`] produced: the new commit's full git SHA and the list
/// of filenames recorded in it.
#[derive(Debug, Clone)]
pub struct MaterializedAttachment {
    pub commit_id: String,
    pub filenames: Vec<String>,
}

/// Materialize `attachments` as one commit in the jj repo at `repo_path`.
/// Returns the new commit id (full git SHA) and the filenames written.
///
/// # Errors
/// Returns an error if `attachments` is empty, `repo_path` is not a jj/git
/// repo, the temp workspace cannot be created, any filename is empty or
/// contains path separators, or any of the underlying `jj` invocations fail.
pub fn materialize(repo_path: &Path, attachments: &[Attachment]) -> Result<MaterializedAttachment> {
    if attachments.is_empty() {
        return Err(anyhow!("no attachments to materialize"));
    }
    if !repo_path.join(".jj").is_dir() && !repo_path.join(".git").exists() {
        return Err(anyhow!(
            "{} is neither a jj nor a git repo",
            repo_path.display()
        ));
    }

    let unique = unique_name();
    let tmp_root = repo_path.join(".crossbridge-tmp");
    std::fs::create_dir_all(&tmp_root)
        .with_context(|| format!("creating {}", tmp_root.display()))?;
    let workspace_path = tmp_root.join(&unique);
    // The destination must NOT already exist for `jj workspace add`.
    if workspace_path.exists() {
        std::fs::remove_dir_all(&workspace_path)?;
    }

    let result = (|| -> Result<MaterializedAttachment> {
        run_jj(
            repo_path,
            &[
                "workspace",
                "add",
                "--name",
                &unique,
                workspace_path.to_str().ok_or_else(|| {
                    anyhow!("workspace path {} not utf-8", workspace_path.display())
                })?,
            ],
        )
        .context("jj workspace add")?;

        let mut filenames = Vec::with_capacity(attachments.len());
        for a in attachments {
            if a.filename.is_empty() {
                return Err(anyhow!("attachment with empty filename"));
            }
            // Reject path traversal outright — filenames are not paths.
            if a.filename.contains('/') || a.filename.contains('\\') {
                return Err(anyhow!(
                    "attachment filename `{}` must not contain path separators",
                    a.filename
                ));
            }
            let dest = workspace_path.join(&a.filename);
            std::fs::write(&dest, &a.data)
                .with_context(|| format!("writing attachment {}", dest.display()))?;
            filenames.push(a.filename.clone());
        }

        let msg = format!("crossbridge attachment: {}", filenames.join(", "));
        run_jj(&workspace_path, &["describe", "-m", &msg]).context("jj describe")?;

        let commit_id = jj_stdout(
            &workspace_path,
            &["log", "-r", "@", "--no-graph", "-T", "commit_id"],
        )
        .context("jj log -r @")?
        .trim()
        .to_string();

        if commit_id.is_empty() {
            return Err(anyhow!("jj log returned empty commit_id"));
        }

        Ok(MaterializedAttachment {
            commit_id,
            filenames,
        })
    })();

    // Always try to clean up, even on failure mid-flight.
    let _ = run_jj(repo_path, &["workspace", "forget", &unique]);
    let _ = std::fs::remove_dir_all(&workspace_path);

    result
}

/// Render the comment that the handler attaches to the issue after a
/// successful [`materialize`].
#[must_use]
pub fn format_comment(rec: &MaterializedAttachment) -> String {
    let files = rec.filenames.join(", ");
    format!(
        "crossbridge: materialized attachment(s) [{files}] as commit {}",
        rec.commit_id
    )
}

fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("xb-attach-{}-{}", std::process::id(), nanos)
}

fn run_jj(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("jj")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("running `jj {}`", args.join(" ")))?;
    if !out.status.success() {
        return Err(anyhow!(
            "`jj {}` failed in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn jj_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("jj")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("running `jj {}`", args.join(" ")))?;
    if !out.status.success() {
        return Err(anyhow!(
            "`jj {}` failed in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Best-effort path used by tests to confirm leftover workspaces don't survive.
#[must_use]
pub fn tmp_root(repo_path: &Path) -> PathBuf {
    repo_path.join(".crossbridge-tmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn jj_available() -> bool {
        Command::new("jj")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_jj_repo(dir: &Path) -> Result<()> {
        run_jj(dir, &["git", "init", "."])?;
        Ok(())
    }

    #[test]
    fn rejects_path_traversal_in_filename() {
        let dir = tempdir().unwrap();
        init_jj_repo(dir.path()).unwrap_or_else(|e| {
            if !jj_available() {
                eprintln!("skipping: jj not available ({e})");
                return;
            }
            panic!("init_jj_repo failed: {e}");
        });
        if !jj_available() {
            return;
        }
        let attachments = vec![Attachment {
            filename: "../escape.bin".to_string(),
            data: vec![1, 2, 3],
        }];
        let err = materialize(dir.path(), &attachments).unwrap_err();
        assert!(format!("{err:#}").contains("path separators"));
    }

    #[test]
    fn materialize_creates_jj_commit_and_cleans_up() {
        if !jj_available() {
            eprintln!("skipping: jj not on PATH");
            return;
        }
        let dir = tempdir().unwrap();
        init_jj_repo(dir.path()).unwrap();

        let attachments = vec![Attachment {
            filename: "coverage.bin".to_string(),
            data: b"binary-payload".to_vec(),
        }];
        let rec = materialize(dir.path(), &attachments).unwrap();
        assert!(!rec.commit_id.is_empty());
        assert_eq!(rec.filenames, vec!["coverage.bin"]);

        // Workspace dir is gone.
        let leftover = tmp_root(dir.path());
        if leftover.exists() {
            // tmp_root parent is allowed to exist as long as it's empty.
            let entries: Vec<_> = std::fs::read_dir(&leftover).unwrap().collect();
            assert!(
                entries.is_empty(),
                "leftover workspace files: {:?}",
                entries
                    .iter()
                    .map(|e| e.as_ref().unwrap().path())
                    .collect::<Vec<_>>()
            );
        }

        // The commit is visible in `jj log` of the original repo.
        let stdout = jj_stdout(
            dir.path(),
            &[
                "log",
                "-r",
                "all()",
                "--no-graph",
                "-T",
                "commit_id ++ \"\\n\"",
            ],
        )
        .unwrap();
        assert!(
            stdout.lines().any(|l| l.trim() == rec.commit_id),
            "commit {} not in `jj log all()`:\n{stdout}",
            rec.commit_id
        );
    }

    #[test]
    fn format_comment_includes_sha_and_files() {
        let rec = MaterializedAttachment {
            commit_id: "abc1234".to_string(),
            filenames: vec!["a.bin".to_string(), "b.bin".to_string()],
        };
        let s = format_comment(&rec);
        assert!(s.contains("abc1234"));
        assert!(s.contains("a.bin"));
        assert!(s.contains("b.bin"));
    }
}
