//! Repo slug derivation from the `origin` remote.
//!
//! The slug is the last path segment of the remote URL with an optional
//! `.git` suffix stripped. Matches the supervisor / repo-server convention
//! so a client and its peer server agree on what to call the repo.

use anyhow::{anyhow, Context, Result};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crossbridge_protocol::own_slug_from_env;

/// Parse a slug out of an origin URL like `git@github.com:org/repo.git` or
/// `https://example.com/org/repo`.
///
/// Returns `None` if the URL has no recognizable path segment (empty or
/// trailing-slash-only input).
#[must_use]
pub fn parse_origin_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Take everything after the last '/' or ':'. Both schemes (ssh-style
    // `git@host:org/repo` and https-style `https://host/org/repo`) end with
    // `<sep><slug>` where `<sep>` is one of these.
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("");
    let stripped = last.strip_suffix(".git").unwrap_or(last);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// Resolve the client's own slug with this precedence:
/// 1. `flag` (e.g. `--slug firmware`)
/// 2. `$CROSSBRIDGE_OWN_SLUG`, via the supplied `env_lookup`
/// 3. derive from the repo's `origin` remote ([`derive_own_slug`])
///
/// Step (3) is the historical behaviour. The flag and env hooks exist for
/// repos with no `origin` remote (fresh local clones, ephemeral worktrees)
/// where derivation would fail with `cannot determine repo slug from git
/// remote`.
///
/// `env_lookup` is parameterized so tests can inject env values without
/// touching the global process environment.
///
/// # Errors
/// Returns an error only when all three steps fail to produce a slug.
pub fn resolve_own_slug<F>(flag: Option<&str>, env_lookup: F, repo_root: &Path) -> Result<String>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(s) = flag {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("--slug must be a non-empty string"));
        }
        return Ok(trimmed.to_string());
    }
    if let Some(s) = own_slug_from_env(env_lookup) {
        return Ok(s);
    }
    derive_own_slug(repo_root)
}

/// Run `git remote get-url origin` (or `jj git remote list` if a `.jj`
/// directory is present) inside `repo_root` and parse the slug.
///
/// # Errors
/// Returns an error if neither command yields a parseable origin URL.
pub fn derive_own_slug(repo_root: &Path) -> Result<String> {
    let url = read_origin_url(repo_root)?;
    parse_origin_url(&url).ok_or_else(|| anyhow!("cannot determine repo slug from git remote"))
}

fn read_origin_url(repo_root: &Path) -> Result<String> {
    if repo_root.join(".jj").is_dir() {
        if let Some(url) = jj_origin_url(repo_root) {
            return Ok(url);
        }
    }
    git_origin_url(repo_root)
}

fn git_origin_url(repo_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "get-url", "origin"])
        .output()
        .context("cannot determine repo slug from git remote")?;
    if !output.status.success() {
        return Err(anyhow!("cannot determine repo slug from git remote"));
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        return Err(anyhow!("cannot determine repo slug from git remote"));
    }
    Ok(s)
}

fn jj_origin_url(repo_root: &Path) -> Option<String> {
    let output = Command::new("jj")
        .arg("--repository")
        .arg(repo_root)
        .args(["git", "remote", "list"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Format: "<name> <url>".
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("");
        let url = parts.next().unwrap_or("");
        if name == "origin" && !url.is_empty() {
            return Some(url.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_origin_url, resolve_own_slug};
    use crossbridge_protocol::OWN_SLUG_ENV;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn resolve_flag_wins_over_env_and_derive() {
        let resolved = resolve_own_slug(
            Some("from-flag"),
            |_| Some(OsString::from("from-env")),
            Path::new("/does/not/exist"),
        )
        .expect("flag value should resolve");
        assert_eq!(resolved, "from-flag");
    }

    #[test]
    fn resolve_flag_trims_whitespace() {
        let resolved =
            resolve_own_slug(Some("  firmware\n"), |_| None, Path::new("/does/not/exist"))
                .expect("trimmed flag should resolve");
        assert_eq!(resolved, "firmware");
    }

    #[test]
    fn resolve_flag_empty_errors() {
        let err = resolve_own_slug(Some("   "), |_| None, Path::new("/does/not/exist"))
            .expect_err("empty flag should fail");
        assert!(err.to_string().contains("--slug"));
    }

    #[test]
    fn resolve_env_used_when_flag_absent() {
        let resolved = resolve_own_slug(
            None,
            |k| (k == OWN_SLUG_ENV).then(|| OsString::from("from-env")),
            Path::new("/does/not/exist"),
        )
        .expect("env value should resolve");
        assert_eq!(resolved, "from-env");
    }

    #[test]
    fn resolve_falls_through_to_derive_when_neither_set() {
        let err = resolve_own_slug(None, |_| None, Path::new("/does/not/exist"))
            .expect_err("derivation should fail in a path with no git/jj repo");
        assert!(err.to_string().contains("cannot determine repo slug"));
    }

    #[test]
    fn ssh_url_with_dot_git() {
        assert_eq!(
            parse_origin_url("git@github.com:AMD-PSP/firmware.git").as_deref(),
            Some("firmware"),
        );
    }

    #[test]
    fn https_url_no_dot_git() {
        assert_eq!(
            parse_origin_url("https://github.com/AMD-PSP/firmware").as_deref(),
            Some("firmware"),
        );
    }

    #[test]
    fn https_url_with_dot_git_and_trailing_whitespace() {
        assert_eq!(
            parse_origin_url("https://example.com/org/tools.git\n").as_deref(),
            Some("tools"),
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(parse_origin_url(""), None);
        assert_eq!(parse_origin_url("   "), None);
    }

    #[test]
    fn trailing_slash_yields_none() {
        assert_eq!(parse_origin_url("https://example.com/org/"), None);
    }

    #[test]
    fn bare_name() {
        assert_eq!(parse_origin_url("repo.git").as_deref(), Some("repo"));
    }
}
