//! End-to-end style integration test:
//! - Stand up a fake supervisor on a tempdir Unix socket.
//! - Drive `crossbridge_server::run::run` against it.
//! - Verify peer listener creation, `SubmitIssue` handling (round-trip Ok),
//!   `PeerLeft` listener teardown, and graceful exit on supervisor EOF.
//!
//! The server itself runs in this test task (rather than `tokio::spawn`-ed)
//! because it owns a `crosslink::db::Database`, which is `!Send`.

use std::path::PathBuf;
use std::time::Duration;

use crossbridge_protocol::{
    read_message, write_message, ClientRequest, Notification, Register, RegisterResponse,
    ServerResponse, SubmitIssue, SupervisorMessage,
};
use crossbridge_server::paths::SocketLayout;
use crossbridge_server::run::{self, ServerConfig};
use crosslink::db::Database;
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

mod common;
use common::ShortTempDir;

fn open_db(dir: &std::path::Path) -> Database {
    let crosslink_dir = dir.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    Database::open(&crosslink_dir.join("issues.db")).unwrap()
}

#[tokio::test]
async fn submit_issue_round_trip_via_supervisor_topology() {
    let runtime_root_holder = ShortTempDir::new();
    let runtime_root = runtime_root_holder.path().to_path_buf();
    let repo_holder = ShortTempDir::new();
    let repo_path: PathBuf = repo_holder.path().to_path_buf();
    let _db = open_db(&repo_path); // create .crosslink/issues.db

    let layout = SocketLayout::new(runtime_root.clone());
    let register_socket = layout.register_socket();
    std::fs::create_dir_all(layout.root()).unwrap();

    // Fake supervisor: send Ack with one peer ("repo-b"), wait, then PeerLeft.
    let supervisor_listener = UnixListener::bind(&register_socket).unwrap();
    let supervisor_task = tokio::spawn(async move {
        let (mut s, _) = supervisor_listener.accept().await.unwrap();
        let _reg: Register = read_message(&mut s).await.unwrap();
        write_message(
            &mut s,
            &SupervisorMessage::RegisterResponse(RegisterResponse::Ack {
                peers: vec!["repo-b".to_string()],
            }),
        )
        .await
        .unwrap();
        // Hold the stream open so the server stays in its session loop.
        // After ~500ms send PeerLeft.
        tokio::time::sleep(Duration::from_millis(500)).await;
        write_message(
            &mut s,
            &SupervisorMessage::Notification(Notification::PeerLeft {
                slug: "repo-b".to_string(),
            }),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await; // hold open
    });

    let cfg = ServerConfig {
        slug: "repo-a".to_string(),
        group: "amd-psp".to_string(),
        repo_path: repo_path.clone(),
        layout: layout.clone(),
    };
    let layout_for_client = layout.clone();
    let repo_path_for_client = repo_path.clone();
    let shutdown = CancellationToken::new();
    let driver_shutdown = shutdown.clone();

    let driver = async move {
        // Wait for the listener at /<root>/repo-b/repo-a.socket to appear.
        let listener_path = layout_for_client.listener_socket("repo-b", "repo-a");
        for _ in 0..100 {
            if listener_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            listener_path.exists(),
            "listener never appeared at {}",
            listener_path.display()
        );

        // Connect as a client and send a SubmitIssue.
        let mut client = UnixStream::connect(&listener_path).await.unwrap();
        let req = ClientRequest::Submit(SubmitIssue {
            title: "Hello from repo-b".to_string(),
            body: "Plz answer".to_string(),
            labels: vec![],
            source_slug: "repo-b".to_string(),
            source_uuid: "uuid-xyz".to_string(),
            attachments: vec![],
        });
        write_message(&mut client, &req).await.unwrap();
        let resp: ServerResponse = read_message(&mut client).await.unwrap();
        let issue_id = match resp {
            ServerResponse::Ok { issue_id } => issue_id,
            ServerResponse::Error { message } => panic!("server error: {message}"),
        };
        assert!(issue_id > 0);

        // Verify the issue exists in the local DB.
        let db =
            Database::open(&repo_path_for_client.join(".crosslink").join("issues.db")).unwrap();
        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, "Hello from repo-b");
        let labels = db.get_labels(issue_id).unwrap();
        assert!(labels.iter().any(|l| l == "xb-source:repo-b"));
        assert!(labels.iter().any(|l| l == "xb-ref:uuid-xyz"));
        drop(db);

        // Wait for PeerLeft to land and the listener socket to be unlinked.
        for _ in 0..200 {
            if !listener_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !listener_path.exists(),
            "listener never removed at {}",
            listener_path.display()
        );

        // Final assertion for the new shutdown plumbing: cancelling the
        // token must drive `run` to return Ok cleanly.
        driver_shutdown.cancel();
    };

    // Run the server inline (it owns a !Send Database) and race the driver.
    // After the driver finishes (and cancels `shutdown`), `run` must return
    // Ok(()) — this exercises the new programmatic-shutdown path.
    let (server_result, ()) = tokio::join!(run::run(cfg, shutdown), driver);
    server_result.expect("server exited cleanly on shutdown.cancel()");

    supervisor_task.abort();
    let _ = supervisor_task.await;
}
