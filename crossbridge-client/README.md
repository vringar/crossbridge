# crossbridge-client

Per-agent CLI for submitting issues and answers across repos via the
crossbridge per-peer server sockets. Synchronous (`std::os::unix::net`); no
tokio runtime.

**Spec:** [`.design/client.md`](../.design/client.md)

## Binary

```
crossbridge-client peers
crossbridge-client submit --issue <ID> --target <SLUG>
crossbridge-client answer --issue <ID>
```

Reads the local repo's `.crosslink/issues.db` directly to fetch issue bodies
and labels, then sends a `ClientRequest` to the target peer's socket under
`<socket_root>/<target-slug>/<own-slug>.socket`.

**Socket-root resolution** (via `socket_root()`):
`$CROSSBRIDGE_SOCKET_ROOT` > `$XDG_RUNTIME_DIR/crossbridge` > compiled-in
fallback `/run/crossbridge`. The shared resolver lives in
`crossbridge_protocol::default_socket_root`.

## Public API (lib)

- `pub mod labels` — label conventions for inbound/outbound/answered states
- `pub mod peers` — `list_peers()` (queries the runtime root for slug dirs)
- `pub mod slug` — `derive_own_slug` (from current repo's origin remote)
- `pub fn socket_root() -> PathBuf`
