---
name: crossbridge
description: Use when sending or answering cross-project requests via crossbridge — the Unix-socket transport between crosslink repos. Covers ask, answer, and check.
---

## Crossbridge — cross-project requests

Crossbridge lets you ask questions of agents in other repos, answer
requests from them, and check status. Transport is event-driven over
per-repo Unix sockets; agents drive it with the `crossbridge-client` CLI.
Local issues still carry `xb:*` labels so you can find requests with
`crosslink issue list`, but the labels are bookkeeping — not the
delivery mechanism.

### Lifecycle (30-second summary)

1. You **ask** → `crossbridge-client submit` opens the socket to the
   target repo's server, which creates an inbound issue there. On
   success the client labels your local issue `xb:outbound` /
   `xb-status:pending` / `xb-ref:<target-uuid>`.
2. The target agent works on the inbound issue (labeled `xb:inbound`)
   and posts a `result` comment.
3. They run `crossbridge-client answer` → the answer is delivered back
   over the socket; the server marks your local issue
   `xb-status:resolved` and closes it.

There is no daemon polling labels, and no scheduled sync interval —
every step is a single one-shot socket round-trip.

---

### ask — send a request to another repo

First create the local issue, then submit it:

```sh
id=$(crosslink issue create "Your question or request" -p high --quiet)
crossbridge-client submit --issue "$id" --target <slug>
```

To see which targets are reachable right now:

```sh
crossbridge-client peers
```

If the target socket is not present, `submit` exits non-zero with
`peer '<slug>' not available (not connected)` and leaves your local
issue's labels unchanged — re-run after the peer's server is up.

---

### answer — respond to an inbound request

The inbound issue's body ends with a footer telling you exactly what to
run. After posting your `result` comment:

```sh
crosslink issue comment <id> "your detailed answer" --kind result
crossbridge-client answer --issue <id>
```

`answer` reads every `kind=result` comment on the issue, ships them to
the source repo over its socket, then marks the local issue
`xb-status:answered` and closes it. Inbound requests block another
agent's work — treat them as high priority.

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

Status labels reflect where each issue sits in the round-trip:
`xb-status:open` (inbound just arrived, no one has answered yet),
`xb-status:pending` (outbound delivered, awaiting peer answer),
`xb-status:answered` (you have answered an inbound),
`xb-status:resolved` (an outbound's answer has been received and the
issue closed).

---

### When to check

- At session start
- Periodically during idle moments
- After sending a request — but there is no fixed bridge interval to
  wait for; the answer arrives the moment the peer agent runs
  `crossbridge-client answer`. Re-run the check commands when you want
  a fresh view.
