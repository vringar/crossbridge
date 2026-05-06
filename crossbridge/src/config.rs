use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub repos: BTreeMap<String, RepoConfig>,
}

#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    pub path: PathBuf,
}

impl RepoConfig {
    pub fn db_path(&self) -> PathBuf {
        self.path.join(".crosslink").join("issues.db")
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;

        if config.repos.is_empty() {
            anyhow::bail!("config has no repos defined");
        }

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for (slug, repo) in &self.repos {
            if !repo.path.exists() {
                tracing::warn!(repo = slug, path = %repo.path.display(), "repo path does not exist, skipping");
            } else if !repo.db_path().exists() {
                tracing::warn!(repo = slug, path = %repo.db_path().display(), "crosslink DB not found, skipping");
            }
        }
        Ok(())
    }

    pub fn repo_slugs(&self) -> Vec<&str> {
        self.repos.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        write!(
            f,
            r#"
[repos.alpha]
path = "/tmp/alpha"

[repos.beta]
path = "/tmp/beta"
"#
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.repos.len(), 2);
        assert!(config.repos.contains_key("alpha"));
        assert!(config.repos.contains_key("beta"));
        assert_eq!(
            config.repos["alpha"].db_path(),
            PathBuf::from("/tmp/alpha/.crosslink/issues.db")
        );
    }

    #[test]
    fn empty_repos_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[repos]\n").unwrap();

        let err = Config::load(&config_path).unwrap_err();
        assert!(err.to_string().contains("no repos defined"));
    }

    #[test]
    fn repo_slugs_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        write!(
            f,
            r#"
[repos.zebra]
path = "/tmp/z"

[repos.alpha]
path = "/tmp/a"
"#
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.repo_slugs(), vec!["alpha", "zebra"]);
    }
}
