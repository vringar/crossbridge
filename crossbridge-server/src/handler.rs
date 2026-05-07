//! `ClientRequest` handlers: turn `SubmitIssue` / `SubmitAnswer` frames into
//! local crosslink DB writes and frame back a `ServerResponse`.
//!
//! The handler is structured so the request-processing logic can be tested
//! against a real `crosslink::db::Database` in a tempdir, independent of the
//! Unix socket / event loop machinery in `main.rs`.

use std::path::Path;

use anyhow::{anyhow, Result};
use crossbridge_protocol::{
    read_message, write_message, ClientRequest, ServerResponse, SubmitAnswer, SubmitIssue,
};
use crosslink::db::Database;
use tokio::net::UnixStream;

use crate::attachment;

/// Footer appended to every inbound issue body so the answering agent knows
/// how to ship the result back. The literal `<id>` is filled in by the agent;
/// we deliberately leave it as a placeholder string in the body.
pub const ANSWER_INSTRUCTION_FOOTER: &str =
    "\n\n---\nAfter answering, run: `crossbridge-client answer --issue <id>`";

/// Read one framed `ClientRequest` from `stream`, dispatch it, and write back
/// the framed `ServerResponse`. Errors are *converted* to `ServerResponse::Error`
/// — a single bad request must never crash the server.
pub async fn handle_connection(
    stream: &mut UnixStream,
    peer_slug: &str,
    db: &Database,
    repo_path: &Path,
) -> Result<()> {
    let request: ClientRequest = match read_message(stream).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(peer = peer_slug, "malformed request frame: {e}");
            let _ = write_message(
                stream,
                &ServerResponse::Error {
                    message: format!("malformed request: {e}"),
                },
            )
            .await;
            return Ok(());
        }
    };

    let response = match request {
        ClientRequest::Submit(submit) => match handle_submit(db, peer_slug, &submit, repo_path) {
            Ok(issue_id) => ServerResponse::Ok { issue_id },
            Err(e) => {
                tracing::warn!(peer = peer_slug, "submit failed: {e:#}");
                ServerResponse::Error {
                    message: format!("{e:#}"),
                }
            }
        },
        ClientRequest::Answer(answer) => match handle_answer(db, peer_slug, &answer) {
            Ok(issue_id) => ServerResponse::Ok { issue_id },
            Err(e) => {
                tracing::warn!(peer = peer_slug, "answer failed: {e:#}");
                ServerResponse::Error {
                    message: format!("{e:#}"),
                }
            }
        },
    };

    write_message(stream, &response)
        .await
        .map_err(|e| anyhow!("writing response frame: {}", e))?;
    Ok(())
}

/// Materialize a `SubmitIssue` in the local crosslink DB.
///
/// Idempotent: if an issue already carries the `xb-ref:<source_uuid>` label,
/// its id is returned without creating a duplicate row or re-applying side
/// effects (no duplicate attachments, no duplicate footer).
pub fn handle_submit(
    db: &Database,
    peer_slug: &str,
    submit: &SubmitIssue,
    repo_path: &Path,
) -> Result<i64> {
    let ref_label = format!("xb-ref:{}", submit.source_uuid);

    if let Some(existing) = find_issue_with_label(db, &ref_label)? {
        tracing::debug!(
            peer = peer_slug,
            issue_id = existing,
            source_uuid = %submit.source_uuid,
            "duplicate submit (idempotency hit), returning existing issue"
        );
        return Ok(existing);
    }

    let body_with_footer = format!("{}{}", submit.body, ANSWER_INSTRUCTION_FOOTER);
    let issue_id = db
        .create_issue(&submit.title, Some(body_with_footer.as_str()), "high")
        .map_err(|e| anyhow!("creating local issue: {}", e))?;

    let mut labels = vec![
        "type:request".to_string(),
        "xb:inbound".to_string(),
        "xb-status:open".to_string(),
        format!("xb-source:{}", submit.source_slug),
        ref_label,
    ];
    // Include any extra labels the client requested, deduped against ours.
    for l in &submit.labels {
        if !labels.iter().any(|existing| existing == l) {
            labels.push(l.clone());
        }
    }
    db.transaction(|| {
        for l in &labels {
            db.add_label(issue_id, l)?;
        }
        Ok(())
    })
    .map_err(|e| anyhow!("applying labels: {}", e))?;

    if !submit.attachments.is_empty() {
        match attachment::materialize(repo_path, &submit.attachments) {
            Ok(record) => {
                let comment = attachment::format_comment(&record);
                if let Err(e) = db.add_comment(issue_id, &comment, "note") {
                    tracing::warn!(issue_id, "adding attachment comment failed: {e}");
                }
            }
            Err(e) => {
                tracing::warn!(issue_id, "attachment materialization failed: {e:#}");
                let comment = format!("crossbridge: failed to materialize attachments: {e:#}");
                let _ = db.add_comment(issue_id, &comment, "note");
            }
        }
    }

    tracing::info!(
        peer = peer_slug,
        issue_id,
        source_uuid = %submit.source_uuid,
        "created inbound issue"
    );
    Ok(issue_id)
}

