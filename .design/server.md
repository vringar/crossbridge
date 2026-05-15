# Crossbridge v2: Repo Server

## Summary

`crossbridge-server` runs one instance per repository (manually, e.g. in
tmux). It registers with the supervisor, owns the repo's crosslink database,
maintains per-peer Unix listening sockets at
`/run/crossbridge/<peer-slug>/<own-slug>.socket`, and routes incoming
`SubmitIssue` / `SubmitAnswer` requests from agent clients into the local
crosslink DB. Single-threaded tokio runtime; the only crate in the workspace
that depends on `crosslink`.

## Requirements

- CLI: `crossbridge-server --group <group> [--slug <slug>] [--repo-path <path>]`. `--repo-path` defaults to current directory.
- Slug resolution (precedence): `--slug <slug>` flag > `$CROSSBRIDGE_OWN_SLUG` env var > derive from the `origin` remote of `--repo-path` (`git remote get-url origin`, or `jj git remote list` if `.jj/` exists; strip optional `.git`, take the last path segment). When all three fail, error with a clear `deriving slug from <path>` chain. The flag and env hooks exist for repos with no `origin` remote (fresh local clones, ephemeral worktrees). An empty/whitespace `--slug` value is rejected with `--slug must be a non-empty string`; an empty/whitespace or non-UTF-8 `$CROSSBRIDGE_OWN_SLUG` is silently ignored. The env var name (`CROSSBRIDGE_OWN_SLUG`) and the lookup helper are exported from `crossbridge-protocol` so server and client agree.
- On startup, verify the crosslink DB exists at `<repo-path>/.crosslink/issues.db` before connecting to the supervisor.
- Connect to the supervisor at `/run/crossbridge/register.socket` and send `Register { slug, group }` exactly once.
- After `RegisterResponse::Ack { peers }`, create a Unix listening socket at `/run/crossbridge/<peer-slug>/<own-slug>.socket` for each peer (idempotent mkdir of the parent directory; remove and recreate the socket file if it already exists).
- React to `Notification::PeerJoined { slug }` by adding a listener at `/run/crossbridge/<slug>/<own-slug>.socket`.
- React to `Notification::PeerLeft { slug }` by removing that listener and unlinking the socket file.
- Handle `ClientRequest::Submit(SubmitIssue)`: create a local issue with title/body from the message; apply labels `type:request`, `xb:inbound`, `xb-status:open`, `xb-source:<source_slug>`, `xb-ref:<source_uuid>`; append the answer-instruction footer `\n\n---\nAfter answering, run: \`crossbridge-client answer --issue <id>\`` to the body; respond with `ServerResponse::Ok { issue_id }`.
- Idempotency: before creating, scan for an existing issue carrying label `xb-ref:<source_uuid>` and return that issue's ID instead of creating a duplicate.
- Materialize each `Attachment` in a submission as a fresh jj worktree commit in the local repo (worktree write → `jj describe` → record SHA → cleanup), and add a comment to the created issue referencing the SHA and filenames.
- Handle `ClientRequest::Answer(SubmitAnswer)`: locate the issue carrying `xb-ref:<source_uuid>`; if none, respond with `ServerResponse::Error`. Otherwise copy each comment into the issue prefixed with `[from <source>]`, deduped by content; swap label `xb-status:pending` → `xb-status:resolved`; close the issue; respond `ServerResponse::Ok { issue_id }`.
- Malformed wire messages, DB write errors, and other per-request failures must return `ServerResponse::Error { message }` and close that one connection — they must not crash the server.
- Reconnect to the supervisor on stream loss with exponential backoff (1s, 2s, 4s, …, capped at 60s); while disconnected, drop all peer listener sockets. On reconnect, re-register and rebuild listeners from the new `RegisterAck.peers` and subsequent `PeerJoined`s.
- Single-threaded tokio runtime (`current_thread`) so `rusqlite::Connection` `!Send` is fine; requests handled inline (not spawned).
- Use `crossbridge-protocol` length-prefixed framing for all wire I/O.
- Dependencies: `tokio` (rt-multi-thread or current-thread, net, io, macros, signal), `crossbridge-protocol`, `crosslink`, `tracing`/`tracing-subscriber`, `anyhow`, `clap`. No direct `serde`/`postcard` use beyond what the protocol crate re-exports.

