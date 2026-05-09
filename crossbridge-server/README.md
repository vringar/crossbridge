# crossbridge-server

Per-repo server. Owns one repo's crosslink DB, registers itself with the
supervisor under a `(slug, group)` pair, and accepts `SubmitIssue` /
`SubmitAnswer` requests over per-peer Unix sockets attached to slug
subdirectories under the runtime root.

**Spec:** [`.design/server.md`](../.design/server.md)

## Binary

```
crossbridge-server --group <GROUP> [--slug <SLUG>] [--repo-path <DIR>] [--runtime-root <DIR>]
```

- `--group` (required) — peer group name (e.g. `amd-psp`)
- `--slug` — repo slug; **derived from the origin remote** of `--repo-path` if omitted (`slug::derive_from_repo`)
- `--repo-path` — defaults to `.`
- `--runtime-root` — overrides the resolution chain below

**Runtime-root resolution:** `--runtime-root` > `$CROSSBRIDGE_SOCKET_ROOT` > `$XDG_RUNTIME_DIR/crossbridge` > compiled-in fallback `/run/crossbridge`.

## Public modules (lib)

- `paths` — `resolve_runtime_root`, `SocketLayout`
- `run` — `ServerConfig`, top-level orchestration
- `slug` — slug derivation from git remotes
- `supervisor` — register-socket client / reconnect logic
- `listeners` — per-peer socket binding + accept
- `handler` — request dispatch (SubmitIssue, SubmitAnswer)
- `attachment` — attachment storage helpers

## Key files

- `src/main.rs` — CLI, runtime bootstrap
- `src/run.rs` — `run` entrypoint that wires everything together
- `src/handler.rs` — request handlers (the meat)
