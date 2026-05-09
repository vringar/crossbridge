# crossbridge-protocol

Wire protocol types and framing for crossbridge. Pure data + (de)serialization;
no I/O policy.

**Spec:** [`.design/protocol.md`](../.design/protocol.md)

## What lives here

- **Framing** (`framing.rs`) — length-prefixed postcard frames, `MAX_FRAME_SIZE`
  cap, sync (`std::io`) and async (`tokio::io`) helpers:
  `read_message` / `write_message` / `read_message_sync` / `write_message_sync`.
- **Socket-root resolution** (`lib.rs`) — single source of truth used by all
  three binaries:
  - `default_socket_root(env_lookup)` — precedence:
    `$CROSSBRIDGE_SOCKET_ROOT` > `$XDG_RUNTIME_DIR/crossbridge` >
    compiled-in fallback `DEFAULT_SOCKET_ROOT` (`/run/crossbridge`)
  - Constants: `SOCKET_ROOT_ENV`, `XDG_RUNTIME_DIR_ENV`, `DEFAULT_SOCKET_ROOT`
- **Message types** (`lib.rs`):
  - Supervisor↔server: `Register`, `RegisterResponse`, `Notification`,
    `SupervisorMessage`
  - Client→server: `ClientRequest` (`SubmitIssue` / `SubmitAnswer`),
    with `AnswerComment`, `Attachment`
  - Server→client: `ServerResponse`

## Used by

`crossbridge-supervisor`, `crossbridge-server`, `crossbridge-client`,
`crossbridge-e2e`. Library only — no binary.
