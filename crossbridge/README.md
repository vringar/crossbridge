# crossbridge (legacy polling binary)

The original one-shot polling-cycle binary that predates the
supervisor/server/client split. Reads a `crossbridge.toml`, opens each repo's
`.crosslink/issues.db` directly, routes outbound requests, collects answers,
exits. Designed to be run on a systemd timer.

This is **not** part of the new socket-based architecture
(supervisor + server + client) and does not interact with it. Treat the
`.design/*.md` documents and the per-crate READMEs as the source of truth
for the current design; the top-level `README.md` covers v2.

## Binary

```
crossbridge -c <CONFIG>
```

`<CONFIG>` defaults to `crossbridge.toml`. Format:

```toml
[repos.<slug>]
path = "/path/to/repo"
```

## Key files

- `src/main.rs` — CLI, tracing init, `run_cycle`
- `src/config.rs` — `Config`, `RepoConfig`, TOML loader
- `src/route.rs` — `run_cycle` (outbound routing + answer collection)
