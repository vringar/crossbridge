//! End-to-end round-trip test: real supervisor + 2 real servers + real
//! client binary, exercising submit and answer across two synthetic repos.
//!
//! See `.design/e2e-integration-test.md` for the full scenario.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{cargo_bin, wait_until, ChildGuard, RepoFixture, ShortTempDir};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

const ENV_VAR: &str = "CROSSBRIDGE_SOCKET_ROOT";

fn spawn_supervisor(runtime: &Path) -> ChildGuard {
    let child = Command::new(cargo_bin("crossbridge-supervisor"))
        .env(ENV_VAR, runtime)
        // Keep stderr noise out of the test transcript when things go wrong;
        // surface it on stderr so a failing test still has diagnostics.
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn crossbridge-supervisor binary");
    ChildGuard::new("supervisor", child)
}

fn spawn_server(runtime: &Path, group: &str, slug: &str, repo_path: &Path) -> ChildGuard {
    let child = Command::new(cargo_bin("crossbridge-server"))
        .args([
            "--group",
            group,
            "--slug",
            slug,
            "--repo-path",
            repo_path.to_str().expect("utf-8 repo path"),
        ])
        .env(ENV_VAR, runtime)
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn crossbridge-server binary");
    ChildGuard::new("server", child)
}

fn run_client(runtime: &Path, repo_path: &Path, args: &[&str]) -> std::process::Output {
    Command::new(cargo_bin("crossbridge-client"))
        .args(args)
        .current_dir(repo_path)
        .env(ENV_VAR, runtime)
        .output()
        .expect("spawn crossbridge-client binary")
}

