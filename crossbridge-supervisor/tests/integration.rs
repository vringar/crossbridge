//! Integration tests for the supervisor.
//!
//! Each test spins up a supervisor on a temp-dir register socket and exercises
//! one acceptance criterion. The supervisor task is aborted when the test
//! function returns; the tempdir is dropped automatically.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbridge_protocol::{
    read_message, write_message, Notification, Register, RegisterResponse, SupervisorMessage,
};
use tempfile::{Builder, TempDir};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const RECV_TIMEOUT: Duration = Duration::from_secs(2);

/// Build a tempdir directly under `/tmp` with a short prefix.
///
/// The sandbox's `$TMPDIR` is too long to fit a Unix-socket `sun_path`
/// (108 bytes), so we bypass it and put the socket under `/tmp` directly.
fn short_tempdir() -> TempDir {
    Builder::new()
        .prefix("cb-")
        .tempdir_in("/tmp")
        .expect("create tempdir under /tmp")
}

fn socket_path(tmp: &TempDir) -> PathBuf {
    tmp.path().join("r.sock")
}

struct Supervisor {
    handle: JoinHandle<anyhow::Result<()>>,
    path: PathBuf,
    tmp: TempDir,
}

impl Supervisor {
    async fn start() -> Self {
        let tmp = short_tempdir();
        let path = socket_path(&tmp);
        Self::start_at(tmp, path).await
    }

    async fn start_at(tmp: TempDir, path: PathBuf) -> Self {
        let p = path.clone();
        let handle = tokio::spawn(async move { crossbridge_supervisor::run(&p).await });
        wait_until_listening(&path).await;
        Supervisor { handle, path, tmp }
    }

    fn base_dir(&self) -> &Path {
        self.path.parent().unwrap()
    }

