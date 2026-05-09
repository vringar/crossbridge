# crossbridge-supervisor

Per-user daemon that coordinates the peer-group socket topology. Repo servers
connect to its register socket, send `Register { slug, group }`, and stay
attached to a persistent stream over which the supervisor delivers
`PeerJoined` / `PeerLeft` notifications for the same group.

**Spec:** [`.design/supervisor.md`](../.design/supervisor.md)

## Binary

```
crossbridge-supervisor [--socket <PATH>]
```

**Resolution precedence for the register socket path:**
1. `--socket <PATH>` flag (taken verbatim)
2. `<root>/register.socket`, where `<root>` comes from:
   1. `$CROSSBRIDGE_SOCKET_ROOT`
   2. `$XDG_RUNTIME_DIR/crossbridge`
   3. compiled-in fallback `/run/crossbridge`

The parent directory of the resolved socket is the **base directory**: it is
**wiped on startup** (AC-4/AC-5) and slug subdirectories are created there for
each registered peer.

## Deployment

Runs as a **systemd user service** — see [`nix/module.nix`](../nix/module.nix)
(`services.crossbridge-supervisor`). Default socket root is
`%t/crossbridge` → `/run/user/$UID/crossbridge`. Server and client must run
as the same user.

## Public API

- `pub async fn run(socket_path: impl AsRef<Path>) -> Result<()>` — bind + serve loop
- `pub fn resolve_register_socket(flag: Option<&Path>, env_lookup: F) -> PathBuf`

## Key files

- `src/main.rs` — CLI, `RUST_LOG` init, runtime
- `src/lib.rs` — `run`, `prepare_base_dir`, `State`, event loop
