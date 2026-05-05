# Crossbridge v2: Supervisor

## Summary

`crossbridge-supervisor` is a stateless, domain-agnostic tokio service that
coordinates the socket topology for peer groups under `/run/crossbridge/`.
Repo servers connect to the register socket, send `Register { slug, group }`,
and stay attached to a persistent stream over which the supervisor delivers
`PeerJoined` / `PeerLeft` notifications for the same group. The supervisor
has no `crosslink` dependency and no awareness of what is being coordinated.

## Requirements

- Listen on a configurable Unix socket (default `/run/crossbridge/register.socket`); a stale file at the path must be removed and rebound on startup.
- On startup, wipe `/run/crossbridge/*` so no stale sockets or directories from a prior run leak through.
- Accept persistent connections; each repo server sends exactly one `Register { slug, group }` immediately after connecting.
- Reject duplicate slugs within a group with `RegisterResponse::Nack { reason }` and close the connection; otherwise reply `RegisterResponse::Ack { peers }` containing the slugs of all currently registered same-group peers (excluding self).
- Maintain in-memory topology as `group → (slug → connection)`; the same slug may exist in different groups.
- For each accepted registration, create `/run/crossbridge/<slug>/` (idempotent mkdir).
- After `Ack`, send `Notification::PeerJoined { slug }` to every other same-group peer over their persistent streams.
- Treat stream EOF or read error from a registered server as departure: remove from the group map, send `Notification::PeerLeft { slug }` to surviving same-group peers, and remove `/run/crossbridge/<departed-slug>/` and any sockets within it.
- If sending a notification to a peer fails, treat that peer as departed too (recursive cleanup is fine).
- Never read application data from a registered server beyond the initial `Register`; unexpected data is logged and ignored.
- Run on a single-threaded tokio runtime using `tokio::select!` to multiplex `listener.accept()` and per-connection EOF detection.
- All wire messages use the length-prefixed framing helpers from `crossbridge-protocol`.
- No `crosslink` dependency. Dependencies limited to `tokio`, `crossbridge-protocol`, `tracing`/`tracing-subscriber`, `anyhow`, `clap`.

## Acceptance Criteria

- Two repo servers registering with the same `group` each see the other in `Ack.peers` (whichever connected second) or via `PeerJoined` (the first); servers in different groups never appear in each other's notifications.
- A second registration with a slug already taken in the same group receives `Nack` and the connection is closed; the original registration is unaffected.
- Killing a registered server causes survivors in the same group to receive `PeerLeft { slug }` and `/run/crossbridge/<dead-slug>/` is removed (including any socket files other servers placed inside it).
- Restarting the supervisor while servers are connected wipes `/run/crossbridge/*` on startup; servers reconnect (their own logic) and re-register cleanly.
- Starting the supervisor when a stale socket file exists at the listen path succeeds (the stale file is removed and the supervisor rebinds).
- Integration tests cover: register/ack with empty peers, register/ack with existing peers, duplicate-slug nack, peer-joined fanout, peer-left fanout on EOF, supervisor-restart wipe.
- `cargo build -p crossbridge-supervisor` and `cargo test -p crossbridge-supervisor` pass with no warnings.

## Responsibility

The supervisor manages the socket topology for peer groups. It has **no
crosslink dependency** and is completely unaware of what it coordinates.
It could equally well coordinate any set of services that want peer-aware
socket directories.

## Interface

- Listens on `/run/crossbridge/register.socket`
- Accepts persistent connections from repo servers
- Maintains in-memory state: `HashMap<String, HashMap<String, Stream>>`
  mapping `group → (slug → connection)`
- Sends peer notifications over persistent streams

## Lifecycle

### Startup

1. Create `/run/crossbridge/` directory if it doesn't exist
2. Remove any stale contents (previous supervisor's socket dirs)
3. Bind and listen on `/run/crossbridge/register.socket`
4. Enter event loop

### Registration

When a repo server connects:
1. Read `Register { slug, group }` message
2. Validate slug is unique within the group
3. If duplicate: send `RegisterNack`, close connection
4. Create directory `/run/crossbridge/<slug>/` for the new server's
   agent clients to discover peers in
5. Send `RegisterAck { peers }` with current same-group peer slugs
6. For each existing same-group peer: send `PeerJoined { slug: new_slug }`
7. Store the connection in the group map

### Peer Departure

When a repo server's stream hits EOF or error:
1. Remove from group map
2. For each surviving same-group peer: send `PeerLeft { slug: departed }`
3. Remove `/run/crossbridge/<departed-slug>/` directory and all contents
   (the departed server's listening sockets are gone anyway, and this
   cleans up any sockets other servers created in this directory)

Note: The supervisor also deletes sockets that OTHER servers created
in the departed server's directory. This is correct — those sockets were
for agents in the departed repo to reach peers, and with the repo gone
there are no agents to use them. The surviving servers will also get
`PeerLeft` and delete their own socket files from the departed directory,
but the supervisor's directory removal handles the race where a server
hasn't cleaned up yet.

### Own Restart

The supervisor is stateless across restarts. On startup it wipes
`/run/crossbridge/*` because:
- Old sockets are stale (the listening processes are dead)
- Old directories reference dead servers
- Repo servers must detect the supervisor disconnect (stream EOF) and
  re-register

Repo servers should implement reconnection logic: when the supervisor
stream breaks, retry connecting to `/run/crossbridge/register.socket`
with backoff.

## Event Loop

The supervisor multiplexes:
- Accepting new connections on the register socket
- Reading from N existing repo server streams (to detect EOF)
- Sending notifications to repo server streams

This is a natural fit for single-threaded tokio:

```rust
loop {
    tokio::select! {
        conn = listener.accept() => {
            handle_registration(conn);
        }
        (slug, result) = next_stream_event(&mut connections) => {
            match result {
                Err(_) | Ok(0) => handle_departure(slug),
                _ => {} // unexpected data from server, ignore or log
            }
        }
    }
}
```

The supervisor never reads application data from repo servers after
registration — it only monitors for EOF. If a repo server sends unexpected
data, the supervisor logs a warning and ignores it.

## CLI

```
crossbridge-supervisor [--socket /run/crossbridge/register.socket]
```

Single optional flag for the socket path. Default is
`/run/crossbridge/register.socket`.

## Dependencies

- `tokio` (rt, net, io, macros) — async event loop, Unix sockets
- `crossbridge-protocol` — message types and framing
- `tracing` + `tracing-subscriber` — structured logging
- `anyhow` — error handling

**No crosslink.** The supervisor is domain-agnostic.

## Security

- The register socket is at `/run/crossbridge/register.socket`, outside
  any agent sandbox. Only repo servers (running unsandboxed) can register.
- The supervisor creates directories under `/run/crossbridge/` with
  permissions that allow the sandboxed agents to read socket files.
- The supervisor itself runs as a regular user (the same user running
  the repo servers and agents).

## Error Handling

| Scenario | Behavior |
|---|---|
| Duplicate slug in group | Send `RegisterNack`, close connection |
| Repo server stream error | Treat as departure, notify peers |
| Can't create directory | Log error, send `RegisterNack` |
| Can't send notification to peer | Treat that peer as departed too |
| Own socket already exists | Remove stale socket, rebind |