## Acceptance Criteria

- With the supervisor running, two servers in the same group each end up with a listener at `/run/crossbridge/<peer>/<own>.socket`.
- A `SubmitIssue` from a peer creates exactly one local issue with the documented label set and the answer-instruction footer; the response is `Ok { issue_id }` and `issue_id` matches a row in the local crosslink DB.
- Sending a second `SubmitIssue` with the same `source_uuid` returns the existing `issue_id` (no duplicate row, no duplicate side effects).
- A `SubmitAnswer` whose `source_uuid` matches an outbound issue copies the `kind=result` comments back, swaps the status label, and closes the local issue. Repeating the same answer is a no-op (deduplication).
- A `SubmitAnswer` for an unknown `source_uuid` returns `ServerResponse::Error` and no local state changes.
- A submission carrying an `Attachment` produces a new commit in the local repo (visible in `jj log`); the issue gains a comment with the SHA and original filename; the worktree used to materialize it is gone after the request.
- Killing and restarting the supervisor while the server is running causes the server to drop peer listeners, reconnect with backoff, re-register, and re-create listeners — no manual intervention required.
- Killing the server (Ctrl+C) cleans up its listening sockets; the supervisor sends `PeerLeft` to surviving peers.
- A request whose framed payload exceeds 16 MiB is rejected with an error response, not by crashing the server.
- `cargo build -p crossbridge-server` and `cargo test -p crossbridge-server` pass with no warnings.

## Responsibility

One repo server per repository. Owns that repo's crosslink database.
Handles incoming issue submissions and answer routing from peer agents.

## Interface

- Connects to supervisor at `/run/crossbridge/register.socket`
- Creates listening sockets at `/run/crossbridge/<peer-slug>/<own-slug>.socket`
  for each peer
- Receives `ClientRequest` messages from agent CLIs on those sockets
- Writes to its own crosslink database

## CLI

```
crossbridge-server --group <group> [--slug <slug>] [--repo-path <path>]
```

- `--group`: the peer group (e.g. "amd-psp"). Required.
- `--slug`: repo slug. Precedence: flag > `$CROSSBRIDGE_OWN_SLUG` env var > derived from git/jj origin remote.
- `--repo-path`: path to the repo root. Defaults to current directory.
  The crosslink DB is at `<repo-path>/.crosslink/issues.db`.

## Lifecycle

### Startup

1. Derive or validate slug (from origin remote or `--slug` flag)
2. Verify crosslink DB exists at `<repo-path>/.crosslink/issues.db`
3. Connect to supervisor at `/run/crossbridge/register.socket`
4. Send `Register { slug, group }`
5. Receive `RegisterAck { peers }` — list of current peers
6. For each peer in the ack: create listening socket at
   `/run/crossbridge/<peer>/<own-slug>.socket`
7. Enter event loop

### Peer Joined

On receiving `Notification::PeerJoined { slug }`:
1. Create directory `/run/crossbridge/<slug>/` if it doesn't exist
   (the supervisor may have already created it, or the new peer's server
   may create it — idempotent mkdir)
2. Create a Unix socket listener at `/run/crossbridge/<slug>/<own-slug>.socket`
3. Add socket to the event loop's listener set

### Peer Left

On receiving `Notification::PeerLeft { slug }`:
1. Stop listening on `/run/crossbridge/<slug>/<own-slug>.socket`
2. Remove the socket file
3. Remove from the event loop's listener set

### Handling Submissions

When an agent connects to one of this server's listening sockets:

The **socket path** tells the server who the client is:
`/run/crossbridge/<client-repo>/<own-slug>.socket` means the client
is an agent in `<client-repo>`.

#### SubmitIssue

1. Read `ClientRequest::Submit(issue)` from the socket
2. Open local crosslink DB
3. Create issue with:
   - Title and body from the message
   - Labels: `type:request`, `xb:inbound`, `xb-status:open`,
     `xb-source:<source_slug>`, `xb-ref:<source_uuid>`
   - Append to body: instruction for the answering agent:
     `\n\n---\nAfter answering, run: \`crossbridge-client answer --issue <id>\``
4. Send `ServerResponse::Ok { issue_id }` back
5. Close connection

