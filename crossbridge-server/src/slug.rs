//! Derive a repo slug from the `origin` remote URL of a git or jj repository.
//!
//! Supports the URL forms produced by GitHub (and most other hosts):
//!
//! - `git@github.com:org/firmware.git`
//! - `https://github.com/org/firmware.git`
//! - `https://github.com/org/firmware`
//! - `ssh://git@github.com/org/firmware.git`
//! - `/abs/path/to/firmware`  (local file remotes)
//!
//! The slug is the last path segment of the URL with a trailing `.git`
//! stripped. If the repo has a `.jj/` directory we ask `jj` for the origin
//! remote URL; otherwise we fall back to `git remote get-url origin`.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

/// Derive a slug from `repo_path`'s origin remote.
///
/// # Errors
/// Returns an error if the origin remote URL cannot be obtained (no `jj`/`git`
/// remote configured, command failure) or if the URL has no extractable
/// last path segment.
pub fn derive_from_repo(repo_path: &Path) -> Result<String> {
    let url = origin_remote_url(repo_path)?;
    parse_slug(&url).ok_or_else(|| {
        anyhow!(
            "could not parse repo slug from origin remote URL `{}` for {}",
            url,
            repo_path.display()
        )
    })
}

/// Read the origin remote URL using `jj` if a `.jj/` directory exists,
/// otherwise `git`.
fn origin_remote_url(repo_path: &Path) -> Result<String> {
    if repo_path.join(".jj").is_dir() {
        return jj_origin_url(repo_path);
    }
    git_origin_url(repo_path)
}

fn git_origin_url(repo_path: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["remote", "get-url", "origin"])
        .output()
        .with_context(|| {
            format!(
                "running `git -C {} remote get-url origin`",
                repo_path.display()
            )
        })?;
    if !out.status.success() {
        return Err(anyhow!(
            "`git remote get-url origin` failed in {}: {}",
            repo_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        return Err(anyhow!(
            "`git remote get-url origin` returned empty URL in {}",
            repo_path.display()
        ));
    }
    Ok(url)
}

fn jj_origin_url(repo_path: &Path) -> Result<String> {
    let out = Command::new("jj")
        .arg("-R")
        .arg(repo_path)
        .args(["git", "remote", "list"])
        .output()
        .with_context(|| format!("running `jj -R {} git remote list`", repo_path.display()))?;
    if !out.status.success() {
        return Err(anyhow!(
            "`jj git remote list` failed in {}: {}",
            repo_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        // jj output format: "<name> <url>"
        let mut parts = line.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").trim();
        let url = parts.next().unwrap_or("").trim();
        if name == "origin" && !url.is_empty() {
            return Ok(url.to_string());
        }
    }
    Err(anyhow!(
        "`jj git remote list` did not list an `origin` remote in {}",
        repo_path.display()
    ))
}

/// Parse a slug from an origin remote URL. Returns `None` if the URL does not
/// have an extractable last path segment.
#[must_use]
pub fn parse_slug(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // SCP-style git URL: `git@host:org/repo.git` — split on `:` once and take
    // the right side as the path portion.
    let path_part = if url.contains("://") {
        // URL form: scheme://[user@]host[:port]/path...
        let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
        match after_scheme.split_once('/') {
            Some((_, path)) => path,
            None => return None,
        }
    } else if let Some((_, rhs)) = url.split_once(':') {
        rhs
    } else {
        url
    };

    // Strip query/fragment.
    let path_part = path_part
        .split(['?', '#'])
        .next()
        .unwrap_or(path_part)
        .trim_end_matches('/');

    let last = path_part.rsplit('/').next()?.trim();
    let slug = last.strip_suffix(".git").unwrap_or(last);
    if slug.is_empty() {
        return None;
    }
    Some(slug.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scp_style() {
        assert_eq!(
            parse_slug("git@github.com:AMD-PSP/firmware.git").as_deref(),
            Some("firmware")
        );
    }

    #[test]
    fn parse_https_with_dot_git() {
        assert_eq!(
            parse_slug("https://github.com/AMD-PSP/firmware.git").as_deref(),
            Some("firmware")
        );
    }

    #[test]
    fn parse_https_without_dot_git() {
        assert_eq!(
            parse_slug("https://github.com/AMD-PSP/firmware").as_deref(),
            Some("firmware")
        );
    }

    #[test]
    fn parse_ssh_url() {
        assert_eq!(
            parse_slug("ssh://git@github.com/AMD-PSP/firmware.git").as_deref(),
            Some("firmware")
        );
    }

    #[test]
    fn parse_trailing_slash() {
        assert_eq!(
            parse_slug("https://github.com/AMD-PSP/firmware/").as_deref(),
            Some("firmware")
        );
    }

    #[test]
    fn parse_local_path() {
        assert_eq!(
            parse_slug("/abs/path/to/firmware").as_deref(),
            Some("firmware")
        );
        assert_eq!(
            parse_slug("/abs/path/to/firmware.git").as_deref(),
            Some("firmware")
        );
    }

    #[test]
    fn parse_query_and_fragment_stripped() {
        assert_eq!(
            parse_slug("https://example.com/org/repo.git?ref=main").as_deref(),
            Some("repo")
        );
        assert_eq!(
            parse_slug("https://example.com/org/repo#frag").as_deref(),
            Some("repo")
        );
    }

    #[test]
    fn parse_empty_or_invalid() {
        assert!(parse_slug("").is_none());
        assert!(parse_slug("   ").is_none());
        assert!(parse_slug("https://example.com/").is_none());
        assert!(parse_slug("https://example.com").is_none());
    }
}
