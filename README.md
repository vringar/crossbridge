# Crossbridge

Cross-project coordination bridge for [crosslink](https://github.com/forecast-bio/crosslink) repositories.

Crossbridge is a one-shot CLI that routes labeled issues between crosslink-managed repos on the same machine. It enables agents working in different repositories to ask questions of each other without shared state or network services.

## How it works

```
Repo A (.crosslink/issues.db)  <--\
Repo B (.crosslink/issues.db)  <---+--> crossbridge (runs every 30s via systemd timer)
Repo C (.crosslink/issues.db)  <--/
```

1. An agent in Repo A creates an issue labeled `xb:outbound` + `xb-target:repo-b`
2. Crossbridge picks it up, creates a corresponding inbound issue in Repo B
3. An agent in Repo B answers (posts a `result` comment, marks `xb-status:answered`)
4. Crossbridge copies the answer back to Repo A and closes both issues

No daemon, no ports, no async runtime. Opens SQLite databases, processes pending work, exits.

## Configuration

Create a `crossbridge.toml` with paths to your crosslink repos:

```toml
# Each table key is the "slug" that agents use in xb-target:<slug> labels.

[repos.psp-firmware]
path = "/home/user/projects/AMD-PSP/firmware"

[repos.psp-tools]
path = "/home/user/projects/AMD-PSP/tools"

[repos.psp-docs]
path = "/home/user/projects/AMD-PSP/docs"

[repos.nixos-setup]
path = "/home/user/projects/nixos-setup"
```

## Usage

```sh
# Run one bridge cycle (scan all repos, route pending, collect answers)
crossbridge -c /path/to/crossbridge.toml

# With debug logging
RUST_LOG=crossbridge=debug crossbridge -c /path/to/crossbridge.toml
```

## NixOS deployment

A NixOS module is provided in `nix/module.nix`:

```nix
{ imports = [ /path/to/crossbridge/nix/module.nix ]; }

services.crossbridge = {
  enable = true;
  configFile = /etc/crossbridge.toml;  # or a path from your secrets/config
  interval = "30s";                     # systemd timer interval
  logLevel = "crossbridge=info";
};
```

The module creates a systemd timer + oneshot service with `DynamicUser=true` and filesystem hardening. The service user needs read access to the `.crosslink/issues.db` files referenced in the config.

## Building

```sh
# With nix
nix-build

# With cargo (requires sqlite + pkg-config)
nix-shell --run "cargo build --release"
```

The package installs three binaries:
- `crossbridge` -- the bridge CLI
- `crossbridge-request` -- helper script for agents to create outbound requests
- `crossbridge-answer` -- helper script for agents to answer inbound requests

## Agent integration

Agents use the `/crossbridge` skill (see `skill/crossbridge.md`) which covers:
- **ask** -- send a question to another repo's agent
- **answer** -- respond to an inbound request
- **check** -- list pending inbound/outbound requests

## License

MIT
