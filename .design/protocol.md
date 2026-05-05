# Crossbridge v2: Wire Protocol

## Serialization

All messages use **postcard** (plain, not postcard-rpc) for serialization.
Postcard is a compact, non-self-describing binary format built on serde.

No postcard-rpc — the RPC framework's COBS framing and dispatch macros are
designed for embedded MCU↔PC communication and would over-engineer the
3-4 message types we need while requiring a custom Unix socket transport
implementation anyway.

## Framing

Length-prefixed framing over Unix sockets:

```
┌──────────────┬──────────────────────┐
│ length: u32  │ postcard payload     │
│ (big-endian) │ (length bytes)       │
└──────────────┴──────────────────────┘
```

4-byte big-endian length prefix followed by the postcard-serialized message.
Maximum message size: 16 MiB (enforced by both sides to prevent OOM from
malformed frames). This limit is generous — issue metadata is typically under
100 KiB, and the 16 MiB ceiling accommodates future binary file transfers.

## Protocol: Supervisor ↔ Repo Server

Communication happens over the persistent Unix stream between a repo server
and the supervisor. The repo server connects to
`/run/crossbridge/register.socket`.

### Registration (repo server → supervisor)

```rust
#[derive(Serialize, Deserialize)]
struct Register {
    slug: String,     // e.g. "firmware"
    group: String,    // e.g. "amd-psp"
}
```

Sent once immediately after connecting. The slug must be unique within
the group. If a slug is already registered, the supervisor rejects with
a `RegisterNack`.

### Registration Acknowledgment (supervisor → repo server)

```rust
#[derive(Serialize, Deserialize)]
enum RegisterResponse {
    Ack { peers: Vec<String> },     // slugs of current same-group peers
    Nack { reason: String },        // e.g. "slug already registered"
}
```

Sent once in response to `Register`. The `peers` list contains all
currently registered repo servers in the same group (excluding self).

### Peer Notifications (supervisor → repo server, ongoing)

```rust
#[derive(Serialize, Deserialize)]
enum Notification {
    PeerJoined { slug: String },
    PeerLeft { slug: String },
}
```

Sent over the persistent stream whenever a same-group peer registers or
disconnects. The repo server is expected to:
- On `PeerJoined`: create a listening socket at
  `/run/crossbridge/<peer-slug>/<own-slug>.socket`
- On `PeerLeft`: delete its socket from
  `/run/crossbridge/<peer-slug>/<own-slug>.socket`

### Message Discrimination

Both `RegisterResponse` and `Notification` travel supervisor → server on
the same stream. They are distinguished by a one-byte message tag prefix
before the postcard payload:

```rust
#[derive(Serialize, Deserialize)]
enum SupervisorMessage {
    RegisterResponse(RegisterResponse),
    Notification(Notification),
}
```

The outer enum is itself postcard-serialized, which handles discrimination
via postcard's enum variant encoding (varint tag).

## Protocol: Client ↔ Repo Server

Communication happens over per-peer Unix sockets. An agent in repo-b
connects to `/run/crossbridge/repo-b/repo-a.socket` to submit to repo-a.

### Client Request

```rust
#[derive(Serialize, Deserialize)]
enum ClientRequest {
    Submit(SubmitIssue),
    Answer(SubmitAnswer),
}

#[derive(Serialize, Deserialize)]
struct SubmitIssue {
    title: String,
    body: String,
    labels: Vec<String>,
    source_slug: String,
    source_uuid: String,
    attachments: Vec<Attachment>,
}

#[derive(Serialize, Deserialize)]
struct SubmitAnswer {
    source_uuid: String,
    comments: Vec<AnswerComment>,
    attachments: Vec<Attachment>,
}

#[derive(Serialize, Deserialize)]
struct AnswerComment {
    content: String,
    kind: String,       // "result", "note", etc.
}

#[derive(Serialize, Deserialize)]
struct Attachment {
    filename: String,
    data: Vec<u8>,
}
```

### Server Response

```rust
#[derive(Serialize, Deserialize)]
enum ServerResponse {
    Ok { issue_id: i64 },
    Error { message: String },
}
```

### Connection Lifecycle

Each client request is a single connection:
1. Client connects to peer socket
2. Client sends one framed `ClientRequest`
3. Server reads, processes, sends one framed `ServerResponse`
4. Both sides close

No connection pooling, no keep-alive. Volume is low (handful of requests
per hour) and Unix socket connect is ~microseconds.

## File Sharing via Commit Materialization

Binary artifacts (coverage maps, fuzzing outputs) are sent inline as
`Attachment` payloads over the socket. The receiving repo server
materializes them as git commits in the target repo:

1. Client reads binary file, includes it as an `Attachment` in the request
2. Server receives the attachment bytes over the socket
3. Server creates a jj worktree in the target repo
4. Server writes the file into the fresh worktree
5. Server describes (commits) the worktree, producing a git SHA
6. Server references the SHA in the issue (e.g. as a comment or label)
7. Server cleans up the worktree

This keeps source and target repos fully isolated — a SHA from the source
repo is meaningless in the target repo, so the binary data must be
materialized as a new commit in the target. The receiving agent can then
access the file via `jj show <sha>` or `git show <sha>:<filename>`.

The 16 MiB frame limit accommodates typical binary artifacts (coverage
maps, small binaries). For larger transfers, chunked streaming can be
added later.

## Label Protocol (carried in messages)

The label protocol from v1 is largely preserved, but labels are now set by
the repo server when it processes incoming submissions rather than by a
bridge scanning databases.

When a repo server receives a `SubmitIssue`, it creates the issue in its
local database with:
- `type:request`
- `xb:inbound`
- `xb-status:open`
- `xb-source:<source_slug>`
- `xb-ref:<source_uuid>`

The source-side labels (`xb:outbound`, `xb-status:pending`, `xb-ref:<target-uuid>`)
are set by the client CLI before/after the socket call.

When a repo server receives a `SubmitAnswer`, it:
- Finds the source issue by `source_uuid` (via `xb-ref` label)
- Copies comments to the source issue
- Swaps `xb-status:pending` → `xb-status:resolved`
- Closes the source issue

## Shared Protocol Crate

All message types live in `crossbridge-protocol`:

```
crossbridge-protocol/
├── Cargo.toml
└── src/
    └── lib.rs     # all types + framing helpers
```

Dependencies: `serde`, `postcard`. No crosslink, no tokio.

The crate also provides framing helpers:

```rust
pub async fn write_message<W: AsyncWrite, T: Serialize>(w: &mut W, msg: &T) -> Result<()>;
pub async fn read_message<R: AsyncRead, T: DeserializeOwned>(r: &mut R) -> Result<T>;
```

And synchronous variants for the client CLI (which doesn't need async):

```rust
pub fn write_message_sync<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<()>;
pub fn read_message_sync<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T>;
```
