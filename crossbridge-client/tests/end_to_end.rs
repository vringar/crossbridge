//! Integration tests covering the full CLI surface.
//!
//! Each test stands up:
//!   - a temp git repository (origin = `git@example.com:org/<slug>.git`)
//!   - a `.crosslink/issues.db` populated via the `crosslink` library
//!   - a `CROSSBRIDGE_SOCKET_ROOT` pointing at a temp socket tree
//!   - a thread-bound mock `crossbridge` server speaking the wire protocol
//!
//! Then it invokes the actual built binary via `Command::new(env!("CARGO_BIN_EXE_*"))`
//! so we exercise CLI parsing, repo-root discovery, socket I/O, and DB
//! mutation end-to-end.

use crossbridge_protocol::{read_message_sync, write_message_sync, ClientRequest, ServerResponse};
use crosslink::db::Database;
use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread::JoinHandle;
use tempfile::TempDir;

const OWN_SLUG: &str = "c";
const PEER_SLUG: &str = "p";

const CLIENT_BIN: &str = env!("CARGO_BIN_EXE_crossbridge-client");

struct Fixture {
    // Repo state can live in the long default tempdir (no socket length cap).
    _repo_tmp: TempDir,
    // Socket tree must live somewhere whose paths fit in `sockaddr_un.sun_path`
    // (~108 bytes on Linux). We allocate it under `/tmp` directly to keep the
    // total path length comfortably under the cap regardless of $TMPDIR.
    _sock_tmp: TempDir,
    repo_root: PathBuf,
    socket_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo_tmp = tempfile::tempdir().unwrap();
        let sock_tmp = tempfile::Builder::new()
            .prefix("xb")
            .tempdir_in("/tmp")
            .unwrap();
        let repo_root = repo_tmp.path().join("repo");
        let socket_root = sock_tmp.path().to_path_buf();
        fs::create_dir_all(&repo_root).unwrap();

        // Minimal git repo so `derive_own_slug` resolves to OWN_SLUG.
        git(&repo_root, &["init", "-q", "-b", "main"]);
        git(
            &repo_root,
            &[
                "remote",
                "add",
                "origin",
                &format!("git@example.com:org/{OWN_SLUG}.git"),
            ],
        );
        // Allow the binary's `git -C ... remote get-url origin` to succeed
        // even when invoked under `safe.directory` policies in CI sandboxes.
        git(&repo_root, &["config", "safe.directory", "*"]);

        // Empty crosslink DB. Database::open initializes the schema.
        fs::create_dir_all(repo_root.join(".crosslink")).unwrap();
        let db = Database::open(&repo_root.join(".crosslink").join("issues.db")).unwrap();
        drop(db);

        Self {
            _repo_tmp: repo_tmp,
            _sock_tmp: sock_tmp,
            repo_root,
            socket_root,
        }
    }

    fn db(&self) -> Database {
        Database::open(&self.repo_root.join(".crosslink").join("issues.db")).unwrap()
    }

    fn own_socket_dir(&self) -> PathBuf {
        let dir = self.socket_root.join(OWN_SLUG);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(CLIENT_BIN)
            .args(args)
            .current_dir(&self.repo_root)
            .env("CROSSBRIDGE_SOCKET_ROOT", &self.socket_root)
            .output()
            .expect("spawn client binary")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

/// Spawn a one-shot server thread on `socket_path`. Returns a handle and a
/// channel that delivers the captured request once the server responds.
fn spawn_server(
    socket_path: PathBuf,
    response: ServerResponse,
) -> (JoinHandle<()>, mpsc::Receiver<ClientRequest>) {
    let listener = UnixListener::bind(&socket_path).expect("bind mock socket");
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let request: ClientRequest = read_message_sync(&mut stream).expect("read request");
        tx.send(request).expect("forward request");
        write_message_sync(&mut stream, &response).expect("write response");
    });
    (handle, rx)
}

// ---- AC-1: peers --------------------------------------------------------