// NOTE on `#[ignore]`:
//
// The full round-trip is currently blocked on a pre-existing bug in
// `crossbridge-client/src/main.rs:123`: the client labels the outbound
// issue with `xb-ref:<target_id_i64>` (the i64 returned in
// `ServerResponse::Ok`), but `crossbridge-server::handler::handle_answer`
// looks up the local issue via `xb-ref:<source_uuid>` (the UUID the
// answering side echoes back). The two never match, so the answer step
// of this test fails with `no local issue with label xb-ref:<uuid>`.
//
// Per the kickoff's explicit constraint ("FILE A SEPARATE ISSUE rather
// than fixing it in-band — this work is the env-var unification plus the
// test, nothing more"), the bug is tracked in crosslink issue #18 and not
// fixed here. The test is left intact (assertions are correct per the
// design spec) and will start passing as soon as #18 is resolved.
//
// Run with `cargo test -p crossbridge-e2e -- --ignored` to reproduce the
// failure once #18 is being worked on.
#[test]
#[ignore = "blocked on crosslink issue #18: client mislabels xb-ref with target i64 instead of source uuid"]
fn submit_then_answer_round_trip() {
    let tmp = ShortTempDir::new();
    let runtime = tmp.path().join("runtime");
    std::fs::create_dir_all(&runtime).expect("creating runtime dir");

    let repos_root = tmp.path().join("repos");
    let repo_a = RepoFixture::new(&repos_root, "repo-a");
    let repo_b = RepoFixture::new(&repos_root, "repo-b");

    // ---- Setup -------------------------------------------------------------
    let _supervisor = spawn_supervisor(&runtime);
    let register_socket = runtime.join("register.socket");
    wait_until(
        STARTUP_TIMEOUT,
        POLL_INTERVAL,
        "supervisor register socket",
        || register_socket.exists(),
    );

    let _server_a = spawn_server(&runtime, "test", "repo-a", &repo_a.root);
    let _server_b = spawn_server(&runtime, "test", "repo-b", &repo_b.root);

    // Both peer-listener sockets must appear before any client traffic.
    // <runtime>/<peer>/<own>.socket — so repo-a's server, when it learns of
    // repo-b, opens a listener at <runtime>/repo-b/repo-a.socket (clients on
    // repo-b connect there to reach repo-a). And vice versa.
    let listener_to_a = runtime.join("repo-b").join("repo-a.socket");
    let listener_to_b = runtime.join("repo-a").join("repo-b.socket");
    wait_until(
        STARTUP_TIMEOUT,
        POLL_INTERVAL,
        "both peer listener sockets",
        || listener_to_a.exists() && listener_to_b.exists(),
    );

    // ---- Submit ------------------------------------------------------------
    let local_a_id = repo_a
        .db()
        .create_issue("hello from a", Some("can you answer?"), "medium")
        .expect("creating issue in repo-a");
    let source_uuid = repo_a
        .db()
        .get_issue_uuid_by_id(local_a_id)
        .expect("reading repo-a issue UUID");

    let submit_out = run_client(
        &runtime,
        &repo_a.root,
        &[
            "submit",
            "--issue",
            &local_a_id.to_string(),
            "--target",
            "repo-b",
        ],
    );
    assert!(
        submit_out.status.success(),
        "submit exit={:?}\nstdout=\n{}\nstderr=\n{}",
        submit_out.status,
        String::from_utf8_lossy(&submit_out.stdout),
        String::from_utf8_lossy(&submit_out.stderr),
    );

    // repo-a's local view: outbound + pending + exactly one xb-ref:* label.
    let labels_a = repo_a
        .db()
        .get_labels(local_a_id)
        .expect("reading repo-a labels");
    assert!(
        labels_a.iter().any(|l| l == "xb:outbound"),
        "expected xb:outbound on repo-a issue, got: {labels_a:?}"
    );
    assert!(
        labels_a.iter().any(|l| l == "xb-status:pending"),
        "expected xb-status:pending on repo-a issue, got: {labels_a:?}"
    );
    let ref_labels: Vec<&String> = labels_a
        .iter()
        .filter(|l| l.starts_with("xb-ref:"))
        .collect();
    assert_eq!(
        ref_labels.len(),
        1,
        "expected exactly one xb-ref:* on repo-a issue, got: {labels_a:?}"
    );

    // repo-b: exactly one inbound issue, with xb-source:repo-a, xb-ref:<repo-a-uuid>,
    // and the appended footer in the body.
    let inbound = repo_b
        .db()
        .list_issues(None, Some("xb:inbound"), None)
        .expect("listing inbound issues in repo-b");
    assert_eq!(
        inbound.len(),
        1,
        "expected exactly one inbound issue in repo-b, got {}",
        inbound.len()
    );
    let inbound_id = inbound[0].id;
    let labels_b = repo_b
        .db()
        .get_labels(inbound_id)
        .expect("reading repo-b inbound labels");
    assert!(
        labels_b.iter().any(|l| l == "xb:inbound"),
        "expected xb:inbound, got: {labels_b:?}"
    );
    assert!(
        labels_b.iter().any(|l| l == "xb-source:repo-a"),
        "expected xb-source:repo-a, got: {labels_b:?}"
    );
    let expected_ref = format!("xb-ref:{source_uuid}");
    assert!(
        labels_b.contains(&expected_ref),
        "expected {expected_ref} on repo-b inbound, got: {labels_b:?}"
    );

    let inbound_issue = repo_b
        .db()
        .get_issue(inbound_id)
        .expect("loading repo-b inbound")
        .expect("inbound issue exists");
    let body = inbound_issue.description.unwrap_or_default();
    assert!(
        body.contains("can you answer?"),
        "expected original body in inbound issue, got: {body:?}"
    );
    assert!(
        body.contains("crossbridge-client answer --issue <id>"),
        "expected answer-instruction footer in inbound issue, got: {body:?}"
    );

    // ---- Answer ------------------------------------------------------------
    repo_b
        .db()
        .add_comment(inbound_id, "the answer is 42", "result")
        .expect("adding result comment");
    repo_b
        .db()
        .add_comment(inbound_id, "internal note (must not forward)", "note")
        .expect("adding note comment");

    let answer_out = run_client(
        &runtime,
        &repo_b.root,
        &["answer", "--issue", &inbound_id.to_string()],
    );
    assert!(
        answer_out.status.success(),
        "answer exit={:?}\nstdout=\n{}\nstderr=\n{}",
        answer_out.status,
        String::from_utf8_lossy(&answer_out.stdout),
        String::from_utf8_lossy(&answer_out.stderr),
    );

    // repo-b's inbound issue: closed and labeled xb-status:answered.
    let labels_b_after = repo_b
        .db()
        .get_labels(inbound_id)
        .expect("re-reading repo-b labels");
    assert!(
        labels_b_after.iter().any(|l| l == "xb-status:answered"),
        "expected xb-status:answered on repo-b inbound, got: {labels_b_after:?}"
    );
    let inbound_after = repo_b
        .db()
        .get_issue(inbound_id)
        .expect("re-loading repo-b inbound")
        .expect("repo-b inbound exists");
    assert_eq!(
        inbound_after.status.as_str(),
        "closed",
        "repo-b inbound issue should be closed"
    );

    // repo-a's outbound issue: closed, labeled xb-status:resolved, has the
    // result comment with [from repo-b] prefix, and no note-kind echo.
    let labels_a_after = repo_a
        .db()
        .get_labels(local_a_id)
        .expect("re-reading repo-a labels");
    assert!(
        labels_a_after.iter().any(|l| l == "xb-status:resolved"),
        "expected xb-status:resolved on repo-a issue, got: {labels_a_after:?}"
    );
    assert!(
        !labels_a_after.iter().any(|l| l == "xb-status:pending"),
        "expected xb-status:pending to be removed, got: {labels_a_after:?}"
    );
    let outbound_after = repo_a
        .db()
        .get_issue(local_a_id)
        .expect("re-loading repo-a issue")
        .expect("repo-a issue exists");
    assert_eq!(
        outbound_after.status.as_str(),
        "closed",
        "repo-a outbound issue should be closed"
    );
    let comments_a = repo_a
        .db()
        .get_comments(local_a_id)
        .expect("reading repo-a comments");
    let result_comment_count = comments_a
        .iter()
        .filter(|c| c.content == "[from repo-b] the answer is 42")
        .count();
    assert_eq!(
        result_comment_count, 1,
        "expected exactly one '[from repo-b] the answer is 42' comment, got: {comments_a:?}"
    );
    assert!(
        !comments_a
            .iter()
            .any(|c| c.content.contains("internal note (must not forward)")),
        "note-kind comment should not have been forwarded, got: {comments_a:?}"
    );

    // ---- Idempotency probe -------------------------------------------------
    // Re-run the same answer. Per the design, the server's de-duplication
    // contract makes the repeated answer either a no-op or an error. Either
    // way, the source repo must not gain a duplicate comment and the issue
    // must remain closed.
    let answer_again = run_client(
        &runtime,
        &repo_b.root,
        &["answer", "--issue", &inbound_id.to_string()],
    );
    let _ = answer_again; // exit code intentionally not asserted

    let comments_a2 = repo_a
        .db()
        .get_comments(local_a_id)
        .expect("re-reading repo-a comments");
    let result_count_after = comments_a2
        .iter()
        .filter(|c| c.content == "[from repo-b] the answer is 42")
        .count();
    assert_eq!(
        result_count_after, 1,
        "duplicate answer must not add a duplicate result comment, got: {comments_a2:?}"
    );
    let outbound_after2 = repo_a
        .db()
        .get_issue(local_a_id)
        .expect("re-loading repo-a issue")
        .expect("repo-a issue exists");
    assert_eq!(
        outbound_after2.status.as_str(),
        "closed",
        "repo-a outbound issue must remain closed after duplicate answer"
    );
}
