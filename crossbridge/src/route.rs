use anyhow::{Context, Result};
use crosslink::db::Database;
use crosslink::models::Issue;

use crate::config::Config;

/// Run one full bridge cycle: route outbound requests, then collect answers.
pub fn run_cycle(config: &Config) -> Result<()> {
    for (slug, repo) in &config.repos {
        let db_path = repo.db_path();
        let db = match Database::open(&db_path) {
            Ok(db) => db,
            Err(e) => {
                tracing::warn!(repo = slug, path = %db_path.display(), "failed to open db: {e}");
                continue;
            }
        };

        if let Err(e) = route_outbound(&db, slug, config) {
            tracing::error!(repo = slug, "outbound routing failed: {e:#}");
        }

        if let Err(e) = collect_answered(&db, slug, config) {
            tracing::error!(repo = slug, "answer collection failed: {e:#}");
        }
    }
    Ok(())
}

/// Phase 1: find outbound requests in this repo and deliver them to target repos.
fn route_outbound(db: &Database, source_slug: &str, config: &Config) -> Result<()> {
    let candidates = db.list_issues(Some("open"), Some("xb:outbound"), None)?;
    if candidates.is_empty() {
        return Ok(());
    }

    let ids: Vec<i64> = candidates.iter().map(|i| i.id).collect();
    let labels_map = db.get_labels_batch(&ids)?;

    for issue in &candidates {
        let labels = match labels_map.get(&issue.id) {
            Some(l) => l,
            None => continue,
        };

        // Post-filter: must have xb-status:open
        if !labels.iter().any(|l| l == "xb-status:open") {
            continue;
        }

        if let Err(e) = route_single_outbound(db, source_slug, config, issue, labels) {
            tracing::error!(
                repo = source_slug,
                issue = issue.id,
                "failed to route outbound: {e:#}"
            );
        }
    }
    Ok(())
}

/// Route a single outbound issue to its target repo.
fn route_single_outbound(
    db: &Database,
    source_slug: &str,
    config: &Config,
    issue: &Issue,
    labels: &[String],
) -> Result<()> {
    let target_slug = labels
        .iter()
        .find_map(|l| l.strip_prefix("xb-target:"))
        .ok_or_else(|| anyhow::anyhow!("outbound issue {} missing xb-target label", issue.id))?;

    let target_repo = match config.repos.get(target_slug) {
        Some(r) => r,
        None => {
            let available = config.repo_slugs().join(", ");
            db.add_comment(
                issue.id,
                &format!("crossbridge: unknown target '{target_slug}'. Available: {available}"),
                "note",
            )?;
            // Remove xb:outbound to prevent scan waste from error-state issues
            db.transaction(|| {
                db.remove_label(issue.id, "xb-status:open")?;
                db.remove_label(issue.id, "xb:outbound")?;
                db.add_label(issue.id, "xb-status:error")?;
                Ok(())
            })?;
            return Ok(());
        }
    };

    let source_uuid = db.get_issue_uuid_by_id(issue.id)?;

    // Open target database
    let target_db = Database::open(&target_repo.db_path())
        .with_context(|| format!("opening target DB for '{target_slug}'"))?;

    // Idempotency check: does the target already have an issue with xb-ref:<source-uuid>?
    let ref_label = format!("xb-ref:{source_uuid}");
    if find_issue_with_label(&target_db, &ref_label)?.is_some() {
        tracing::debug!(
            source = source_slug,
            target = target_slug,
            issue = issue.id,
            "already delivered, ensuring source labels are correct"
        );
        // Ensure source is marked pending (may have crashed after target creation)
        if labels.iter().any(|l| l == "xb-status:open") {
            swap_status_label(db, issue.id, "xb-status:open", "xb-status:pending")?;
        }
        return Ok(());
    }

    // Create issue in target repo
    let target_id = target_db.create_issue(&issue.title, issue.description.as_deref(), "high")?;
    let target_uuid = target_db.get_issue_uuid_by_id(target_id)?;

    // Label the target issue
    for label in [
        "type:request",
        "xb:inbound",
        "xb-status:open",
        &format!("xb-source:{source_slug}"),
        &ref_label,
    ] {
        target_db.add_label(target_id, label)?;
    }

    // Update source issue atomically
    let target_ref_label = format!("xb-ref:{target_uuid}");
    db.transaction(|| {
        db.remove_label(issue.id, "xb-status:open")?;
        db.add_label(issue.id, "xb-status:pending")?;
        db.add_label(issue.id, &target_ref_label)?;
        Ok(())
    })?;

    tracing::info!(
        source = source_slug,
        target = target_slug,
        source_issue = issue.id,
        target_issue = target_id,
        "routed request"
    );
    Ok(())
}