/// Apply a `SubmitAnswer` to the local outbound issue identified by
/// `xb-ref:<source_uuid>`.
///
/// - Copies `kind == "result"` comments back, deduped by content; each one is
///   prefixed with `[from <peer_slug>]`.
/// - Swaps `xb-status:pending` → `xb-status:resolved` (no-op if the labels are
///   already in their resolved state).
/// - Closes the local issue.
///
/// Returns `Err` if no local issue carries `xb-ref:<source_uuid>` — the spec
/// requires this to surface as `ServerResponse::Error` to the caller.
pub fn handle_answer(db: &Database, peer_slug: &str, answer: &SubmitAnswer) -> Result<i64> {
    let ref_label = format!("xb-ref:{}", answer.source_uuid);
    let issue_id = find_issue_with_label(db, &ref_label)?
        .ok_or_else(|| anyhow!("no local issue with label {ref_label}"))?;

    let existing_comments = db
        .get_comments(issue_id)
        .map_err(|e| anyhow!("reading comments: {}", e))?;
    let existing_contents: std::collections::HashSet<&str> = existing_comments
        .iter()
        .map(|c| c.content.as_str())
        .collect();

    let to_copy: Vec<String> = answer
        .comments
        .iter()
        .filter(|c| c.kind == "result")
        .map(|c| format!("[from {peer_slug}] {}", c.content))
        .filter(|c| !existing_contents.contains(c.as_str()))
        .collect();

    db.transaction(|| {
        for c in &to_copy {
            db.add_comment(issue_id, c, "result")?;
        }

        let labels = db.get_labels(issue_id)?;
        if labels.iter().any(|l| l == "xb-status:pending") {
            db.remove_label(issue_id, "xb-status:pending")?;
        }
        if !labels.iter().any(|l| l == "xb-status:resolved") {
            db.add_label(issue_id, "xb-status:resolved")?;
        }
        Ok(())
    })
    .map_err(|e| anyhow!("applying answer: {}", e))?;

    let issue = db
        .get_issue(issue_id)
        .map_err(|e| anyhow!("loading issue {issue_id}: {}", e))?
        .ok_or_else(|| anyhow!("issue {issue_id} disappeared"))?;
    if issue.status.as_str() != "closed" {
        db.close_issue(issue_id)
            .map_err(|e| anyhow!("closing issue {issue_id}: {}", e))?;
    }

    tracing::info!(
        peer = peer_slug,
        issue_id,
        source_uuid = %answer.source_uuid,
        comments_copied = to_copy.len(),
        "applied answer"
    );
    Ok(issue_id)
}

