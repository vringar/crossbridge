# Crossbridge v2: End-to-End Integration Test

## Summary

Add a workspace-level integration test that wires up real `crossbridge-supervisor`,
two real `crossbridge-server` instances, and the real `crossbridge-client` binary
to verify the full bidirectional flow: an issue submitted from repo-a's agent
reaches repo-b's crosslink DB as an inbound issue, and an answer issued from
repo-b's agent flows back to close out repo-a's outbound issue with the result
comments attached.

The existing per-crate integration tests each stub out the components on the
other side of their crate boundary (the client tests use a thread-bound mock
server; the server's "E2E" test uses a fake supervisor and a raw socket
client; the supervisor tests are pure topology). None exercise the full
supervisor + 2 servers + 2 clients topology, and none verify that
`xb-status:answered` on the responder lines up with `xb-status:resolved` and
issue closure on the originator across two separate crosslink databases.

This is the test that will catch wire/label-contract drift between the four
crates as they evolve.

## Requirements

### Unified runtime-root override (production code change)

Today the runtime root is configured inconsistently across the three binaries:
the supervisor takes `--register-socket <path>` (CLI only), the server takes
`--runtime-root <path>` (CLI only), and the client reads
`CROSSBRIDGE_SOCKET_ROOT` (env only). This makes it impossible to point all
three at a single tempdir without a different mechanism per binary.

Unify on a single env var, **`CROSSBRIDGE_SOCKET_ROOT`** (the existing
client-side name — keep the name, expand its scope), with the following
semantics in **all three** binaries:

- Resolution precedence: CLI flag > env var > compiled-in default
  (`/run/crossbridge`).
- Supervisor: when `CROSSBRIDGE_SOCKET_ROOT` is set and `--register-socket`
  is not, the register socket path becomes `$CROSSBRIDGE_SOCKET_ROOT/register.socket`.
  The existing `--register-socket` flag continues to work and overrides the env.
- Server: when `CROSSBRIDGE_SOCKET_ROOT` is set and `--runtime-root` is not,
  the runtime root becomes `$CROSSBRIDGE_SOCKET_ROOT`. The existing
  `--runtime-root` flag continues to work and overrides the env.
- Client: behavior is unchanged. Already reads `CROSSBRIDGE_SOCKET_ROOT`
  via `crossbridge_client::SOCKET_ROOT_ENV` (`crossbridge-client/src/lib.rs:17`).
- Export the env-var name as a public constant in `crossbridge-protocol`
  (e.g. `pub const SOCKET_ROOT_ENV: &str = "CROSSBRIDGE_SOCKET_ROOT";`) so
  all three binaries reference one definition. Migrate
  `crossbridge_client::SOCKET_ROOT_ENV` to re-export the protocol-level
  constant (or replace the local definition outright).
- Add (or extend) unit tests in each binary's CLI parsing layer covering all
  three precedence cases: flag-only, env-only, both (flag wins), neither
  (default).

### Test crate

- New workspace member crate `crossbridge-e2e` (root-level, matching the existing
  layout convention from `.design/overview.md`). The crate is empty apart from
  a stub `src/lib.rs` and integration tests under `tests/`.
- `crossbridge-e2e` must `dev-depend` on `crossbridge-supervisor`,
  `crossbridge-server`, `crossbridge-client`, `crossbridge-protocol`, and
  `crosslink` so that `CARGO_BIN_EXE_*` env vars are populated and DB inspection
  is possible from the test.
- Add `crossbridge-e2e` to `[workspace.members]` in the root `Cargo.toml`.
- Tests configure all three binaries via the unified env var only — NO CLI
  flags needed for the runtime root once the env-var support above is in
  place. Set `CROSSBRIDGE_SOCKET_ROOT=<tmp>` on each spawned process and
  on each client invocation. Real `/run/crossbridge` must never be touched.
- All Unix sockets must live under a path short enough for `sockaddr_un.sun_path`
  (~108 bytes on Linux). Reuse the `ShortTempDir` pattern from
  `crossbridge-server/tests/common/mod.rs` (copy it into the new crate's
  `tests/common/mod.rs` — keep it simple, no shared crate needed).
- Each test sets up two minimal git repos (origin = `git@example.com:org/<slug>.git`)
  with their own `.crosslink/issues.db` initialized via `crosslink::db::Database::open`.
  Mirror the fixture pattern from `crossbridge-client/tests/end_to_end.rs:39`.