/// Phase 2: find answered inbound requests in this repo and copy results back to source.
fn collect_answered(db: &Database, this_slug: &str, config: &Config) -> Result<()> {
    let candidates = db.list_issues(Some("open"), Some("xb:inbound"), None)?;
    if candidates.is_empty() {
        return Ok(());
    }

    let ids: Vec<i64> = candidates.iter().map(|i| i.id).collect();
    let labels_map = db.get_labels_batch(&ids)?;

    for issue in &candidates {
        let labels = match labels_map.get(&issue.id) {
            Some(l) => l,
            None => continue,
        };

        // Post-filter: must have xb-status:answered
        if !labels.iter().any(|l| l == "xb-status:answered") {
            continue;
        }

        if let Err(e) = collect_single_answer(db, this_slug, config, issue, labels) {
            tracing::error!(
                repo = this_slug,
                issue = issue.id,
                "failed to collect answer: {e:#}"
            );
        }
    }
    Ok(())
}

/// Collect a single answered inbound issue: copy results back to source and close both.
fn collect_single_answer(
    db: &Database,
    this_slug: &str,
    config: &Config,
    issue: &Issue,
    labels: &[String],
) -> Result<()> {
    let source_slug = labels
        .iter()
        .find_map(|l| l.strip_prefix("xb-source:"))
        .ok_or_else(|| anyhow::anyhow!("inbound issue {} missing xb-source label", issue.id))?;

    let source_ref = labels
        .iter()
        .find_map(|l| l.strip_prefix("xb-ref:"))
        .ok_or_else(|| anyhow::anyhow!("inbound issue {} missing xb-ref label", issue.id))?;

    let source_repo = match config.repos.get(source_slug) {
        Some(r) => r,
        None => {
            tracing::warn!(
                issue = issue.id,
                source = source_slug,
                "source repo not in config, skipping"
            );
            return Ok(());
        }
    };

    let source_db = Database::open(&source_repo.db_path())
        .with_context(|| format!("opening source DB for '{source_slug}'"))?;

    let source_issue_id = match source_db.get_issue_id_by_uuid(source_ref) {
        Ok(id) => id,
        Err(_) => {
            tracing::warn!(
                issue = issue.id,
                source_uuid = source_ref,
                "source issue not found (deleted?), closing orphaned target"
            );
            db.close_issue(issue.id)?;
            return Ok(());
        }
    };

    // Copy result comments with deduplication
    let target_comments = db.get_comments(issue.id)?;
    let source_comments = source_db.get_comments(source_issue_id)?;
    let existing_contents: std::collections::HashSet<&str> =
        source_comments.iter().map(|c| c.content.as_str()).collect();

    let result_comments: Vec<_> = target_comments
        .iter()
        .filter(|c| c.kind == "result")
        .collect();

    if result_comments.is_empty() {
        source_db.add_comment(
            source_issue_id,
            &format!(
                "[from {this_slug}] crossbridge: target agent marked answered \
                 but provided no result comments"
            ),
            "result",
        )?;
    } else {
        for c in &result_comments {
            let prefixed = format!("[from {this_slug}] {}", c.content);
            if !existing_contents.contains(prefixed.as_str()) {
                source_db.add_comment(source_issue_id, &prefixed, "result")?;
            }
        }
    }

    // Close lifecycle
    swap_status_label(
        &source_db,
        source_issue_id,
        "xb-status:pending",
        "xb-status:resolved",
    )?;
    source_db.close_issue(source_issue_id)?;
    db.close_issue(issue.id)?;

    tracing::info!(
        target_repo = this_slug,
        source_repo = source_slug,
        target_issue = issue.id,
        source_issue = source_issue_id,
        "collected answer and closed both issues"
    );
    Ok(())
}