#[test]
fn peers_lists_one_registered_peer() {
    let f = Fixture::new();
    let dir = f.own_socket_dir();
    UnixListener::bind(dir.join(format!("{PEER_SLUG}.socket"))).unwrap();

    let out = f.run(&["peers"]);
    assert!(out.status.success(), "stderr: {}", str(&out.stderr));
    assert_eq!(str(&out.stdout).trim(), PEER_SLUG);
}

#[test]
fn peers_empty_dir_is_empty_output() {
    let f = Fixture::new();
    let _ = f.own_socket_dir();
    let out = f.run(&["peers"]);
    assert!(out.status.success(), "stderr: {}", str(&out.stderr));
    assert!(str(&out.stdout).trim().is_empty());
}

// ---- AC-2: peers, missing dir ------------------------------------------

#[test]
fn peers_missing_dir_errors() {
    let f = Fixture::new();
    // intentionally do not create the own-slug dir
    let out = f.run(&["peers"]);
    assert!(!out.status.success());
    assert!(
        str(&out.stderr).contains("not registered with crossbridge (no socket dir)"),
        "stderr was: {}",
        str(&out.stderr)
    );
}

// ---- AC-3: submit happy path -------------------------------------------

#[test]
fn submit_round_trips_and_labels_local_issue() {
    let f = Fixture::new();
    let dir = f.own_socket_dir();
    let socket = dir.join(format!("{PEER_SLUG}.socket"));

    let local_id = f
        .db()
        .create_issue("question", Some("body"), "medium")
        .unwrap();

    let (server, rx) = spawn_server(socket, ServerResponse::Ok { issue_id: 4242 });
    let out = f.run(&[
        "submit",
        "--issue",
        &local_id.to_string(),
        "--target",
        PEER_SLUG,
    ]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        str(&out.stdout),
        str(&out.stderr)
    );
    server.join().unwrap();

    let request = rx.recv().unwrap();
    let ClientRequest::Submit(sub) = request else {
        panic!("expected Submit, got {request:?}");
    };
    assert_eq!(sub.title, "question");
    assert_eq!(sub.body, "body");
    assert_eq!(sub.source_slug, OWN_SLUG);
    let local_uuid = f.db().get_issue_uuid_by_id(local_id).unwrap();
    assert_eq!(sub.source_uuid, local_uuid);

    // Local issue picked up the post-submit labels.
    let labels = f.db().get_labels(local_id).unwrap();
    assert!(labels.iter().any(|l| l == "xb:outbound"), "{labels:?}");
    assert!(
        labels.iter().any(|l| l == "xb-status:pending"),
        "{labels:?}"
    );
    let expected_ref = format!("xb-ref:{local_uuid}");
    assert!(
        labels.iter().any(|l| l == &expected_ref),
        "expected {expected_ref:?} in {labels:?}"
    );
}

// ---- AC-4: submit, target not connected --------------------------------

#[test]
fn submit_with_no_socket_errors_and_does_not_label() {
    let f = Fixture::new();
    let _ = f.own_socket_dir(); // dir exists but no peer socket
    let local_id = f.db().create_issue("nope", Some("b"), "medium").unwrap();

    let out = f.run(&[
        "submit",
        "--issue",
        &local_id.to_string(),
        "--target",
        PEER_SLUG,
    ]);
    assert!(!out.status.success());
    let stderr = str(&out.stderr);
    assert!(
        stderr.contains(&format!("peer '{PEER_SLUG}' not available (not connected)")),
        "stderr was: {stderr}"
    );

    let labels = f.db().get_labels(local_id).unwrap();
    assert!(
        !labels.iter().any(|l| l.starts_with("xb")),
        "no xb* labels expected, got: {labels:?}"
    );
}

// ---- AC-5: submit, missing local issue ---------------------------------

#[test]
fn submit_missing_issue_errors() {
    let f = Fixture::new();
    let _ = f.own_socket_dir();
    let out = f.run(&["submit", "--issue", "9999", "--target", PEER_SLUG]);
    assert!(!out.status.success());
    assert!(
        str(&out.stderr).contains("issue #9999 not found"),
        "stderr was: {}",
        str(&out.stderr)
    );
}

// ---- AC-6: answer happy path -------------------------------------------

