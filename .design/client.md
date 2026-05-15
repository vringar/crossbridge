# Crossbridge v2: Client CLI

## Summary

`crossbridge-client` is a synchronous, one-shot CLI invoked from inside an
agent sandbox. It enumerates available peer repos by listing sockets in
`/run/crossbridge/<own-slug>/`, submits local issues to peer repos, and
routes answer comments back to the originating repo. No async runtime, no
long-running state — every invocation makes at most one socket round-trip
and exits.

## Requirements

- Subcommands: `peers`, `submit --issue <id> --target <slug>`, `answer --issue <id>`. A global `--slug <slug>` flag overrides own-slug derivation for any subcommand.
- Own-slug resolution (precedence): `--slug <slug>` flag > `$CROSSBRIDGE_OWN_SLUG` env var > derive from the `origin` remote of the current repo (`git remote get-url origin`, or `jj git remote list` if `.jj/` exists; strip optional `.git`, take last path segment). When all three fail (no flag, env unset, no parseable origin URL), error with `cannot determine repo slug from git remote`. The flag and env hooks exist so the client works in repos with no `origin` remote (fresh local clones, ephemeral worktrees). Both inputs are trimmed; an empty/whitespace-only `--slug` value is rejected with `--slug must be a non-empty string`, an empty/whitespace-only or non-UTF-8 `$CROSSBRIDGE_OWN_SLUG` is silently ignored and resolution falls through to derivation.
- `peers`: list `*.socket` files in `/run/crossbridge/<own-slug>/`, strip the `.socket` suffix, print one slug per line. Empty output if the directory is empty. If the directory does not exist, error with `not registered with crossbridge (no socket dir)`.
- `submit --issue <id> --target <slug>`:
  - Open `<repo-root>/.crosslink/issues.db` and read issue `<id>`; if missing, error with `issue #<id> not found`.
  - Verify `/run/crossbridge/<own-slug>/<target>.socket` exists; if not, error with `peer '<target>' not available (not connected)`.
  - Connect with `std::os::unix::net::UnixStream` and send a single framed `ClientRequest::Submit(SubmitIssue { title, body, labels, source_slug=<own>, source_uuid=<issue uuid>, attachments })`.
  - Read one framed `ServerResponse`. On `Ok { issue_id }`: add labels `xb:outbound`, `xb-status:pending`, `xb-ref:<target-uuid>` to the local issue (target UUID derived from `issue_id` per protocol) and print confirmation. On `Error { message }`: print the message to stderr and exit non-zero, leaving local labels unchanged.
- `answer --issue <id>`:
  - Open the local DB, read issue `<id>`; verify it carries `xb:inbound`. If not, error with `issue #<id> is not an inbound crossbridge issue`.
  - Extract `xb-source:<source-slug>` and `xb-ref:<source-uuid>` from the issue's labels; if either is missing, error with a clear diagnostic.
  - Collect all comments with `kind=result` from the local issue.
  - Connect to `/run/crossbridge/<own-slug>/<source-slug>.socket` and send a single framed `ClientRequest::Answer(SubmitAnswer { source_uuid, comments, attachments })`.
  - On `Ok`: add label `xb-status:answered` to the local issue and close it. On `Error`: print the message and exit non-zero.
- Use the synchronous framing helpers (`write_message_sync` / `read_message_sync`) from `crossbridge-protocol`. Do NOT pull in `tokio`.
- Diagnostic errors all exit non-zero and print to stderr; success paths print a short confirmation to stdout.
- Dependencies: `crossbridge-protocol`, `crosslink`, `clap`, `anyhow`. No `tokio`.

## Acceptance Criteria

