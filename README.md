# demoswarm

`demoswarm` is the shared lifecycle and evidence-operations manager for DemoSwarm.
It installs and maintains host-native DemoSwarm integrations without becoming the
runtime that executes Signal → Wisdom.

The repository contains two crates:

- `demoswarm`: the canonical library and executable;
- `demo-swarm`: a compatibility executable backed by the same library.

## Current implementation

The initial manager foundation provides a stable command grammar, versioned JSON
output, deterministic project discovery, platform detection, project status,
configuration scaffolding, health diagnostics, and read-only `.runs` inspection.
Lifecycle mutation commands are reserved but fail explicitly until the shared
planner, ownership ledger, and transaction journal land.

```bash
demoswarm version
demoswarm platforms
demoswarm configure --platform claude-code --dry-run
demoswarm status
demoswarm doctor
demoswarm runs list
```

Normal DemoSwarm flows must operate through the selected host's native agents,
skills, plugins, extensions, and tools. The manager is not an agent orchestrator.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Licensed under Apache-2.0.
