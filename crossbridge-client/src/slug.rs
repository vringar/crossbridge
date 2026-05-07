//! Repo slug derivation from the `origin` remote.
//!
//! The slug is the last path segment of the remote URL with an optional
//! `.git` suffix stripped. Matches the supervisor / repo-server convention
//! so a client and its peer server agree on what to call the repo.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

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
        if let Some(url) = jj_origin_url(repo_root)? {
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

fn jj_origin_url(repo_root: &Path) -> Result<Option<String>> {
    let output = Command::new("jj")
        .arg("--repository")
        .arg(repo_root)
        .args(["git", "remote", "list"])
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Format: "<name> <url>".
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("");
        let url = parts.next().unwrap_or("");
        if name == "origin" && !url.is_empty() {
            return Ok(Some(url.to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::parse_origin_url;

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