If the submission includes attachments, materialize them as a git commit:
   - Create a jj worktree in the local repo
   - Write each attachment file into the worktree
   - Describe (commit) the worktree to produce a git SHA
   - Add a comment to the issue referencing the SHA and filenames
   - Clean up the worktree

**Idempotency**: Before creating, scan for existing issue with label
`xb-ref:<source_uuid>`. If found, return its ID without creating a
duplicate.

#### SubmitAnswer

1. Read `ClientRequest::Answer(answer)` from the socket
2. Open local crosslink DB
3. Find issue with label `xb-ref:<source_uuid>`
4. If not found: send `ServerResponse::Error`
5. Copy each comment to the issue, prefixed with `[from <source>]`
6. Deduplicate by content comparison
7. Swap `xb-status:pending` → `xb-status:resolved`
8. Close the issue
9. Send `ServerResponse::Ok { issue_id }` back
10. Close connection

## Event Loop

The server multiplexes:
- The supervisor stream (for `PeerJoined` / `PeerLeft` notifications)
- N peer socket listeners (for client connections)

Single-threaded tokio runtime. The `!Send` constraint on
`rusqlite::Connection` is a non-issue on a single-threaded runtime.

```rust
loop {
    tokio::select! {
        notification = read_supervisor_msg(&mut supervisor_stream) => {
            match notification {
                PeerJoined { slug } => add_peer_listener(slug),
                PeerLeft { slug } => remove_peer_listener(slug),
            }
        }
        (peer_slug, conn) = accept_any_peer(&mut listeners) => {
            handle_client_request(peer_slug, conn, &db).await;
        }
    }
}
```

`accept_any_peer` selects across all active `UnixListener`s. Since volume
is low, handling each request inline (not spawned) is fine — one request
at a time per server.

## Supervisor Reconnection

When the supervisor stream hits EOF or error:
1. Log warning
2. Close all peer listener sockets (peers are unknown without supervisor)
3. Retry connecting to `/run/crossbridge/register.socket` with exponential
   backoff (1s, 2s, 4s, 8s, ..., capped at 60s)
4. On successful reconnect: re-register, receive fresh peer list, recreate
   listener sockets

## Slug Resolution

Precedence (matching `crossbridge-client`):

1. **`--slug <slug>` flag** — explicit override. Trimmed; empty/whitespace
   rejected with `--slug must be a non-empty string`.
2. **`$CROSSBRIDGE_OWN_SLUG` env var** — set once in the environment.
   Trimmed; empty/whitespace or non-UTF-8 values are silently ignored so
   resolution falls through.
3. **Derive from `--repo-path`'s `origin` remote**:

   ```
   git remote get-url origin
   # → git@github.com:AMD-PSP/firmware.git → firmware
   # → https://github.com/AMD-PSP/firmware  → firmware
   # → https://github.com/AMD-PSP/firmware.git → firmware
   ```

   Strip trailing `.git`, take the last path component. If `.jj/` exists,
   use `jj git remote list` and parse the `origin` entry instead.

The constant `OWN_SLUG_ENV` and the helper `own_slug_from_env` live in
`crossbridge-protocol` so all three binaries (supervisor, server, client)
agree on the env var name. The flag-vs-env-vs-derive composition itself
is duplicated per binary (`crossbridge-server::slug::resolve_slug` and
`crossbridge-client::slug::resolve_own_slug`) because each binary's
derive entry point has a slightly different name and signature.

## Dependencies

- `tokio` (rt, net, io, macros, signal) — event loop, sockets
- `crossbridge-protocol` — message types and framing
- `crosslink` — database access for issue creation
- `tracing` + `tracing-subscriber` — structured logging
- `anyhow` — error handling
- `clap` — CLI argument parsing

## Error Handling

| Scenario | Behavior |
|---|---|
| Supervisor gone | Reconnect with backoff, close peer sockets meanwhile |
| Can't create peer socket | Log error, skip that peer (try again on next PeerJoined) |
| Malformed client message | Send `ServerResponse::Error`, close connection |
| DB write fails | Send `ServerResponse::Error`, close connection |
| Duplicate issue (idempotency) | Return existing issue ID, no error |
| Client disconnects mid-request | Log, clean up, continue |
| Socket file already exists (stale) | Remove and recreate |
