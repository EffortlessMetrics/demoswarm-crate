# demoswarm

The canonical DemoSwarm lifecycle and evidence-operations manager.

`demoswarm` detects supported agent hosts, scaffolds project-owned configuration,
reports installation and run health, and provides deterministic `.runs` inspection.
Host-native adapters execute DemoSwarm flows; the manager does not invoke models or
route agents.

## Commands

```text
demoswarm install
demoswarm update
demoswarm uninstall
demoswarm status
demoswarm diff
demoswarm configure
demoswarm migrate
demoswarm doctor
demoswarm platforms
demoswarm runs ...
demoswarm version
```

The initial alpha implements the shared CLI/output foundation and read-only
operational surfaces. Mutation commands that require the transaction engine fail
with stable diagnostics rather than pretending to have changed the project.

## Machine output

Pass `--json` for a versioned, noninteractive JSON envelope. JSON is written to
stdout; diagnostics and rendering failures use stderr and stable exit classes.

## License

Apache-2.0.
