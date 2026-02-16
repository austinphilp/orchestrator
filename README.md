# orchestrator

Rust workspace for the Orchestrator bootstrap architecture.

## Workspace crates

- `orchestrator-app`: composition root and process entrypoint.
- `orchestrator-core`: shared domain models, traits, and errors.
- `orchestrator-runtime`: PTY/session lifecycle interfaces and runtime boundaries.
- `backend-opencode`: OpenCode `WorkerBackend` adapter over runtime PTY primitives.
- `orchestrator-ui`: minimal ratatui-based interface loop.
- `orchestrator-supervisor`: OpenRouter supervisor adapter.
- `orchestrator-github`: `gh` CLI adapter with process abstraction.
- `integration-linear`: Linear GraphQL adapter with polling-backed ticket cache.

## Quick start

```bash
cargo check --workspace
cargo test --workspace
cargo run -p orchestrator-app
```
