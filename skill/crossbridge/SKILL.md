---
name: crossbridge
description: Use when sending or answering cross-project requests via crossbridge — the xb:* labeled-issue transport between crosslink repos. Covers ask, answer, and check.
---

## Crossbridge — cross-project requests

Crossbridge lets you ask questions of agents in other repos, answer
requests from them, and check status. All operations use crosslink issues
with `xb:` labels as the transport.

### Lifecycle (30-second summary)

1. You **ask** → creates an outbound issue with `xb:outbound` + `xb-target:<slug>`
2. The bridge daemon copies it to the target repo as an inbound issue
3. Target agent **answers** → posts a `result` comment, marks `xb-status:answered`
4. The bridge copies the answer back, marks `xb-status:resolved`, closes both issues

---

### ask — send a request to another repo

```sh
crossbridge-request "Your question or request" <target-slug>
```

If the script is not on PATH, do it manually:

```sh
id=$(crosslink issue create "Your question or request" -p high --quiet)
crosslink issue label "$id" type:request
crosslink issue label "$id" xb:outbound
crosslink issue label "$id" xb-status:open
crosslink issue label "$id" "xb-target:<slug>"
```

If you use an unknown target slug, the bridge will comment on your issue
with the list of available targets.

---

### answer — respond to an inbound request

First find inbound requests (see **check** below), then answer:

```sh
crossbridge-answer <issue-id> "your detailed answer"
```

Manual fallback:

```sh
crosslink issue comment <id> "your detailed answer" --kind result
crosslink issue unlabel <id> xb-status:open
crosslink issue label <id> xb-status:answered
```

Inbound requests block another agent's work — treat them as high priority.

---

### check — see what's pending

**Inbound requests waiting for you:**

```sh
crosslink issue list -l xb:inbound -s open
```

**Outbound requests you sent (still waiting for answers):**

```sh
crosslink issue list -l xb:outbound
```

Filter further by status label: `xb-status:open` (not yet picked up),
`xb-status:pending` (delivered, awaiting answer), `xb-status:resolved`
(answer received), `xb-status:error` (routing failed).

---

### When to check

- At session start
- Periodically during idle moments
- After sending a request, poll until resolved (bridge runs every ~30s)
