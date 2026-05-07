//! AC-9: a request whose framed payload exceeds 16 MiB must be rejected with
//! an error response, not by crashing the server.
//!
//! We exercise this by hand-writing a 4-byte length prefix that is larger than
//! `MAX_FRAME_SIZE` and invoking `handler::handle_connection` directly — the
//! handler should write back a `ServerResponse::Error` and return cleanly.

use std::path::PathBuf;

use crossbridge_protocol::{read_message, ServerResponse, MAX_FRAME_SIZE};
use crossbridge_server::handler;
use crosslink::db::Database;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};

mod common;
use common::ShortTempDir;

fn open_db(dir: &std::path::Path) -> Database {
    let crosslink_dir = dir.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    Database::open(&crosslink_dir.join("issues.db")).unwrap()
}

#[tokio::test]
async fn oversized_frame_returns_error_response() {
    let dir = ShortTempDir::new();
    let db = open_db(dir.path());
    let sock = dir.path().join("peer.socket");
    let listener = UnixListener::bind(&sock).unwrap();
    let repo_path = PathBuf::from(dir.path());

    // Connect first so the listener has something to accept.
    let connect_fut = UnixStream::connect(&sock);
    let (client, accepted) = tokio::join!(connect_fut, listener.accept());
    let (mut conn, _addr) = accepted.unwrap();
    let mut client = client.unwrap();

    // The server-side handler runs in this same task via tokio::select! while
    // the test acts as the client below — Database is !Send so we cannot
    // spawn the handler.
    let server_fut = handler::handle_connection(&mut conn, "peer-a", &db, &repo_path);
    let client_fut = async {
        // Send a length prefix that exceeds MAX_FRAME_SIZE; the framing layer
        // rejects this before reading any payload.
        let oversize = (MAX_FRAME_SIZE as u32) + 1;
        client.write_all(&oversize.to_be_bytes()).await.unwrap();
        let response: ServerResponse = read_message(&mut client).await.unwrap();
        response
    };

    let (server_res, response) = tokio::join!(server_fut, client_fut);
    server_res.expect("handler should not propagate an error for oversize frames");
    match response {
        ServerResponse::Error { message } => {
            assert!(
                message.to_lowercase().contains("frame too large")
                    || message.to_lowercase().contains("too large"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected ServerResponse::Error, got {other:?}"),
    }
}
