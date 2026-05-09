# Crossbridge

[![CI](https://github.com/vringar/crossbridge/actions/workflows/ci.yml/badge.svg)](https://github.com/vringar/crossbridge/actions/workflows/ci.yml)

Cross-project coordination bridge for [crosslink](https://github.com/forecast-bio/crosslink) repositories.

Crossbridge lets agents working in different crosslink-managed repos on the same machine ask questions of each other and exchange answers, without shared state, network services, or shared filesystem access to each other's databases.

## Architecture

Three programs cooperate over per-user Unix sockets under `$XDG_RUNTIME_DIR/crossbridge/`:

```
                ┌────────────────────────────┐
                │       SUPERVISOR           │
                │ register.socket (per-user) │
                │ no crosslink dep           │
                │ tracks (slug, group) peers │
                │ wipes runtime dir on start │
                └─────┬────────────────┬─────┘
                      │ persistent     │
              ┌───────▼──────┐  ┌──────▼───────┐
              │ SERVER A     │  │ SERVER B     │
              │ owns repo-a  │  │ owns repo-b  │
              │ DB: r/w      │  │ DB: r/w      │
              └──────▲───────┘  └──────▲───────┘
                     │                 │
              ┌──────┴───────┐  ┌──────┴───────┐
              │ client       │  │ client       │
              │ (in repo-b)  │  │ (in repo-a)  │
              └──────────────┘  └──────────────┘
```

- **`crossbridge-supervisor`** — long-running per-user daemon. Listens on `register.socket`, tracks peer groups, notifies servers when peers join/leave. No crosslink dependency. See [`.design/supervisor.md`](.design/supervisor.md).
- **`crossbridge-server`** — one per repo, started manually (e.g. in tmux). Registers `(slug, group)` with the supervisor, owns the repo's `.crosslink/issues.db`, accepts `SubmitIssue` / `SubmitAnswer` over per-peer sockets. See [`.design/server.md`](.design/server.md).
- **`crossbridge-client`** — per-agent CLI invoked from inside a repo's working tree. Lists peers, submits issues to a target peer, posts answers back. See [`.design/client.md`](.design/client.md).

Wire protocol (length-prefixed postcard): [`.design/protocol.md`](.design/protocol.md).

### Why per-user?

The supervisor + server + client all run as the same user, so default Unix-socket permissions Just Work and there's no DynamicUser/group plumbing. Each user that wants crossbridge runs their own supervisor; sockets live under `$XDG_RUNTIME_DIR/crossbridge` (typically `/run/user/$UID/crossbridge`) and are wiped on supervisor startup.

## Usage

```sh
# 1. Start the supervisor (or enable the systemd user unit, see below).
crossbridge-supervisor

# 2. In each repo you want to expose, start a server in a tmux pane:
cd ~/projects/repo-a
crossbridge-server --group amd-psp           # slug derived from origin remote

cd ~/projects/repo-b
crossbridge-server --group amd-psp --slug repo-b

# 3. From any repo, agents use the client:
crossbridge-client peers
crossbridge-client submit --issue 42 --target repo-b
crossbridge-client answer --issue 17
```

Logging on each binary is controlled by `RUST_LOG` (e.g. `RUST_LOG=crossbridge_server=debug`).

### Socket-root resolution

All three binaries share the same precedence:

1. Per-binary CLI flag (`--socket`, `--runtime-root`)
2. `$CROSSBRIDGE_SOCKET_ROOT`
3. `$XDG_RUNTIME_DIR/crossbridge`
4. Compiled-in fallback `/run/crossbridge`

## NixOS deployment

A NixOS module is provided in [`nix/module.nix`](nix/module.nix) for the supervisor as a **systemd user service**:

```nix
{
  imports = [ /path/to/crossbridge/nix/module.nix ];

  services.crossbridge-supervisor = {
    enable = true;
    # socketRoot defaults to "%t/crossbridge" → /run/user/$UID/crossbridge
    # logLevel defaults to "crossbridge_supervisor=info"
  };
}
```

If you want the supervisor running without an active login session, run `loginctl enable-linger $USER` once. Servers and clients are started manually (typically in tmux) — the module deliberately doesn't wrap them, since they're per-repo and per-agent.

## Building

```sh
nix-build                                   # via nix
nix-shell --run "cargo build --release"     # via cargo (sqlite + pkg-config)
```

The Rust toolchain is pinned via [`npins`](https://github.com/andir/npins) — `npins/sources.json` locks a specific `nixpkgs` revision so local dev and CI use the same `rustc` and `clippy`. Bump with `npins update nixpkgs`.

The package installs:

- `crossbridge-supervisor`, `crossbridge-server`, `crossbridge-client` — the three v2 binaries
- `crossbridge` — **legacy** one-shot polling binary; kept for now but not part of the new architecture (see [`crossbridge/README.md`](crossbridge/README.md))
- `crossbridge-request`, `crossbridge-answer` — helper scripts used by the agent skill

## Repo layout

| Path | What |
|---|---|
| `crossbridge-protocol/` | Wire types + framing helpers (lib only) |
| `crossbridge-supervisor/` | Per-user supervisor daemon |
| `crossbridge-server/` | Per-repo server |
| `crossbridge-client/` | Per-agent CLI |
| `crossbridge-e2e/` | Workspace integration tests (real binaries) |
| `crossbridge/` | Legacy polling binary (predates v2) |
| `.design/` | Architecture + per-component specs (source of truth) |
| `nix/module.nix` | NixOS user-mode systemd module |
| `script/` | Helper shell scripts shipped in the package |
| `skill/crossbridge/SKILL.md` | Agent-facing skill: ask / answer / check |

Each crate has its own `README.md` with a thumbnail and a pointer to the relevant design doc.

## Agent integration

Agents use the `/crossbridge` skill (see [`skill/crossbridge/SKILL.md`](skill/crossbridge/SKILL.md)):

- **ask** — send a question to another repo's agent
- **answer** — respond to an inbound request
- **check** — list pending inbound/outbound requests

## License

MIT