- With one peer registered, `crossbridge-client peers` prints exactly that peer's slug. With no peers, output is empty (exit 0).
- With the socket directory missing, `peers` exits non-zero with `not registered with crossbridge (no socket dir)`.
- `submit --issue N --target T` against a running peer creates a remote issue (verified via the peer's crosslink DB) and applies `xb:outbound` / `xb-status:pending` / `xb-ref:<target-uuid>` to local issue N.
- `submit --issue N --target T` against a target with no socket exits non-zero with `peer '<T>' not available (not connected)` and does not modify local labels.
- `submit --issue 9999 --target T` for a non-existent local issue exits non-zero with `issue #9999 not found`.
- `answer --issue N` on a properly-tagged inbound issue with `kind=result` comments delivers them, marks the local issue `xb-status:answered`, and closes it.
- `answer --issue N` on an issue without `xb:inbound` exits non-zero with `issue #<N> is not an inbound crossbridge issue`; the local issue is unchanged.
- The crate's `Cargo.toml` does not list `tokio` (direct or via default features of another dep).
- A submission whose total framed size would exceed 16 MiB fails fast on the client with a clear error rather than producing a malformed frame.
- `cargo build -p crossbridge-client` and `cargo test -p crossbridge-client` pass with no warnings.

## Responsibility

Per-agent CLI tool that runs inside bubblewrap sandboxes. Submits issues
to peer repos and sends answers back, communicating over Unix sockets
created by peer repo servers.

## Interface

- Reads from local crosslink database (for issue content)
- Discovers peers by listing sockets in `/run/crossbridge/<own-slug>/`
- Connects to peer sockets to submit issues or answers
- Synchronous (no async runtime needed)

## CLI

```
crossbridge-client [--slug <slug>] peers
crossbridge-client [--slug <slug>] submit --issue <id> --target <slug>
crossbridge-client [--slug <slug>] answer --issue <id>
```

The optional `--slug <slug>` flag is global (accepted by every subcommand)
and overrides own-slug derivation. See [Slug Derivation](#slug-derivation)
below.

### `peers`

List available peer repos:

```
$ crossbridge-client peers
firmware
tools
ghidra
```

Implementation: list `*.socket` files in `/run/crossbridge/<own-slug>/`,
strip the `.socket` suffix. If the directory doesn't exist or is empty,
print nothing (no peers registered).

### `submit --issue <id> --target <slug>`

Submit a local issue to a peer repo:

1. Resolve own slug (flag > env > git/jj origin remote — see below)
2. Open local crosslink DB at `.crosslink/issues.db`
3. Read issue by ID — verify it exists, get title, body, labels, UUID
4. Verify target socket exists at `/run/crossbridge/<own-slug>/<target>.socket`
5. Connect to the socket
6. Send `ClientRequest::Submit(SubmitIssue { ... })`
7. Read `ServerResponse`
8. On success: update local issue labels:
   - Add `xb:outbound`, `xb-status:pending`, `xb-ref:<target-uuid>`
     (target UUID comes from response)
   - Print confirmation
9. On error: print error message, exit non-zero

The client applies source-side labels AFTER the socket call succeeds,
ensuring labels reflect actual delivery state.

### `answer --issue <id>`

Send an answer back to the issue's source repo:

1. Resolve own slug (flag > env > git/jj origin remote — see below)
2. Open local crosslink DB
3. Read issue by ID — verify it exists and has `xb:inbound` label
4. Extract `xb-source:<slug>` label to determine the source repo
5. Extract `xb-ref:<uuid>` label to identify the source issue
6. Collect all `kind=result` comments on the issue
7. Connect to `/run/crossbridge/<own-slug>/<source-slug>.socket`
8. Send `ClientRequest::Answer(SubmitAnswer { ... })`
9. Read `ServerResponse`
10. On success: mark local issue `xb-status:answered`, close it
11. On error: print error message, exit non-zero

## Slug Derivation

Own-slug resolution precedence:

1. **`--slug <slug>` flag** — explicit per-invocation override. Trimmed; an
   empty/whitespace value is rejected with
   `--slug must be a non-empty string`.
2. **`$CROSSBRIDGE_OWN_SLUG` env var** — set once in the shell or per-agent
   environment. Trimmed; empty/whitespace or non-UTF-8 values are ignored
   (resolution falls through to derivation rather than erroring, so a stray
   `export CROSSBRIDGE_OWN_SLUG=` does not block clients in repos that *do*
   have an origin remote).
3. **Derive from origin remote** — same logic as the repo server:

   ```sh
   git remote get-url origin
   # git@github.com:AMD-PSP/firmware.git → firmware
   # https://github.com/AMD-PSP/firmware  → firmware
   ```

   Parse URL, strip `.git`, take last path component. If `.jj/` exists,
   use `jj git remote list` and parse the origin entry instead.

The flag and env hooks exist for repos with no `origin` remote (fresh
local clones, ephemeral worktrees) where derivation would fail with
`cannot determine repo slug from git remote`. The constant
`OWN_SLUG_ENV` and the env-lookup helper `own_slug_from_env` are exported
from `crossbridge-protocol` so the supervisor / server / client agree on
the env var name; the flag-vs-env-vs-derive composition lives in each
binary's own `slug.rs`.

The CLI runs the chosen resolution on every invocation (no cached state).
The cost is negligible — at most one `git` subprocess in the derive
fallback.

## Synchronous Design

The client is a one-shot CLI. It makes exactly one socket connection,
sends one message, reads one response, and exits. No async runtime needed.

Uses `std::os::unix::net::UnixStream` with the synchronous framing
helpers from `crossbridge-protocol`.

## Dependencies

- `crossbridge-protocol` — message types and framing (sync variants)
- `crosslink` — database access for reading issues
- `clap` — CLI argument parsing
- `anyhow` — error handling

**No tokio.** Synchronous only.

## Error Handling

| Scenario | Behavior |
|---|---|
| Issue doesn't exist locally | Error: "issue #N not found" |
| Target socket doesn't exist | Error: "peer '<slug>' not available (not connected)" |
| Socket connect fails | Error: "cannot reach peer '<slug>': <os error>" |
| Server returns error | Print server's error message, exit non-zero |
| Issue missing required labels | Error: "issue #N is not an inbound crossbridge issue" |
| No result comments to send | Send answer with empty comments list (server handles) |
| Can't determine own slug (no flag, no env, no parseable origin) | Error: "cannot determine repo slug from git remote" |
| `--slug` value is empty or whitespace-only | Error: "--slug must be a non-empty string" |
| Own socket directory doesn't exist | Error: "not registered with crossbridge (no socket dir)" |

## Agent Integration

Agents interact with crossbridge-client in two ways:

### Submitting a Request

The agent (or a skill/script) runs:
```sh
crosslink issue create "question for firmware team" -p high --quiet
# → 42
crossbridge-client submit --issue 42 --target firmware
```

### Answering a Request

The inbound issue body includes the instruction:
```
After answering, run: `crossbridge-client answer --issue <id>`
```

The agent follows this instruction after posting its result comment:
```sh
crosslink issue comment 17 "here are the results..." --kind result
crossbridge-client answer --issue 17
```

### Hook-Based Automation

A PostToolUse Claude Code hook can automatically call `crossbridge-client answer`
when it detects an issue close on a crossbridge-tagged issue. This is a
safety net, not the primary mechanism.