- Spawn the supervisor, then both servers (in either order — registration is
  the synchronization point). Wait deterministically for both servers' peer
  listener sockets to appear at `<tmp>/<peer>/<own>.socket` before invoking any
  client. **No bare sleeps as synchronization** — poll for the socket file with
  a hard timeout (5–10s) and a short interval (10–20ms), and `panic!` with a
  diagnostic if the timeout elapses.
- Drive the test using the real `crossbridge-client` binary
  (`env!("CARGO_BIN_EXE_crossbridge-client")`), invoked with
  `current_dir(<repo-a-root>)` / `current_dir(<repo-b-root>)` and
  `CROSSBRIDGE_SOCKET_ROOT` set to the tempdir.
- On test completion (success or panic), all child processes
  (supervisor + 2 servers) must be killed and reaped. Use a `Drop` guard
  around the `Child` handles. Test must not leave processes behind even on
  assertion failure.
- Test must be runnable repeatedly without manual cleanup — tempdir lifecycle
  handles socket and DB removal.

## Scenario (`tests/round_trip.rs`)

A single test, `submit_then_answer_round_trip`, executes this flow:

1. **Setup**:
   - Create `<tmp>/runtime/` (the runtime root for all three binaries).
   - Create `<tmp>/repo-a/` and `<tmp>/repo-b/`, each a git repo with origin
     `git@example.com:org/repo-a.git` (and `repo-b.git`) and an empty crosslink
     DB at `.crosslink/issues.db`.
   - Spawn `crossbridge-supervisor` with `CROSSBRIDGE_SOCKET_ROOT=<runtime>` in
     its environment (no CLI flags needed once the unified env var lands).
   - Wait for `<runtime>/register.socket` to exist.
   - Spawn `crossbridge-server --group test --slug repo-a --repo-path <tmp>/repo-a`
     with `CROSSBRIDGE_SOCKET_ROOT=<runtime>` in its environment.
   - Spawn `crossbridge-server --group test --slug repo-b --repo-path <tmp>/repo-b`
     with `CROSSBRIDGE_SOCKET_ROOT=<runtime>` in its environment.
   - Wait for `<runtime>/repo-a/repo-b.socket` AND `<runtime>/repo-b/repo-a.socket`
     to exist (that's both servers being aware of each other through the
     supervisor).

