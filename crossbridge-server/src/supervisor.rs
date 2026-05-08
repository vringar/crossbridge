//! Supervisor stream lifecycle: connect, register, read notifications, and
//! reconnect with exponential backoff.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use crossbridge_protocol::{
    read_message, write_message, Notification, Register, RegisterResponse, SupervisorMessage,
};
use tokio::net::UnixStream;

/// Initial reconnect delay; doubles each failure up to [`MAX_BACKOFF`].
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Cap on the reconnect backoff between attempts.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// State established after a successful registration: the live duplex stream
/// to the supervisor, plus the peer slugs the supervisor reported in its
/// `RegisterResponse::Ack`.
pub struct Registration {
    pub stream: UnixStream,
    pub peers: Vec<String>,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("peers", &self.peers)
            .finish_non_exhaustive()
    }
}

/// Connect once to the supervisor at `socket_path` and send `Register { slug, group }`.
///
/// On success, returns the open stream and the peer list reported by the
/// supervisor. On `Nack` or any I/O / protocol error, returns `Err` — the
/// caller is expected to retry with backoff (see [`connect_and_register_with_backoff`]).
///
/// # Errors
/// Returns an error if the connection cannot be established, the `Register`
/// frame cannot be sent, the response frame cannot be read, the supervisor
/// returns `Nack`, or the first frame is not a `RegisterResponse`.
pub async fn connect_and_register(
    socket_path: &Path,
    slug: &str,
    group: &str,
) -> Result<Registration> {
    let mut stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow!(
            "connecting to supervisor at {}: {}",
            socket_path.display(),
            e
        )
    })?;

    write_message(
        &mut stream,
        &Register {
            slug: slug.to_string(),
            group: group.to_string(),
        },
    )
    .await
    .map_err(|e| anyhow!("sending Register frame: {e}"))?;

    // The supervisor sends a SupervisorMessage envelope; for the first frame
    // we expect RegisterResponse.
    let first: SupervisorMessage = read_message(&mut stream)
        .await
        .map_err(|e| anyhow!("reading RegisterResponse frame: {e}"))?;

    match first {
        SupervisorMessage::RegisterResponse(RegisterResponse::Ack { peers }) => {
            tracing::info!(
                slug,
                group,
                peer_count = peers.len(),
                "registered with supervisor"
            );
            Ok(Registration { stream, peers })
        }
        SupervisorMessage::RegisterResponse(RegisterResponse::Nack { reason }) => {
            Err(anyhow!("supervisor rejected registration: {reason}"))
        }
        SupervisorMessage::Notification(n) => Err(anyhow!(
            "expected RegisterResponse from supervisor, got Notification: {n:?}"
        )),
    }
}

