# Crossbridge v2 Implementation Plan

## Agents

### crossbridge-protocol
Convert to Cargo workspace. Implement the shared protocol crate with postcard
message types and length-prefixed framing helpers (both async and sync variants).
See `.design/protocol.md` for full specification.

### crossbridge-supervisor
Implement the supervisor daemon. Listens on a Unix socket for repo server
registrations, tracks peer groups, sends join/leave notifications over
persistent streams. No crosslink dependency.
See `.design/supervisor.md` for full specification.

### crossbridge-server
Implement the per-repo server. Registers with supervisor, creates listening
sockets in peer directories, handles issue submissions and answer routing,
writes to local crosslink database. Materializes binary attachments as
git commits via jj worktrees.
See `.design/server.md` for full specification.

### crossbridge-client
Implement the per-agent CLI. Derives repo slug from git remote, discovers
peers via socket directory listing, submits issues and answers over Unix
sockets. Synchronous (no tokio).
See `.design/client.md` for full specification.