/// Atomically swap one xb-status label for another.
fn swap_status_label(db: &Database, issue_id: i64, old: &str, new: &str) -> Result<()> {
    db.transaction(|| {
        db.remove_label(issue_id, old)?;
        db.add_label(issue_id, new)?;
        Ok(())
    })
}

/// Find an open issue that has a specific label. Used for idempotency checks.
fn find_issue_with_label(db: &Database, label: &str) -> Result<Option<Issue>> {
    let issues = db.list_issues(Some("open"), Some(label), None)?;
    Ok(issues.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoConfig;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn setup_test_db(dir: &std::path::Path) -> Database {
        let crosslink_dir = dir.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db_path = crosslink_dir.join("issues.db");
        Database::open(&db_path).unwrap()
    }

    fn make_config(repos: Vec<(&str, PathBuf)>) -> Config {
        Config {
            repos: repos
                .into_iter()
                .map(|(slug, path)| (slug.to_string(), RepoConfig { path }))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn test_route_outbound_creates_target_issue() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        let source_db = setup_test_db(source_dir.path());
        let target_db = setup_test_db(target_dir.path());

        // Create an outbound request in source
        let issue_id = source_db
            .create_issue("What is the PSP entry point?", None, "high")
            .unwrap();
        source_db.add_label(issue_id, "type:request").unwrap();
        source_db.add_label(issue_id, "xb:outbound").unwrap();
        source_db.add_label(issue_id, "xb-status:open").unwrap();
        source_db.add_label(issue_id, "xb-target:target").unwrap();

        let config = make_config(vec![
            ("source", source_dir.path().to_path_buf()),
            ("target", target_dir.path().to_path_buf()),
        ]);

        // Run routing
        route_outbound(&source_db, "source", &config).unwrap();

        // Verify source issue was updated
        let source_labels = source_db.get_labels(issue_id).unwrap();
        assert!(source_labels.contains(&"xb-status:pending".to_string()));
        assert!(!source_labels.contains(&"xb-status:open".to_string()));
        assert!(source_labels.iter().any(|l| l.starts_with("xb-ref:")));

        // Verify target issue was created
        let target_issues = target_db
            .list_issues(Some("open"), Some("xb:inbound"), None)
            .unwrap();
        assert_eq!(target_issues.len(), 1);
        assert_eq!(target_issues[0].title, "What is the PSP entry point?");

        let target_labels = target_db.get_labels(target_issues[0].id).unwrap();
        assert!(target_labels.contains(&"type:request".to_string()));
        assert!(target_labels.contains(&"xb:inbound".to_string()));
        assert!(target_labels.contains(&"xb-status:open".to_string()));
        assert!(target_labels.contains(&"xb-source:source".to_string()));
        assert!(target_labels.iter().any(|l| l.starts_with("xb-ref:")));

        // Verify idempotency: running again should not create a second target issue
        // Reset source to open to simulate the scenario (in practice this wouldn't happen)
        // Instead, just run again and confirm no duplicate
        route_outbound(&source_db, "source", &config).unwrap();
        let target_issues_2 = target_db
            .list_issues(Some("open"), Some("xb:inbound"), None)
            .unwrap();
        assert_eq!(target_issues_2.len(), 1, "should not create duplicate");

        drop(target_db);
        drop(source_db);
    }

    #[test]
    fn test_route_outbound_unknown_target() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_db = setup_test_db(source_dir.path());

        let issue_id = source_db
            .create_issue("Question for nonexistent repo", None, "high")
            .unwrap();
        source_db.add_label(issue_id, "type:request").unwrap();
        source_db.add_label(issue_id, "xb:outbound").unwrap();
        source_db.add_label(issue_id, "xb-status:open").unwrap();
        source_db
            .add_label(issue_id, "xb-target:nonexistent")
            .unwrap();

        let config = make_config(vec![("source", source_dir.path().to_path_buf())]);

        route_outbound(&source_db, "source", &config).unwrap();

        let labels = source_db.get_labels(issue_id).unwrap();
        assert!(labels.contains(&"xb-status:error".to_string()));
        assert!(!labels.contains(&"xb-status:open".to_string()));
        assert!(!labels.contains(&"xb:outbound".to_string()));

        // Verify error comment was added
        let comments = source_db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].content.contains("unknown target"));
        assert!(comments[0].content.contains("source"));

        drop(source_db);
    }

    #[test]
    fn test_collect_answered() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        let source_db = setup_test_db(source_dir.path());
        let target_db = setup_test_db(target_dir.path());

        // Simulate state after phase 1: source has a pending issue, target has an inbound issue
        let source_id = source_db.create_issue("What is X?", None, "high").unwrap();
        let source_uuid = source_db.get_issue_uuid_by_id(source_id).unwrap();
        source_db.add_label(source_id, "type:request").unwrap();
        source_db.add_label(source_id, "xb:outbound").unwrap();
        source_db.add_label(source_id, "xb-status:pending").unwrap();

        let target_id = target_db.create_issue("What is X?", None, "high").unwrap();
        let target_uuid = target_db.get_issue_uuid_by_id(target_id).unwrap();
        target_db.add_label(target_id, "type:request").unwrap();
        target_db.add_label(target_id, "xb:inbound").unwrap();
        target_db
            .add_label(target_id, "xb-status:answered")
            .unwrap();
        target_db.add_label(target_id, "xb-source:source").unwrap();
        target_db
            .add_label(target_id, &format!("xb-ref:{source_uuid}"))
            .unwrap();

        // Cross-reference on source side
        source_db
            .add_label(source_id, &format!("xb-ref:{target_uuid}"))
            .unwrap();

        // Add a result comment on the target
        target_db
            .add_comment(target_id, "X is the PSP reset vector at 0x100", "result")
            .unwrap();

        let config = make_config(vec![
            ("source", source_dir.path().to_path_buf()),
            ("target", target_dir.path().to_path_buf()),
        ]);

        // Run collection from target repo's perspective
        collect_answered(&target_db, "target", &config).unwrap();

        // Verify source issue got the answer and was resolved
        let source_labels = source_db.get_labels(source_id).unwrap();
        assert!(source_labels.contains(&"xb-status:resolved".to_string()));
        assert!(!source_labels.contains(&"xb-status:pending".to_string()));

        let source_issue = source_db.get_issue(source_id).unwrap().unwrap();
        assert_eq!(source_issue.status.as_str(), "closed");

        let source_comments = source_db.get_comments(source_id).unwrap();
        assert_eq!(source_comments.len(), 1);
        assert!(source_comments[0]
            .content
            .contains("X is the PSP reset vector"));
        assert!(source_comments[0].content.starts_with("[from target]"));

        // Verify target issue was closed
        let target_issue = target_db.get_issue(target_id).unwrap().unwrap();
        assert_eq!(target_issue.status.as_str(), "closed");

        drop(target_db);
        drop(source_db);
    }

    #[test]
    fn test_full_cycle() {
        let repo_a_dir = tempfile::tempdir().unwrap();
        let repo_b_dir = tempfile::tempdir().unwrap();

        let db_a = setup_test_db(repo_a_dir.path());
        let db_b = setup_test_db(repo_b_dir.path());

        // Agent in repo A creates a request for repo B
        let req_id = db_a
            .create_issue("What compiler flags does repo B use?", None, "high")
            .unwrap();
        db_a.add_label(req_id, "type:request").unwrap();
        db_a.add_label(req_id, "xb:outbound").unwrap();
        db_a.add_label(req_id, "xb-status:open").unwrap();
        db_a.add_label(req_id, "xb-target:repo-b").unwrap();

        let config = make_config(vec![
            ("repo-a", repo_a_dir.path().to_path_buf()),
            ("repo-b", repo_b_dir.path().to_path_buf()),
        ]);

        // Cycle 1: bridge routes the request
        run_cycle(&config).unwrap();

        // Verify: source is pending, target has inbound issue
        let a_labels = db_a.get_labels(req_id).unwrap();
        assert!(a_labels.contains(&"xb-status:pending".to_string()));

        let b_issues = db_b
            .list_issues(Some("open"), Some("xb:inbound"), None)
            .unwrap();
        assert_eq!(b_issues.len(), 1);

        // Simulate: agent in repo B answers
        let b_issue_id = b_issues[0].id;
        db_b.add_comment(b_issue_id, "-O2 -march=znver3 -flto", "result")
            .unwrap();
        db_b.remove_label(b_issue_id, "xb-status:open").unwrap();
        db_b.add_label(b_issue_id, "xb-status:answered").unwrap();

        // Cycle 2: bridge collects the answer
        run_cycle(&config).unwrap();

        // Verify: both issues closed, source has the answer
        let a_issue = db_a.get_issue(req_id).unwrap().unwrap();
        assert_eq!(a_issue.status.as_str(), "closed");

        let a_comments = db_a.get_comments(req_id).unwrap();
        assert!(a_comments.iter().any(|c| c.content.contains("-O2")));

        let b_issue = db_b.get_issue(b_issue_id).unwrap().unwrap();
        assert_eq!(b_issue.status.as_str(), "closed");

        drop(db_a);
        drop(db_b);
    }

    #[test]
    fn test_idempotent_after_crash_between_dbs() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        let source_db = setup_test_db(source_dir.path());
        let _target_db = setup_test_db(target_dir.path());

        // Create outbound request
        let issue_id = source_db
            .create_issue("Crash test question", None, "high")
            .unwrap();
        source_db.add_label(issue_id, "type:request").unwrap();
        source_db.add_label(issue_id, "xb:outbound").unwrap();
        source_db.add_label(issue_id, "xb-status:open").unwrap();
        source_db.add_label(issue_id, "xb-target:target").unwrap();

        let config = make_config(vec![
            ("source", source_dir.path().to_path_buf()),
            ("target", target_dir.path().to_path_buf()),
        ]);

        // First run: creates target issue and updates source
        route_outbound(&source_db, "source", &config).unwrap();

        // Simulate crash: revert source to xb-status:open (as if the source update never happened)
        source_db
            .remove_label(issue_id, "xb-status:pending")
            .unwrap();
        source_db.add_label(issue_id, "xb-status:open").unwrap();
        // Remove xb-ref too, simulating the transaction rollback
        let labels = source_db.get_labels(issue_id).unwrap();
        for l in &labels {
            if l.starts_with("xb-ref:") {
                source_db.remove_label(issue_id, l).unwrap();
            }
        }

        // Re-open target DB to count issues before second run
        let target_db = Database::open(&config.repos["target"].db_path()).unwrap();
        let before = target_db
            .list_issues(Some("open"), Some("xb:inbound"), None)
            .unwrap();
        assert_eq!(before.len(), 1);

        // Second run: should detect existing target issue and not create a duplicate
        drop(target_db);
        route_outbound(&source_db, "source", &config).unwrap();

        let target_db = Database::open(&config.repos["target"].db_path()).unwrap();
        let after = target_db
            .list_issues(Some("open"), Some("xb:inbound"), None)
            .unwrap();
        assert_eq!(after.len(), 1, "idempotency check should prevent duplicate");

        // Source should now be pending
        let source_labels = source_db.get_labels(issue_id).unwrap();
        assert!(source_labels.contains(&"xb-status:pending".to_string()));

        drop(target_db);
        drop(source_db);
    }
}