/// Connect to the supervisor, retrying with exponential backoff (1s, 2s, 4s, ...
/// capped at [`MAX_BACKOFF`]) on connect / register failure.
///
/// `Nack` is *not* retried — a slug collision will not resolve itself, and
/// silent looping would mask a config error from the operator.
///
/// # Errors
/// Returns an error if the supervisor sends `Nack`. Other I/O / protocol
/// failures are retried indefinitely.
pub async fn connect_and_register_with_backoff(
    socket_path: &Path,
    slug: &str,
    group: &str,
) -> Result<Registration> {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match connect_and_register(socket_path, slug, group).await {
            Ok(reg) => return Ok(reg),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("supervisor rejected registration") {
                    return Err(e);
                }
                tracing::warn!(
                    slug,
                    group,
                    backoff_s = backoff.as_secs(),
                    "supervisor connect/register failed: {msg}; retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Read the next [`Notification`] from the supervisor stream.
///
/// Returns `Ok(None)` on a clean EOF, `Ok(Some(...))` on a notification, and
/// `Err` on any other I/O or protocol failure. `RegisterResponse` frames are
/// rejected — they should never arrive after the initial handshake.
///
/// # Errors
/// Returns an error if the supervisor sends a `RegisterResponse` mid-stream
/// or if any non-EOF I/O / protocol failure occurs.
pub async fn read_notification(stream: &mut UnixStream) -> Result<Option<Notification>> {
    match read_message::<_, SupervisorMessage>(stream).await {
        Ok(SupervisorMessage::Notification(n)) => Ok(Some(n)),
        Ok(SupervisorMessage::RegisterResponse(r)) => Err(anyhow!(
            "supervisor sent unexpected RegisterResponse mid-stream: {r:?}"
        )),
        Err(crossbridge_protocol::Error::Io(e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof =>
        {
            Ok(None)
        }
        Err(e) => Err(anyhow!("reading notification: {e}")),
    }
}

/// Convenience wrapper used by the main loop: returns the `(register_socket,
/// runtime_root)` pair for cleanup paths derived from the supervisor's socket.
pub fn runtime_root_from_socket(register_socket: &Path) -> PathBuf {
    register_socket
        .parent()
        .map_or_else(|| PathBuf::from("/run/crossbridge"), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ShortTempDir;
    use crossbridge_protocol::{
        write_message, Notification, Register, RegisterResponse, SupervisorMessage,
    };
    use tokio::net::UnixListener;

    // `async` here is intentional: callers `.await` this fixture, and the
    // sync `UnixListener::bind` must run on the tokio runtime.
    #[allow(clippy::unused_async)]
    async fn run_fake_supervisor(
        socket: PathBuf,
        response: SupervisorMessage,
        followups: Vec<SupervisorMessage>,
    ) -> tokio::task::JoinHandle<Register> {
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let reg: Register = read_message(&mut stream).await.unwrap();
            write_message(&mut stream, &response).await.unwrap();
            for n in followups {
                write_message(&mut stream, &n).await.unwrap();
            }
            // Hold the stream open briefly so the client side can drain.
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(stream);
            reg
        })
    }

    #[tokio::test]
    async fn register_ack_returns_peers() {
        let dir = ShortTempDir::new();
        let sock = dir.path().join("register.socket");
        let server = run_fake_supervisor(
            sock.clone(),
            SupervisorMessage::RegisterResponse(RegisterResponse::Ack {
                peers: vec!["repo-b".to_string()],
            }),
            vec![],
        )
        .await;

        let reg = connect_and_register(&sock, "repo-a", "amd-psp")
            .await
            .unwrap();
        assert_eq!(reg.peers, vec!["repo-b".to_string()]);
        let observed = server.await.unwrap();
        assert_eq!(observed.slug, "repo-a");
        assert_eq!(observed.group, "amd-psp");
    }

    #[tokio::test]
    async fn register_nack_is_error() {
        let dir = ShortTempDir::new();
        let sock = dir.path().join("register.socket");
        let _server = run_fake_supervisor(
            sock.clone(),
            SupervisorMessage::RegisterResponse(RegisterResponse::Nack {
                reason: "slug taken".to_string(),
            }),
            vec![],
        )
        .await;

        let err = connect_and_register(&sock, "repo-a", "amd-psp")
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("rejected"), "got: {msg}");
        assert!(msg.contains("slug taken"), "got: {msg}");
    }

    #[tokio::test]
    async fn read_notification_decodes_peer_joined() {
        let dir = ShortTempDir::new();
        let sock = dir.path().join("register.socket");
        let _server = run_fake_supervisor(
            sock.clone(),
            SupervisorMessage::RegisterResponse(RegisterResponse::Ack { peers: vec![] }),
            vec![SupervisorMessage::Notification(Notification::PeerJoined {
                slug: "repo-c".to_string(),
            })],
        )
        .await;

        let mut reg = connect_and_register(&sock, "repo-a", "amd-psp")
            .await
            .unwrap();
        let n = read_notification(&mut reg.stream).await.unwrap();
        assert_eq!(
            n,
            Some(Notification::PeerJoined {
                slug: "repo-c".to_string()
            })
        );
        // After the followup, the fake supervisor closes -> EOF.
        let n2 = read_notification(&mut reg.stream).await.unwrap();
        assert!(n2.is_none());
    }

    #[tokio::test]
    async fn backoff_eventually_connects() {
        let dir = ShortTempDir::new();
        let sock = dir.path().join("register.socket");

        let sock_for_server = sock.clone();
        let _server = tokio::spawn(async move {
            // Bind a moment after the client starts retrying.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let listener = UnixListener::bind(&sock_for_server).unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Register = read_message(&mut stream).await.unwrap();
            write_message(
                &mut stream,
                &SupervisorMessage::RegisterResponse(RegisterResponse::Ack { peers: vec![] }),
            )
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        // Use a very small initial backoff in this test by setting
        // `INITIAL_BACKOFF` - we can't, so instead drive the public function
        // with a real wait. The default 1s backoff means the test takes ~1s
        // worst case, which is acceptable.
        let reg = connect_and_register_with_backoff(&sock, "repo-a", "amd-psp")
            .await
            .unwrap();
        assert!(reg.peers.is_empty());
    }
}
