# demo-swarm

Compatibility package for the canonical [`demoswarm`](https://crates.io/crates/demoswarm)
manager.

Installing this package exposes a `demo-swarm` executable backed by the same
library and command implementation as `demoswarm`:

```bash
cargo install demo-swarm
demo-swarm version
```

The canonical package and executable spelling is `demoswarm`. This package exists
so the repository-style spelling remains functional without creating a second
installer implementation.

Licensed under Apache-2.0.
