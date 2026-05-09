# crossbridge-e2e

Workspace-level end-to-end integration tests. The library is intentionally
empty — the value lives in `tests/`, which spawn real
`crossbridge-supervisor`, `crossbridge-server`, and `crossbridge-client`
binaries (via `CARGO_BIN_EXE_*`) and exercise the full bidirectional
issue/answer flow across two synthetic crosslink repositories.

**Spec:** [`.design/e2e-integration-test.md`](../.design/e2e-integration-test.md)

## Layout

- `tests/round_trip.rs` — full submit→answer→back-propagate flow
- `tests/common/` — fixtures: temp runtime root, synthetic repos, process
  spawn/teardown helpers