/// Find any open issue carrying `label`. Used for idempotency checks.
fn find_issue_with_label(db: &Database, label: &str) -> Result<Option<i64>> {
    // Search both open and closed issues — an inbound issue might already be
    // closed if the answer arrived first or the issue was manually resolved.
    for status in ["open", "closed"] {
        let issues = db
            .list_issues(Some(status), Some(label), None)
            .map_err(|e| anyhow!("listing issues by label {label}: {}", e))?;
        if let Some(i) = issues.into_iter().next() {
            return Ok(Some(i.id));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbridge_protocol::AnswerComment;
    use tempfile::tempdir;

    fn open_db(dir: &Path) -> Database {
        let crosslink_dir = dir.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        Database::open(&crosslink_dir.join("issues.db")).unwrap()
    }

    fn submit() -> SubmitIssue {
        SubmitIssue {
            title: "Need answer X".to_string(),
            body: "Question body here.".to_string(),
            labels: vec!["custom:tag".to_string()],
            source_slug: "repo-a".to_string(),
            source_uuid: "uuid-1".to_string(),
            attachments: vec![],
        }
    }

    #[test]
    fn submit_creates_issue_with_label_set() {
        let dir = tempdir().unwrap();
        let db = open_db(dir.path());
        let id = handle_submit(&db, "repo-a", &submit(), dir.path()).unwrap();
        assert!(id > 0, "issue id should be assigned");

        let labels = db.get_labels(id).unwrap();
        for required in [
            "type:request",
            "xb:inbound",
            "xb-status:open",
            "xb-source:repo-a",
            "xb-ref:uuid-1",
            "custom:tag",
        ] {
            assert!(
                labels.iter().any(|l| l == required),
                "missing label {required}: {labels:?}"
            );
        }

        let issue = db.get_issue(id).unwrap().unwrap();
        let body = issue.description.unwrap_or_default();
        assert!(body.contains("Question body here."));
        assert!(body.contains("crossbridge-client answer --issue <id>"));
    }

    #[test]
    fn submit_is_idempotent_on_duplicate_source_uuid() {
        let dir = tempdir().unwrap();
        let db = open_db(dir.path());
        let id1 = handle_submit(&db, "repo-a", &submit(), dir.path()).unwrap();
        let id2 = handle_submit(&db, "repo-a", &submit(), dir.path()).unwrap();
        assert_eq!(id1, id2);
        // Only one row exists.
        let issues = db
            .list_issues(Some("open"), Some("xb:inbound"), None)
            .unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn answer_unknown_source_uuid_is_error() {
        let dir = tempdir().unwrap();
        let db = open_db(dir.path());
        let answer = SubmitAnswer {
            source_uuid: "no-such-uuid".to_string(),
            comments: vec![],
            attachments: vec![],
        };
        let err = handle_answer(&db, "repo-b", &answer).unwrap_err();
        assert!(format!("{err:#}").contains("no local issue"));
    }

    #[test]
    fn answer_copies_result_comments_and_swaps_status() {
        let dir = tempdir().unwrap();
        let db = open_db(dir.path());

        // Simulate an outbound issue: source repo's local view labeled with
        // xb-ref:<target-uuid> so the SubmitAnswer can find it.
        let outbound_id = db.create_issue("My question", None, "high").unwrap();
        db.add_label(outbound_id, "type:request").unwrap();
        db.add_label(outbound_id, "xb:outbound").unwrap();
        db.add_label(outbound_id, "xb-status:pending").unwrap();
        db.add_label(outbound_id, "xb-ref:target-uuid").unwrap();

        let answer = SubmitAnswer {
            source_uuid: "target-uuid".to_string(),
            comments: vec![
                AnswerComment {
                    content: "the answer is 42".to_string(),
                    kind: "result".to_string(),
                },
                AnswerComment {
                    content: "stray internal note, should not copy".to_string(),
                    kind: "note".to_string(),
                },
            ],
            attachments: vec![],
        };

        let id = handle_answer(&db, "repo-b", &answer).unwrap();
        assert_eq!(id, outbound_id);

        let labels = db.get_labels(outbound_id).unwrap();
        assert!(labels.iter().any(|l| l == "xb-status:resolved"));
        assert!(!labels.iter().any(|l| l == "xb-status:pending"));

        let issue = db.get_issue(outbound_id).unwrap().unwrap();
        assert_eq!(issue.status.as_str(), "closed");

        let comments = db.get_comments(outbound_id).unwrap();
        assert_eq!(comments.len(), 1, "only the result comment is copied");
        assert!(comments[0].content.starts_with("[from repo-b]"));
        assert!(comments[0].content.contains("the answer is 42"));
    }

    #[test]
    fn answer_is_idempotent() {
        let dir = tempdir().unwrap();
        let db = open_db(dir.path());

        let outbound_id = db.create_issue("Q", None, "high").unwrap();
        db.add_label(outbound_id, "xb:outbound").unwrap();
        db.add_label(outbound_id, "xb-status:pending").unwrap();
        db.add_label(outbound_id, "xb-ref:target-uuid").unwrap();

        let answer = SubmitAnswer {
            source_uuid: "target-uuid".to_string(),
            comments: vec![AnswerComment {
                content: "duplicate answer".to_string(),
                kind: "result".to_string(),
            }],
            attachments: vec![],
        };

        handle_answer(&db, "repo-b", &answer).unwrap();
        handle_answer(&db, "repo-b", &answer).unwrap();
        let comments = db.get_comments(outbound_id).unwrap();
        assert_eq!(
            comments.len(),
            1,
            "duplicate answer must not duplicate comments"
        );
    }
}
