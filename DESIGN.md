# Crossbridge Design Document

## Overview

Crossbridge is a one-shot Rust CLI that enables cross-project coordination
between crosslink-managed repositories. It scans multiple local crosslink
databases, routes labeled issues between repos, and manages the lifecycle
of cross-project requests.

Invoked via cron/systemd timer (e.g., every 30 seconds). No daemon, no
async runtime, no signal handling.

## Problem

Crosslink is project-scoped. An agent working in repo A cannot ask a question
of or make a request to repo B's codebase or agents. There is no cross-repo
communication primitive. Crossbridge fills this gap.

## Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Repo A      │     │  Repo B      │     │  Repo C      │
│  .crosslink/ │     │  .crosslink/ │     │  .crosslink/ │
│  issues.db   │     │  issues.db   │     │  issues.db   │
└──────┬───────┘     └──────┬───────┘     └──────┬───────┘
       │                    │                    │
       └────────────┬───────┴────────────────────┘
                    │
            ┌───────▼────────┐
            │  crossbridge   │
            │  run-once      │
            │                │
            │  config.toml   │
            │  issue router  │
            └────────────────┘
                    ▲
                    │
            cron / systemd timer
            (every 30s)
```

Crossbridge does NOT use crosslink's git-based sync or coordination branches.
It reads and writes `.crosslink/issues.db` files directly via the `crosslink`
crate's `Database` API.

## Crate Dependency

Crossbridge depends on the `crosslink` crate as a git dependency pinned to
a specific commit to prevent breakage from upstream changes:

```toml
[dependencies]
crosslink = { git = "https://github.com/forecast-bio/crosslink.git", rev = "12eb7b917e9ef726f40eb2f9b36cf87fa38efa4d" }
toml = "0.8"
serde = { version = "1", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
clap = { version = "4", features = ["derive"] }
```

No tokio — all operations are synchronous SQLite. `rusqlite::Connection` is
`!Send` and cannot be held across `.await` points, so an async runtime would
add complexity for zero benefit.

Key library types used:
- `crosslink::db::Database` — open, query, and mutate issue databases
- `crosslink::models::{Issue, Comment}` — domain types

### API Surface

`Database.conn` is `pub(crate)`, so crossbridge cannot execute raw SQL.
All operations go through public `Database` methods:

- `Database::open(path)` — open or create DB, run migrations
- `Database::transaction(closure)` — wrap operations in a transaction
- `list_issues(status, label, priority)` — single-label filter
- `get_labels(id)` / `get_labels_batch(ids)` — read labels
- `add_label(id, label)` / `remove_label(id, label)` — mutate labels
- `create_issue(title, desc, priority)` — create issue
- `add_comment(id, content, kind)` — add typed comment
- `get_comments(id)` — read comments
- `get_issue_uuid_by_id(id)` / `get_issue_id_by_uuid(uuid)` — UUID lookup
- `close_issue(id)` — close an issue

The main limitation is that `list_issues()` only filters by a single label.
Crossbridge filters on the most selective label (e.g., `xb:outbound`) and
post-filters using `get_labels_batch()`. Low volume makes this acceptable.

## Configuration

Per-machine config file, not committed (gitignored):

```toml
[repos.psp-firmware]
path = "/home/user/projects/AMD-PSP/firmware"

[repos.psp-tools]
path = "/home/user/projects/AMD-PSP/tools"

[repos.psp-docs]
path = "/home/user/projects/AMD-PSP/docs"
```

Repo slugs are the TOML table keys. These are what agents use in
`xb-target:psp-tools` labels.

## Label Protocol

All crossbridge labels use the `xb:` or `xb-` prefix to avoid collision
with user labels. Crosslink labels are free-form strings up to 128 chars.

### Label Reference

| Label | Side | Meaning |
|---|---|---|
| `type:request` | both | Marks the issue as a cross-project request |
| `xb:outbound` | source | Awaiting bridge pickup |
| `xb:inbound` | target | Created by bridge in target repo |
| `xb-status:open` | both | Not yet picked up / not yet answered |
| `xb-status:pending` | source | Bridge delivered to target |
| `xb-status:answered` | target | Target agent posted a result |
| `xb-status:resolved` | source | Answer copied back, lifecycle complete |
| `xb-status:error` | source | Routing failed (unknown target, etc.) |
| `xb-target:<slug>` | source | Destination repo slug |
| `xb-source:<slug>` | target | Originating repo slug |
| `xb-ref:<uuid>` | both | Links paired issues via crosslink UUID |

### Label Invariants

- An issue has at most ONE `xb-status:*` label at any time.
- `xb-target:` and `xb-source:` are mutually exclusive on the same issue.
- `xb-ref:` is added after delivery, pointing to the paired issue's UUID.
- On error, `xb:outbound` is removed to prevent scan waste from accumulating
  error-state issues in every poll cycle.

## State Machine

```
SOURCE REPO                              TARGET REPO
-----------                              -----------

[1] Agent creates issue with labels:
    type:request, xb:outbound,
    xb-status:open, xb-target:<slug>
         |
         |
[2] Bridge picks up (phase 1 scan):     [3] Bridge creates issue with labels:
    - validates target exists                type:request, xb:inbound,
    - idempotency check: scan target         xb-status:open, xb-source:<slug>,
      for existing xb-ref:<src-uuid>         xb-ref:<source-uuid>
    - creates issue in target           -------------------------------------
    - in transaction:                        |
      remove xb-status:open                  |
      add    xb-status:pending          [4] Target agent works on request,
      add    xb-ref:<target-uuid>           adds comment (kind=result),
         |                                  in transaction:
         |                                    remove xb-status:open
         |                                    add    xb-status:answered
         |                                   |
[5] Bridge picks up (phase 2 scan): <-------+
    - copies result comments to source
      (dedup by checking existing comments)
    - in transaction on source:
      remove xb-status:pending
      add    xb-status:resolved
    - closes source issue
    - closes target issue
         |
[DONE] Both issues closed.
```

## Implementation

### Project Layout

```
hyperlink/
+-- Cargo.toml
+-- crossbridge.toml.example    # checked in - template
+-- crossbridge.toml            # gitignored - per-machine config
+-- DESIGN.md                   # this document
+-- src/
|   +-- main.rs                 # one-shot entrypoint, tracing, config loading
|   +-- config.rs               # TOML parsing, validation
|   +-- route.rs                # phase 1 + phase 2 + helpers
+-- script/
|   +-- crossbridge-request     # wrapper: create outbound request in one command
|   +-- crossbridge-answer      # wrapper: answer + mark answered in one command
+-- skill/
    +-- crossbridge.md              # unified skill: ask, answer, check
```

Three source files. `route.rs` contains both phases because they share the
same helpers and operate on the same "bridge cycle" concept.

### Entry Point (main.rs)

One-shot execution. Run once, process all pending work, exit.

```
crossbridge                     # uses ./crossbridge.toml
crossbridge -c /path/to/config  # explicit config path
crossbridge --dry-run            # scan and report, don't mutate
```

### Phase 1: Route Outbound (route.rs)

For each repo, scan for issues with label `xb:outbound` and status `open`.
Post-filter using `get_labels_batch()` for `xb-status:open`.

For each matching issue:
1. Extract `xb-target:<slug>` label, validate target exists in config
2. Get source issue UUID via `get_issue_uuid_by_id()`
3. **Idempotency check**: scan target DB for existing issue with label
   `xb-ref:<source-uuid>`. If found, skip creation — just ensure source
   labels are updated
4. Create issue in target DB with labels: `type:request`, `xb:inbound`,
   `xb-status:open`, `xb-source:<slug>`, `xb-ref:<source-uuid>`
5. Update source in a transaction:
   - remove `xb-status:open`
   - add `xb-status:pending`
   - add `xb-ref:<target-uuid>`

On unknown target:
- Add error comment with available targets
- In transaction: remove `xb-status:open`, remove `xb:outbound`,
  add `xb-status:error`

### Phase 2: Collect Answered (route.rs)

For each repo, scan for issues with label `xb:inbound` and status `open`.
Post-filter for `xb-status:answered`.

For each matching issue:
1. Extract `xb-source:<slug>` and `xb-ref:<uuid>` labels
2. Open source DB, find source issue by UUID
3. Copy `kind=result` comments to source issue, prefixed with `[from <slug>]`
4. **Dedup**: skip comments whose content already exists on the source issue
5. Update source in transaction: swap `xb-status:pending` to `xb-status:resolved`
6. Close both issues

### Helper: Atomic Status Swap

```rust
fn swap_status_label(db: &Database, issue_id: i64, old: &str, new: &str) -> Result<()> {
    db.transaction(|| {
        db.remove_label(issue_id, old)?;
        db.add_label(issue_id, new)?;
        Ok(())
    })
}
```

Uses `Database::transaction()` to ensure the swap is atomic. No crash
recovery needed for missing status labels.

## Target Discovery

No manifest file. Agents learn available targets through two paths:

1. **The responder skill** documents the naming convention
2. **Error feedback**: if an agent uses an unknown target slug, crossbridge
   comments on the issue with "unknown target 'X', available: [Y, Z, ...]"

This avoids writing files to repos, avoids gitignore churn, and leverages
the error path that already exists.

## Wrapper Scripts

Multi-step label application is fragile for LLM agents. Provide single-command
wrappers:

### crossbridge-request

```sh
#!/bin/sh
# Usage: crossbridge-request "question or request" <target-slug>
set -e
id=$(crosslink issue create "$1" -p high --quiet)
crosslink issue label "$id" type:request
crosslink issue label "$id" xb:outbound
crosslink issue label "$id" xb-status:open
crosslink issue label "$id" "xb-target:$2"
echo "Created outbound request $id -> $2"
```

### crossbridge-answer

```sh
#!/bin/sh
# Usage: crossbridge-answer <issue-id> "your answer"
set -e
crosslink issue comment "$1" "$2" --kind result
crosslink issue unlabel "$1" xb-status:open
crosslink issue label "$1" xb-status:answered
echo "Answered issue $1"
```

## Error Handling

| Scenario | Behavior |
|---|---|
| Unknown target slug | Comment on source with available targets, label `xb-status:error`, remove `xb:outbound` |
| Target DB can't be opened | Log warning, skip this issue, continue with next |
| Source DB can't be opened | Log warning, skip repo, continue with next |
| Crash after target creation, before source update | Idempotency check on next run detects existing target issue, skips creation, updates source labels |
| Source issue deleted while pending | Log warning, close orphaned target issue if found |
| Duplicate result comments | Dedup by content comparison before copying |
| No result comments on answered issue | Copy placeholder: "crossbridge: target agent marked answered but provided no result comments" |
| Config references nonexistent path | Log error at startup, exclude from processing |
| Individual issue processing error | Log and continue with next issue (no `?` propagation that kills the loop) |

## Concurrency and Safety

- Single-threaded, single-process, one-shot execution. No concurrent writes.
- SQLite WAL mode allows crossbridge to read while agents write.
- Connections opened per-invocation, closed on exit. No lock holding.
- Label swaps use `Database::transaction()` for atomicity within a single DB.
- Cross-DB operations (write target, then write source) cannot be atomic.
  The idempotency check handles the crash-between-DBs scenario.

## MVP vs Deferred

### MVP (this implementation)
- Phase 1: outbound routing
- Phase 2: answer collection
- Config parsing + validation
- Wrapper scripts
- Responder skill
- Integration tests with tempfile databases

### Deferred
- Daemon mode with poll loop (use cron for now)
- inotify-based triggering
- Remote repo support
- `--dry-run` flag
- Request chaining / conversations
