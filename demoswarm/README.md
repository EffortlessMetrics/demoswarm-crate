# DemoSwarm

DemoSwarm is an existing EffortlessMetrics SDLC pack for Claude Code and agentic development workflows.

The future `demoswarm` crate will provide an installer for setting up DemoSwarm in Claude Code and other LLM harnesses.

This `0.0.1` release is a crates.io reservation release.

## Status

The installer is not published yet.

DemoSwarm already exists as a public template repository. This crate reserves the canonical install package name while the installer surface is prepared.

## Current crate surface

This release intentionally exposes no production installer.

Do not depend on `demoswarm = "0.0.1"` for automation or setup flows.

## Direction

The real installer should make it easy to install or update DemoSwarm assets across supported harnesses, starting with Claude Code.

The installer should not hide what it writes. It should make setup explicit, reversible, and reviewable.

## License

Licensed under the Apache License, Version 2.0.
