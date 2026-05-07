//! Per-host supervisor for crossbridge.
//!
//! Coordinates peer-group socket topology under a base directory (typically
//! `/run/crossbridge/`). Repo servers connect to the register socket, send
//! `Register { slug, group }`, and stay attached to a persistent stream over
//! which the supervisor delivers `PeerJoined` / `PeerLeft` notifications for
//! the same group.
//!
//! See `.design/supervisor.md` for the full specification.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use crossbridge_protocol::{
    read_message, write_message, Notification, Register, RegisterResponse, SupervisorMessage,
    DEFAULT_SOCKET_ROOT, SOCKET_ROOT_ENV,
};
use tokio::io::AsyncReadExt;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

type ConnId = u64;

/// Resolve the register socket path with this precedence:
/// 1. `flag` (e.g. `--socket /custom/register.socket`)
/// 2. `$CROSSBRIDGE_SOCKET_ROOT/register.socket` if the env var is set
/// 3. compiled-in default `/run/crossbridge/register.socket`
///
/// `env_lookup` is parameterized so tests can inject env values without
/// touching the global process environment.
pub fn resolve_register_socket<F>(flag: Option<&Path>, env_lookup: F) -> PathBuf
where
    F: FnOnce(&str) -> Option<OsString>,
{
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(root) = env_lookup(SOCKET_ROOT_ENV) {
        let root: &OsStr = &root;
        return PathBuf::from(root).join("register.socket");
    }
    PathBuf::from(DEFAULT_SOCKET_ROOT).join("register.socket")
}

/// Run the supervisor against the given register socket path.
///
/// The parent directory of `socket_path` is treated as the base directory;
/// its contents are wiped on startup and slug subdirectories are created
/// there for each registered peer. The function loops indefinitely; cancel
/// the calling task to stop it.
pub async fn run(socket_path: impl AsRef<Path>) -> Result<()> {
    let socket_path = socket_path.as_ref().to_path_buf();
    let base_dir = socket_path
        .parent()
        .ok_or_else(|| {
            anyhow!(
                "socket path '{}' has no parent directory",
                socket_path.display()
            )
        })?
        .to_path_buf();

    prepare_base_dir(&base_dir).context("preparing base directory")?;

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding register socket at {}", socket_path.display()))?;
    info!(path = %socket_path.display(), "supervisor listening");

    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<Event>();
    let mut state = State::new(base_dir, events_tx.clone());

    loop {
        tokio::select! {
            accept = listener.accept() => match accept {
                Ok((stream, _)) => spawn_registration_reader(stream, events_tx.clone()),
                Err(e) => error!("accept failed: {e}"),
            },
            Some(event) = events_rx.recv() => {
                state.handle_event(event).await;
            }
        }
    }
}

/// Wipe the contents of `base_dir` (creating it if missing).
///
/// Both the stale register socket and any leftover slug directories from a
/// prior run live here, so a single sweep handles AC-4 and AC-5.
fn prepare_base_dir(base_dir: &Path) -> Result<()> {
    match std::fs::read_dir(base_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    std::fs::remove_dir_all(&path)
                        .with_context(|| format!("removing directory {}", path.display()))?;
                } else {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("removing file {}", path.display()))?;
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::create_dir_all(base_dir)
            .with_context(|| format!("creating {}", base_dir.display())),
        Err(e) => Err(e.into()),
    }
}

enum Event {
    Registered {
        stream: UnixStream,
        register: Register,
    },
    Departed {
        id: ConnId,
    },
}

fn spawn_registration_reader(mut stream: UnixStream, events_tx: mpsc::UnboundedSender<Event>) {
    tokio::spawn(async move {
        match read_message::<_, Register>(&mut stream).await {
            Ok(register) => {
                let _ = events_tx.send(Event::Registered { stream, register });
            }
            Err(e) => {
                warn!("failed to read Register message from new connection: {e}");
            }
        }
    });
}

struct State {
    base_dir: PathBuf,
    next_id: ConnId,
    /// `group → slug → conn_id`
    groups: HashMap<String, HashMap<String, ConnId>>,
    conns: HashMap<ConnId, ConnRecord>,
    events_tx: mpsc::UnboundedSender<Event>,
}

struct ConnRecord {
    slug: String,
    group: String,
    write_half: OwnedWriteHalf,
    reader: JoinHandle<()>,
}