2. **Submit**:
   - Create a local issue in `repo-a`'s DB via the `crosslink` library:
     `db.create_issue("hello from a", Some("can you answer?"), "medium")`.
     Capture its local ID `LA`.
   - Run `crossbridge-client submit --issue <LA> --target repo-b` with
     `current_dir(<tmp>/repo-a)` and `CROSSBRIDGE_SOCKET_ROOT=<runtime>`.
   - Assert exit 0.
   - Inspect `repo-a`'s DB: issue `LA` now carries `xb:outbound`,
     `xb-status:pending`, and exactly one `xb-ref:<uuid>` label. Capture the UUID
     value `UR` (this is the receiver-side UUID returned by repo-b's server).
   - Inspect `repo-b`'s DB: exactly one issue exists, with `xb:inbound`,
     `xb-source:repo-a`, `xb-ref:<source-uuid>` matching the source UUID
     of `LA` in repo-a, and a body that includes the appended footer
     `After answering, run: \`crossbridge-client answer --issue <id>\``.
     Capture its ID `LB`.

3. **Answer**:
   - In `repo-b`'s DB: add a `kind=result` comment to issue `LB`
     (`db.add_comment(LB, "the answer is 42", "result")`) and a `kind=note`
     comment that should NOT be forwarded.
   - Run `crossbridge-client answer --issue <LB>` with
     `current_dir(<tmp>/repo-b)` and `CROSSBRIDGE_SOCKET_ROOT=<runtime>`.
   - Assert exit 0.
   - Inspect `repo-b`'s DB: issue `LB` is closed and labeled `xb-status:answered`.
   - Inspect `repo-a`'s DB: issue `LA` is closed; carries label
     `xb-status:resolved` (the server applies this on Answer per `.design/server.md`);
     has at least one comment whose content is `[from repo-b] the answer is 42`;
     has no comment containing the `kind=note` body (`note` was filtered by the
     client per `.design/client.md`).

4. **Idempotency probe** (within the same test):
   - Re-run the same `crossbridge-client answer --issue <LB>` invocation. The
     server's de-duplication contract (per `.design/server.md`) says the
     repeated answer is a no-op. Assert the second invocation exits non-zero
     OR exits 0 without adding a duplicate `[from repo-b] the answer is 42`
     comment to `repo-a`'s issue `LA` — accept whichever the existing
     server/client implementation already produces, but assert *both* "no
     duplicate comment" and "issue still closed". Do not modify production
     code to make this branch pass; if the current behavior is to error, the
     test asserts the error path; if it succeeds idempotently, the test
     asserts no DB drift.

5. **Teardown**: drop the process guard, which kills supervisor + both
   servers; tempdirs go away on drop.

## Acceptance Criteria

- `crossbridge-protocol` exports a public `SOCKET_ROOT_ENV` constant whose
  value is `"CROSSBRIDGE_SOCKET_ROOT"`, and `crossbridge-client` references
  this constant rather than defining its own (or its local definition
  re-exports the protocol constant).
- Running `crossbridge-supervisor` with `CROSSBRIDGE_SOCKET_ROOT=<dir>` in
  the environment (and no `--register-socket` flag) binds its register
  socket at `<dir>/register.socket`.
- Running `crossbridge-server` with `CROSSBRIDGE_SOCKET_ROOT=<dir>` in the
  environment (and no `--runtime-root` flag) connects to the supervisor at
  `<dir>/register.socket` and creates listener sockets under `<dir>/...`.
- For both supervisor and server, an explicit CLI flag still overrides the
  env var, and the env var still overrides the compiled-in default. Unit
  tests cover all three precedence cases (flag-only, env-only, both,
  neither).
- A new workspace member crate `crossbridge-e2e` exists at the project root
  with a `Cargo.toml` listing `crossbridge-supervisor`, `crossbridge-server`,
  `crossbridge-client`, `crossbridge-protocol`, and `crosslink` under
  `[dev-dependencies]` (or workspace path deps), plus `tempfile` and any
  helpers it needs.
- `Cargo.toml` (workspace root) has `crossbridge-e2e` added to `members`.
- `cargo build -p crossbridge-e2e` succeeds with no warnings.
- `cargo test -p crossbridge-e2e` runs `submit_then_answer_round_trip` and
  passes deterministically (no flake when run 5 times in a row).
- The test launches real binaries via `CARGO_BIN_EXE_*`, NOT in-process
  library entrypoints.
- The test uses `--register-socket`, `--runtime-root`, and
  `CROSSBRIDGE_SOCKET_ROOT` to keep all sockets inside a tempdir; running
  the test never creates anything under `/run/crossbridge`.
- The test cleans up child processes even on assertion failure (verified by
  checking with `ps` after a forced panic — manual sanity check during
  development; `Drop` guard is sufficient for the implementation).
- `cargo test --workspace` continues to pass (existing 75+ tests still green).
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- `cargo fmt --check` is clean.
- The test file lives at `crossbridge-e2e/tests/round_trip.rs`. Helpers (the
  `ShortTempDir` clone, child-process guard, fixture builder) live in
  `crossbridge-e2e/tests/common/mod.rs`.

## Constraints

- No mocks, fakes, or in-process shortcuts. Use the real binaries.
- The only production-code changes expected are the unified
  `CROSSBRIDGE_SOCKET_ROOT` env-var support in supervisor and server (and the
  shared protocol-crate constant). If the test additionally surfaces a real
  bug in the existing implementation, FILE A SEPARATE ISSUE rather than
  fixing it in-band — this work is the env-var unification plus the test,
  nothing more.
- No `sleep(Duration::from_secs(N))` for synchronization. Always poll for an
  observable predicate (file exists, process exited, DB row present) with a
  bounded timeout and a clear panic message on timeout.
- The test must be tolerant of running inside the project's
  `claude-sandbox` environment — i.e., it cannot assume access to host
  `/run/`. Tempdirs only.
- The `ShortTempDir` helper — keep socket paths under `/tmp/xt-…` so
  `sockaddr_un.sun_path` doesn't overflow; the per-test path budget is tight.
- Do NOT use `tokio` in the test crate. Spawn child processes with
  `std::process::Command`; poll with `std::thread::sleep` between filesystem
  checks (sleep is fine *within* a polling loop, but never as a sole barrier).
- Do NOT modify `.crosslink/hook-config.json`. If the kickoff machinery
  injects edits to it in the worktree, leave them unstaged.

## Out of Scope

- Attachment materialization round-trip (`crossbridge-server` materializing a
  binary attachment as a jj worktree commit). That's a worthwhile follow-up
  test but adds a jj dependency and is orthogonal to the issue/answer wire
  contract this test exists to lock down.
- Supervisor restart / reconnect behavior. Already covered by
  `crossbridge-supervisor/tests/integration.rs` and
  `crossbridge-server/src/supervisor.rs` unit tests against the fake
  supervisor.
- Failure-injection (server crash mid-submit, oversize frames). Existing
  per-crate tests already cover the wire-level failure modes.