    async fn shutdown(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

async fn wait_until_listening(path: &Path) {
    let deadline = tokio::time::Instant::now() + RECV_TIMEOUT;
    loop {
        match UnixStream::connect(path).await {
            Ok(_) => return,
            Err(e) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "supervisor did not start listening at {} within {:?}: last err={}",
                    path.display(),
                    RECV_TIMEOUT,
                    e
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

async fn register(path: &Path, slug: &str, group: &str) -> (UnixStream, RegisterResponse) {
    let mut stream = UnixStream::connect(path).await.expect("connect");
    let reg = Register {
        slug: slug.into(),
        group: group.into(),
    };
    write_message(&mut stream, &reg).await.unwrap();
    let resp = read_supervisor_message(&mut stream).await;
    match resp {
        SupervisorMessage::RegisterResponse(r) => (stream, r),
        SupervisorMessage::Notification(n) => {
            panic!("expected RegisterResponse, got Notification({n:?})")
        }
    }
}

async fn read_supervisor_message(stream: &mut UnixStream) -> SupervisorMessage {
    timeout(RECV_TIMEOUT, read_message::<_, SupervisorMessage>(stream))
        .await
        .expect("read timed out")
        .expect("read failed")
}

async fn read_notification(stream: &mut UnixStream) -> Notification {
    match read_supervisor_message(stream).await {
        SupervisorMessage::Notification(n) => n,
        SupervisorMessage::RegisterResponse(r) => {
            panic!("expected Notification, got RegisterResponse({r:?})")
        }
    }
}

async fn expect_no_message(stream: &mut UnixStream, dur: Duration) {
    match timeout(dur, read_message::<_, SupervisorMessage>(stream)).await {
        Err(_) => {} // timeout: good
        Ok(Ok(msg)) => panic!("unexpected message: {msg:?}"),
        Ok(Err(e)) => panic!("unexpected read error: {e}"),
    }
}

#[tokio::test]
async fn register_ack_with_empty_peers() {
    let sup = Supervisor::start().await;

    let (_alpha, ack) = register(&sup.path, "alpha", "g1").await;
    match ack {
        RegisterResponse::Ack { peers } => {
            assert!(peers.is_empty(), "expected empty, got {peers:?}");
        }
        RegisterResponse::Nack { reason } => panic!("expected Ack, got Nack: {reason}"),
    }

    assert!(
        sup.base_dir().join("alpha").is_dir(),
        "slug directory missing"
    );

    sup.shutdown().await;
}

#[tokio::test]
async fn register_ack_with_existing_peers_and_join_fanout() {
    let sup = Supervisor::start().await;

    let (mut alpha, ack_a) = register(&sup.path, "alpha", "g1").await;
    assert!(matches!(ack_a, RegisterResponse::Ack { peers } if peers.is_empty()));

    let (_beta, ack_b) = register(&sup.path, "beta", "g1").await;
    match ack_b {
        RegisterResponse::Ack { peers } => assert_eq!(peers, vec!["alpha".to_string()]),
        RegisterResponse::Nack { reason } => panic!("nack: {reason}"),
    }

    // alpha should observe beta joining.
    let n = read_notification(&mut alpha).await;
    assert_eq!(
        n,
        Notification::PeerJoined {
            slug: "beta".into()
        }
    );

    sup.shutdown().await;
}

#[tokio::test]
async fn duplicate_slug_in_group_is_nacked_and_first_unaffected() {
    let sup = Supervisor::start().await;

    let (mut alpha, ack_a) = register(&sup.path, "alpha", "g1").await;
    assert!(matches!(ack_a, RegisterResponse::Ack { .. }));

    let (mut dup, ack_d) = register(&sup.path, "alpha", "g1").await;
    match ack_d {
        RegisterResponse::Nack { reason } => assert!(!reason.is_empty(), "empty nack reason"),
        RegisterResponse::Ack { peers } => panic!("expected Nack, got Ack with peers={peers:?}"),
    }

    // The duplicate connection should be closed by the supervisor.
    let mut buf = [0u8; 16];
    let read_result = timeout(RECV_TIMEOUT, dup.read(&mut buf)).await;
    match read_result {
        // EOF or connection-level error both indicate the supervisor closed the dup.
        Ok(Ok(0) | Err(_)) => {}
        Ok(Ok(n)) => panic!("expected EOF on duplicate, read {n} bytes"),
        Err(elapsed) => panic!(
            "supervisor did not close duplicate connection within {RECV_TIMEOUT:?}: {elapsed}"
        ),
    }

    // The original alpha connection must remain usable: a new same-group join
    // still notifies it.
    let (_beta, _) = register(&sup.path, "beta", "g1").await;
    let n = read_notification(&mut alpha).await;
    assert_eq!(
        n,
        Notification::PeerJoined {
            slug: "beta".into()
        }
    );

    sup.shutdown().await;
}

#[tokio::test]
async fn different_groups_do_not_see_each_other() {
    let sup = Supervisor::start().await;

    let (mut alpha, _) = register(&sup.path, "alpha", "g1").await;
    let (mut gamma, ack_g) = register(&sup.path, "gamma", "g2").await;
    match ack_g {
        RegisterResponse::Ack { peers } => assert!(peers.is_empty()),
        RegisterResponse::Nack { reason } => panic!("nack: {reason}"),
    }

    expect_no_message(&mut alpha, Duration::from_millis(150)).await;
    expect_no_message(&mut gamma, Duration::from_millis(150)).await;

    sup.shutdown().await;
}

#[tokio::test]
async fn peer_left_fanout_on_eof_and_dir_removed() {
    let sup = Supervisor::start().await;
    let alpha_dir = sup.base_dir().join("alpha");

    let (alpha, _) = register(&sup.path, "alpha", "g1").await;
    let (mut beta, _) = register(&sup.path, "beta", "g1").await;
    // Drain beta's PeerJoined for alpha-was-already-there case? No: beta is
    // the second to join, so it received Ack with peers=["alpha"], no notif.

    // Sanity: both slug dirs exist.
    assert!(alpha_dir.is_dir());
    assert!(sup.base_dir().join("beta").is_dir());

    // Drop alpha's stream → supervisor sees EOF → notifies beta and removes
    // /run/crossbridge/alpha/.
    drop(alpha);

    let n = read_notification(&mut beta).await;
    assert_eq!(
        n,
        Notification::PeerLeft {
            slug: "alpha".into()
        }
    );

    // The directory removal happens just before fanout, so it must be gone now.
    assert!(!alpha_dir.exists(), "alpha directory not removed");

    sup.shutdown().await;
}

#[tokio::test]
async fn departure_dir_removal_includes_files_other_servers_placed() {
    let sup = Supervisor::start().await;
    let alpha_dir = sup.base_dir().join("alpha");

    let (alpha, _) = register(&sup.path, "alpha", "g1").await;
    let (mut beta, _) = register(&sup.path, "beta", "g1").await;

    // Simulate beta dropping a socket file in alpha's dir for an agent to use.
    let foreign_socket = alpha_dir.join("beta.socket");
    std::fs::write(&foreign_socket, b"").unwrap();
    assert!(foreign_socket.exists());

    drop(alpha);
    let n = read_notification(&mut beta).await;
    assert_eq!(
        n,
        Notification::PeerLeft {
            slug: "alpha".into()
        }
    );

    assert!(!alpha_dir.exists(), "alpha dir not removed");
    assert!(!foreign_socket.exists(), "foreign socket leaked");

    sup.shutdown().await;
}

#[tokio::test]
async fn supervisor_restart_wipes_base_dir() {
    let tmp = short_tempdir();
    let path = socket_path(&tmp);

    let sup1 = Supervisor::start_at(tmp, path.clone()).await;
    let alpha_dir = sup1.base_dir().join("alpha");

    let (alpha, _) = register(&sup1.path, "alpha", "g1").await;
    assert!(alpha_dir.is_dir());

    // Plant some extra junk that should be wiped on restart.
    let stale_dir = sup1.base_dir().join("ghost");
    std::fs::create_dir(&stale_dir).unwrap();
    std::fs::write(stale_dir.join("relic"), b"old").unwrap();
    let stale_file = sup1.base_dir().join("stray.tmp");
    std::fs::write(&stale_file, b"").unwrap();

    // Take ownership of the tempdir before shutting down so it survives.
    let base_dir = sup1.base_dir().to_path_buf();
    let recovered_tmp = sup1.tmp;
    sup1.handle.abort();
    let _ = sup1.handle.await;
    drop(alpha); // alpha sees EOF on the dead supervisor; its slug dir may
                 // briefly persist between supervisors.

    // The directories may still be on disk because the supervisor task died.
    assert!(base_dir.exists());

    // Restart the supervisor against the same socket path.
    let sup2 = Supervisor::start_at(recovered_tmp, path.clone()).await;

    // After startup, all prior contents should be wiped.
    assert!(!alpha_dir.exists(), "alpha dir not wiped on restart");
    assert!(!stale_dir.exists(), "stale dir not wiped on restart");
    assert!(!stale_file.exists(), "stale file not wiped on restart");

    // Fresh registration works.
    let (_alpha2, ack) = register(&sup2.path, "alpha", "g1").await;
    match ack {
        RegisterResponse::Ack { peers } => assert!(peers.is_empty()),
        RegisterResponse::Nack { reason } => panic!("post-restart nack: {reason}"),
    }
    assert!(sup2.base_dir().join("alpha").is_dir());

    sup2.shutdown().await;
}

#[tokio::test]
async fn stale_socket_file_is_removed_and_rebound() {
    let tmp = short_tempdir();
    let path = socket_path(&tmp);

    // Plant a stale regular file at the socket path; bind would normally fail.
    std::fs::write(&path, b"stale").unwrap();
    assert!(path.exists());

    let sup = Supervisor::start_at(tmp, path.clone()).await;

    let (_alpha, ack) = register(&sup.path, "alpha", "g1").await;
    assert!(matches!(ack, RegisterResponse::Ack { .. }));

    sup.shutdown().await;
}