impl State {
    fn new(base_dir: PathBuf, events_tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            base_dir,
            next_id: 0,
            groups: HashMap::new(),
            conns: HashMap::new(),
            events_tx,
        }
    }

    async fn handle_event(&mut self, event: Event) {
        match event {
            Event::Registered { stream, register } => {
                self.handle_registration(stream, register).await;
            }
            Event::Departed { id } => {
                self.handle_departure(id).await;
            }
        }
    }

    async fn handle_registration(&mut self, stream: UnixStream, register: Register) {
        let Register { slug, group } = register;

        if self
            .groups
            .get(&group)
            .is_some_and(|m| m.contains_key(&slug))
        {
            warn!(%slug, %group, "duplicate slug; sending Nack");
            let mut stream = stream;
            let nack = SupervisorMessage::RegisterResponse(RegisterResponse::Nack {
                reason: format!("slug '{slug}' already registered in group '{group}'"),
            });
            if let Err(e) = write_message(&mut stream, &nack).await {
                debug!("failed to send Nack: {e}");
            }
            return;
        }

        let slug_dir = self.base_dir.join(&slug);
        if let Err(e) = std::fs::create_dir_all(&slug_dir) {
            error!(%slug, dir = %slug_dir.display(), "failed to create slug directory: {e}");
            let mut stream = stream;
            let nack = SupervisorMessage::RegisterResponse(RegisterResponse::Nack {
                reason: format!("failed to create slug directory: {e}"),
            });
            let _ = write_message(&mut stream, &nack).await;
            return;
        }

        let peers: Vec<String> = self
            .groups
            .get(&group)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();

        let (read_half, mut write_half) = stream.into_split();
        let ack = SupervisorMessage::RegisterResponse(RegisterResponse::Ack { peers });
        if let Err(e) = write_message(&mut write_half, &ack).await {
            warn!(%slug, "failed to send Ack: {e}");
            let _ = std::fs::remove_dir_all(&slug_dir);
            return;
        }

        let id = self.next_id;
        self.next_id += 1;

        let reader = spawn_eof_watcher(read_half, id, self.events_tx.clone());

        let peer_ids: Vec<ConnId> = self
            .groups
            .get(&group)
            .map(|m| m.values().copied().collect())
            .unwrap_or_default();

        let joined =
            SupervisorMessage::Notification(Notification::PeerJoined { slug: slug.clone() });
        let mut failed_peers = Vec::new();
        for peer_id in peer_ids {
            if let Some(rec) = self.conns.get_mut(&peer_id) {
                if let Err(e) = write_message(&mut rec.write_half, &joined).await {
                    warn!(peer_id, %slug, "failed to deliver PeerJoined: {e}");
                    failed_peers.push(peer_id);
                }
            }
        }

        info!(%slug, %group, conn_id = id, "registered");
        self.groups
            .entry(group.clone())
            .or_default()
            .insert(slug.clone(), id);
        self.conns.insert(
            id,
            ConnRecord {
                slug,
                group,
                write_half,
                reader,
            },
        );

        for peer_id in failed_peers {
            let _ = self.events_tx.send(Event::Departed { id: peer_id });
        }
    }

    async fn handle_departure(&mut self, id: ConnId) {
        let Some(rec) = self.conns.remove(&id) else {
            return;
        };
        let ConnRecord {
            slug,
            group,
            write_half,
            reader,
        } = rec;
        drop(write_half);
        reader.abort();

        if let Some(group_map) = self.groups.get_mut(&group) {
            group_map.remove(&slug);
            if group_map.is_empty() {
                self.groups.remove(&group);
            }
        }

        let slug_dir = self.base_dir.join(&slug);
        if let Err(e) = std::fs::remove_dir_all(&slug_dir) {
            if e.kind() != io::ErrorKind::NotFound {
                warn!(%slug, dir = %slug_dir.display(), "failed to remove slug directory: {e}");
            }
        }

        info!(%slug, %group, conn_id = id, "departed");

        let peer_ids: Vec<ConnId> = self
            .groups
            .get(&group)
            .map(|m| m.values().copied().collect())
            .unwrap_or_default();

        let left = SupervisorMessage::Notification(Notification::PeerLeft { slug: slug.clone() });
        let mut failed_peers = Vec::new();
        for peer_id in peer_ids {
            if let Some(rec) = self.conns.get_mut(&peer_id) {
                if let Err(e) = write_message(&mut rec.write_half, &left).await {
                    warn!(peer_id, %slug, "failed to deliver PeerLeft: {e}");
                    failed_peers.push(peer_id);
                }
            }
        }

        for peer_id in failed_peers {
            let _ = self.events_tx.send(Event::Departed { id: peer_id });
        }
    }
}

fn spawn_eof_watcher(
    mut read_half: OwnedReadHalf,
    id: ConnId,
    events_tx: mpsc::UnboundedSender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = [0u8; 64];
        loop {
            match read_half.read(&mut buf).await {
                Ok(0) => {
                    debug!(conn_id = id, "stream EOF");
                    break;
                }
                Ok(n) => {
                    warn!(
                        conn_id = id,
                        bytes = n,
                        "ignoring unexpected data from registered server"
                    );
                }
                Err(e) => {
                    debug!(conn_id = id, "stream read error: {e}");
                    break;
                }
            }
        }
        let _ = events_tx.send(Event::Departed { id });
    })
}

#[cfg(test)]
mod resolve_register_socket_tests {
    use super::*;

    #[test]
    fn flag_only_wins() {
        let flag = PathBuf::from("/custom/r.sock");
        let resolved = resolve_register_socket(Some(&flag), |_| None);
        assert_eq!(resolved, flag);
    }

    #[test]
    fn env_only_used_when_no_flag() {
        let resolved =
            resolve_register_socket(None, |k| (k == SOCKET_ROOT_ENV).then(|| "/srv/run".into()));
        assert_eq!(resolved, PathBuf::from("/srv/run/register.socket"));
    }

    #[test]
    fn flag_overrides_env() {
        let flag = PathBuf::from("/custom/r.sock");
        let resolved = resolve_register_socket(Some(&flag), |_| Some(OsString::from("/srv/run")));
        assert_eq!(resolved, flag);
    }

    #[test]
    fn neither_falls_back_to_default() {
        let resolved = resolve_register_socket(None, |_| None);
        assert_eq!(resolved, PathBuf::from("/run/crossbridge/register.socket"));
    }
}
