# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- End-to-end integration test + unify CROSSBRIDGE_SOCKET_ROOT across all three binaries (#17)
- crossbridge-client: Implement the per-agent CLI. See .design/client.md for full spec. SYNCHRONOUS (no tokio). Derives repo slug from git/jj origin remote (strip .git, take last path component). Three subcommands: (1) peers - list *.socket files in /run/crossbridge/<own-slug>/, (2) submit --issue <id> --target <slug> - read issue from local crosslink DB, connect to peer socket, send ClientRequest::Submit, on success update local labels (xb:outbound, xb-status:pending, xb-ref:<target-uuid>), (3) answer --issue <id> - read inbound issue, extract xb-source and xb-ref labels, collect result comments, send ClientRequest::Answer, on success mark xb-status:answered and close. Uses std::os::unix::net::UnixStream with sync framing helpers from crossbridge-protocol. Dependencies: crossbridge-protocol, crosslink, clap, anyhow. NO tokio. (#8)
- crossbridge-server: Implement the per-repo server. See .design/server.md for full spec. Registers with supervisor (sends Register, receives RegisterAck with peer list), creates listening sockets at /run/crossbridge/<peer-slug>/<own-slug>.socket for each peer. Handles PeerJoined/PeerLeft notifications. Accepts ClientRequest (Submit/Answer) from agents on those sockets. SubmitIssue: creates issue in local crosslink DB with proper labels, handles attachments via jj worktree materialization, idempotent via xb-ref check. SubmitAnswer: finds source issue by UUID, copies comments, swaps status to resolved, closes. Single-threaded tokio. Supervisor reconnection with exponential backoff. CLI: crossbridge-server --group <group> [--slug <slug>] [--repo-path <path>]. Dependencies: tokio, crossbridge-protocol, crosslink, tracing, tracing-subscriber, anyhow, clap. (#7)
- crossbridge-supervisor: Implement the supervisor daemon. See .design/supervisor.md for full spec. Listens on Unix socket for repo server registrations, tracks peer groups in-memory (HashMap<String, HashMap<String, Stream>>), sends join/leave notifications over persistent streams. Single-threaded tokio event loop. Creates/removes /run/crossbridge/<slug>/ directories. Handles registration (validate unique slug), peer departure (notify survivors, cleanup dirs), and own restart (wipe state). CLI: crossbridge-supervisor [--socket path]. Dependencies: tokio, crossbridge-protocol (workspace dep), tracing, tracing-subscriber, anyhow. NO crosslink dependency. (#6)
- crossbridge-protocol: Convert the project to a Cargo workspace and implement the shared protocol crate. See .design/protocol.md for the full spec. This includes: workspace Cargo.toml at root, crossbridge-protocol/ crate with all message types (Register, RegisterResponse, Notification, SupervisorMessage, ClientRequest, SubmitIssue, SubmitAnswer, AnswerComment, Attachment, ServerResponse), postcard serde, and length-prefixed framing helpers (both async with tokio AsyncRead/AsyncWrite and sync with std Read/Write). Max frame size 16 MiB. Dependencies: serde, postcard, tokio (for async helpers only). No crosslink dependency. (#5)
- test sandbox config 3 (L3)
- test sandbox config 2 (L2)
- test sandbox config (L1)
- Convert to Cargo workspace, implement shared protocol crate with postcard message types and framing helpers (#4)

### Fixed

### Changed
- Quality pass: clippy pedantic clean workspace-wide (#33)
- Quality pass: clippy pedantic cleanup in crossbridge-e2e (#32)
- Quality pass: clippy pedantic cleanup in crossbridge-client (#31)
- Quality pass: clippy pedantic cleanup in crossbridge-server (#30)
- Quality pass: clippy pedantic cleanup in crossbridge-supervisor (#29)
- Quality pass: clippy pedantic cleanup in crossbridge-protocol (#28)
- Quality pass: clippy pedantic cleanup in crossbridge (v1 bin) (#27)
- Quality pass: clippy pedantic clean workspace-wide (#25)
- Test (#26)
- Quality pass: clippy pedantic cleanup in crossbridge-e2e (#23)
- Quality pass: clippy pedantic cleanup in crossbridge-client (#22)
- Quality pass: clippy pedantic cleanup in crossbridge-server (#21)
- Quality pass: clippy pedantic cleanup in crossbridge-supervisor (#20)
- Quality pass: clippy pedantic cleanup in crossbridge-protocol (#19)
- Quality pass: clippy pedantic cleanup in crossbridge (v1 bin) (#18)
- Quality pass: clippy pedantic cleanup in crossbridge (v1 bin) (#24)
- Convert to Cargo workspace, implement shared protocol crate with postcard message types and framing helpers (#3)
- Package crossbridge as systemd service for vringar/nixos-setup (#2)
