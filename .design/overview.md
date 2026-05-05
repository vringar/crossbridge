# Crossbridge v2: Architecture Overview

## Motivation

Crossbridge v1 is a one-shot CLI with a static TOML config listing all repos.
This creates three problems:

1. **Static config per machine** — the config contains local paths that differ
   per machine, can't be committed to a public repo without leaking paths or
   requiring age encryption.
2. **No sandbox isolation** — the single bridge binary needs read/write access
   to ALL repo databases. Claude agents run in bubblewrap sandboxes with
   isolated `/run` and HOME tmpfs; a single process accessing all DBs breaks
   this model.
3. **Polling latency** — the 30s timer means messages take up to 30s to route.

## Architecture: Three Components

Crossbridge v2 splits into three programs:

```
┌──────────────────────────────────────────────────────────┐
│                     SUPERVISOR                            │
│  Listens: /run/crossbridge/register.socket                │
│  No crosslink dependency. No domain awareness.            │
│  Manages socket topology for peer groups.                 │
│  On peer join: notifies same-group servers                │
│  On peer leave: notifies survivors, cleans up dirs        │
│  On own restart: wipes /run/crossbridge/*, fresh start    │
└──────────────┬──────────────────────────────┬────────────┘
               │ persistent stream            │
       ┌───────▼───────┐             ┌────────▼──────┐
       │ REPO SERVER A │             │ REPO SERVER B │
       │ (tmux, manual)│             │ (tmux, manual)│
       │               │             │               │
       │ Owns: repo-a  │             │ Owns: repo-b  │
       │ DB: read/write│             │ DB: read/write│
       │               │             │               │
       │ On "B joined":│             │ On "A joined":│
       │  creates       │             │  creates       │
       │  /run/cross-  │             │  /run/cross-  │
       │  bridge/repo-b│             │  bridge/repo-a│
       │  /repo-a.sock │             │  /repo-b.sock │
       │               │             │               │
       │ On "B left":  │             │  On "A left": │
       │  deletes       │             │  deletes       │
       │  that socket   │             │  that socket   │
       └───────▲───────┘             └────────▲──────┘
               │                              │
    ┌──────────┴──────────┐        ┌──────────┴──────────┐
    │ CROSSBRIDGE-CLIENT  │        │ CROSSBRIDGE-CLIENT  │
    │ (in repo-b sandbox) │        │ (in repo-a sandbox) │
    │                     │        │                     │
    │ Sees: /run/cross-  │        │ Sees: /run/cross-  │
    │ bridge/repo-b/      │        │ bridge/repo-a/      │
    │   repo-a.socket     │        │   repo-b.socket     │
    │                     │        │                     │
    │ crossbridge-client  │        │ crossbridge-client  │
    │   peers             │        │   submit --issue 16 │
    │   → ls on dir       │        │   --target repo-b   │
    └─────────────────────┘        └─────────────────────┘
```

### Supervisor

- Stateless systemd service (or manual)
- Exposes `/run/crossbridge/register.socket`
- Maintains persistent streams to each registered repo server
- Tracks groups internally; repo servers only hear about their own group
- On repo server disconnect: notifies survivors, removes the dead server's
  socket directory under each peer
- On own restart: wipes `/run/crossbridge/*`, all repo servers must re-register
- **No crosslink dependency**. Completely unaware of what it coordinates.

### Repo Server (per-repo)

- Started manually in tmux (not systemd)
- Registers with supervisor providing slug + group
- Creates listening sockets in peer directories when notified of peers
- Receives issue submissions from agents via those sockets
- Writes received issues into its own crosslink database
- Deletes peer sockets when notified of peer departure
- **Has crosslink dependency** for DB writes

### Client CLI (per-agent, in sandbox)

- Determines own repo slug from git/jj origin remote (GitHub assumed:
  `github.com/<org>/<repo>` → slug is `<repo>`)
- Discovers peers by listing sockets in `/run/crossbridge/<own-slug>/`
- Submits issues by connecting to the target's socket
- Reads the local issue from crosslink DB, serializes, sends over socket
- Verifies the local issue exists before sending (deterministic failure)
- **Has crosslink dependency** for DB reads

## Repo Slug Derivation

All repos are assumed to be on GitHub. The slug is derived from the origin
remote URL:

```
git@github.com:AMD-PSP/firmware.git  →  firmware
https://github.com/AMD-PSP/tools     →  tools
```

The CLI extracts this from `git remote get-url origin` or `jj git remote list`.
The repo server takes it as a CLI argument (or derives it the same way).

## Sandbox Integration

Each agent sandbox (bubblewrap) gets one additional bind mount:

```
--bind /run/crossbridge/<slug> /run/crossbridge/<slug>
```

The register socket (`/run/crossbridge/register.socket`) is NOT mounted into
sandboxes — only repo servers (running outside sandboxes) can register.

Peer sockets appear/disappear in the mounted directory as peers join/leave.
The agent sees this in real-time because it's a bind mount to the host path.

## Answer Routing

Answers flow back through the same socket mechanism, triggered by the agent:

1. **Primary**: The inbound issue description includes an instruction:
   "After answering, run: `crossbridge-client answer --issue <id>`"
2. **Safety net**: A PostToolUse Claude Code hook detects issue closes with
   crossbridge labels and fires the CLI automatically
3. **Fallback**: The repo server can optionally poll its own DB for
   `xb-status:answered` issues (reintroduces timer-based logic, last resort)

## Project Structure

Cargo workspace with four crates:

```
crossbridge/
├── Cargo.toml              # workspace root
├── crossbridge-protocol/   # shared message types, postcard serde
├── crossbridge-supervisor/ # supervisor binary
├── crossbridge-server/     # repo server binary
├── crossbridge-client/     # agent CLI binary
├── nix/                    # NixOS packaging
└── skill/                  # agent skill documentation
```

## File Sharing via Commit Materialization

Binary artifacts (coverage maps, fuzzing outputs) are sent inline as
`Attachment` payloads (postcard `Vec<u8>`) over the socket. The receiving
repo server materializes them as git commits in the target repo:

1. Client reads the binary file, includes it in the submission message
2. Server receives the bytes, creates a jj worktree in its own repo
3. Server writes the file into the worktree and describes it (commits)
4. This produces a git SHA in the target repo
5. The SHA is referenced in the issue so the target agent can access the file

Source and target repos are fully isolated — a SHA from the source repo
is meaningless in the target. The binary data must be materialized as a
fresh commit in the target repo. The 16 MiB frame limit accommodates
typical binary artifacts.

## Differences from v1

| Aspect | v1 | v2 |
|---|---|---|
| Configuration | Static TOML per machine | Dynamic registration |
| Repo discovery | Manual config | Supervisor + groups |
| IPC | Direct SQLite access | Unix sockets + postcard |
| Invocation | systemd timer, 30s | Long-running servers |
| Sandbox support | None | Native (bind mount per repo) |
| Latency | Up to 30s | Immediate |
| Crash recovery | Idempotency on next timer | Supervisor lifecycle mgmt |
| Dependencies | crosslink only | crosslink (server/client), tokio |
| Complexity | ~730 LOC, 1 binary | 4 crates, 3 binaries |