#[test]
fn answer_sends_result_comments_and_closes() {
    let f = Fixture::new();
    let dir = f.own_socket_dir();
    let socket = dir.join(format!("{PEER_SLUG}.socket"));

    // Local inbound issue tagged appropriately.
    let local_id = f
        .db()
        .create_issue("inbound", Some("from peer"), "medium")
        .unwrap();
    {
        let db = f.db();
        db.add_label(local_id, "xb:inbound").unwrap();
        db.add_label(local_id, &format!("xb-source:{PEER_SLUG}"))
            .unwrap();
        db.add_label(local_id, "xb-ref:source-uuid-xyz").unwrap();
        db.add_comment(local_id, "this is the answer", "result")
            .unwrap();
        db.add_comment(local_id, "ignored note", "note").unwrap();
    }

    let (server, rx) = spawn_server(socket, ServerResponse::Ok { issue_id: 77 });
    let out = f.run(&["answer", "--issue", &local_id.to_string()]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        str(&out.stdout),
        str(&out.stderr)
    );
    server.join().unwrap();

    let request = rx.recv().unwrap();
    let ClientRequest::Answer(ans) = request else {
        panic!("expected Answer, got {request:?}");
    };
    assert_eq!(ans.source_uuid, "source-uuid-xyz");
    assert_eq!(ans.comments.len(), 1);
    assert_eq!(ans.comments[0].kind, "result");
    assert_eq!(ans.comments[0].content, "this is the answer");

    let labels = f.db().get_labels(local_id).unwrap();
    assert!(
        labels.iter().any(|l| l == "xb-status:answered"),
        "{labels:?}"
    );
    let issue = f.db().require_issue(local_id).unwrap();
    assert_eq!(issue.status.as_str(), "closed");
}

// ---- AC-7: answer rejects non-inbound issues ---------------------------

#[test]
fn answer_without_inbound_label_errors_and_leaves_unchanged() {
    let f = Fixture::new();
    let _ = f.own_socket_dir();
    let local_id = f
        .db()
        .create_issue("plain", Some("not inbound"), "medium")
        .unwrap();

    let out = f.run(&["answer", "--issue", &local_id.to_string()]);
    assert!(!out.status.success());
    assert!(
        str(&out.stderr).contains(&format!(
            "issue #{local_id} is not an inbound crossbridge issue"
        )),
        "stderr was: {}",
        str(&out.stderr)
    );

    let issue = f.db().require_issue(local_id).unwrap();
    assert_eq!(issue.status.as_str(), "open");
    let labels = f.db().get_labels(local_id).unwrap();
    assert!(labels.is_empty(), "labels expected empty, got {labels:?}");
}

// ---- AC-9: oversize submission fails fast ------------------------------
//
// The crosslink DB caps a single issue body at 64 KiB, so the CLI can't
// physically build a 16 MiB submission from real DB rows today. The
// guarantee we actually need is upstream of the CLI: the framing helper
// the CLI uses (`write_message_sync`) refuses to emit a frame larger than
// `MAX_FRAME_SIZE` and surfaces a clear error, so a future code path
// (e.g. attachments) can't accidentally produce a malformed frame.

#[test]
fn write_message_sync_refuses_oversize_submission() {
    use crossbridge_protocol::{
        write_message_sync, Attachment, ClientRequest, SubmitIssue, MAX_FRAME_SIZE,
    };

    let req = ClientRequest::Submit(SubmitIssue {
        title: "t".into(),
        body: String::new(),
        labels: Vec::new(),
        source_slug: "s".into(),
        source_uuid: "u".into(),
        attachments: vec![Attachment {
            filename: "blob".into(),
            data: vec![0u8; MAX_FRAME_SIZE + 1],
        }],
    });

    let mut sink: Vec<u8> = Vec::new();
    let err = write_message_sync(&mut sink, &req).expect_err("expected FrameTooLarge");
    let msg = err.to_string();
    assert!(msg.contains("frame too large"), "got: {msg}");
    assert!(
        sink.is_empty(),
        "no bytes should have been written, got {} bytes",
        sink.len()
    );
}

// ---- helpers ------------------------------------------------------------

fn str(buf: &[u8]) -> String {
    String::from_utf8_lossy(buf).into_owned()
}
