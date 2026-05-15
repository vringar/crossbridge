# crossbridge-client

Per-agent CLI for submitting issues and answers across repos via the
crossbridge per-peer server sockets. Synchronous (`std::os::unix::net`); no
tokio runtime.

**Spec:** [`.design/client.md`](../.design/client.md)

## Binary

```
crossbridge-client [--slug <SLUG>] peers
crossbridge-client [--slug <SLUG>] submit --issue <ID> --target <SLUG>
crossbridge-client [--slug <SLUG>] answer --issue <ID>
```

Reads the local repo's `.crosslink/issues.db` directly to fetch issue bodies
and labels, then sends a `ClientRequest` to the target peer's socket under
`<socket_root>/<target-slug>/<own-slug>.socket`.

**Own-slug resolution** (precedence): `--slug <SLUG>` flag >
`$CROSSBRIDGE_OWN_SLUG` env var > derived from the `origin` remote of the
current repo (`git remote get-url origin`, or `jj git remote list` if a
`.jj/` directory is present). Use the flag or env override in a repo with no
`origin` remote (fresh local clones, ephemeral worktrees) where derivation
would fail with `cannot determine repo slug from git remote`.

**Socket-root resolution** (via `socket_root()`):
`$CROSSBRIDGE_SOCKET_ROOT` > `$XDG_RUNTIME_DIR/crossbridge` > compiled-in
fallback `/run/crossbridge`. The shared resolver lives in
`crossbridge_protocol::default_socket_root`.

## Public API (lib)

- `pub mod labels` — label conventions for inbound/outbound/answered states
- `pub mod peers` — `list_peers()` (queries the runtime root for slug dirs)
- `pub mod slug` — `derive_own_slug` (from current repo's origin remote)
- `pub fn socket_root() -> PathBuf`
