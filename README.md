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
- `integration-git`: Git CLI adapter for repository discovery and worktree lifecycle.
- `integration-linear`: Linear GraphQL adapter with polling-backed ticket cache.

## Quick start

```bash
cargo check --workspace
cargo test --workspace
cargo ci-check
cargo ci-test
cargo run -p orchestrator-app
```

Configuration:
- Set `ORCHESTRATOR_CONFIG` to a path containing TOML config (for example `/path/to/config.toml`).
- If `ORCHESTRATOR_CONFIG` is not set, the app defaults to `~/.config/orchestrator/config.toml`.
- When the resolved config file does not exist, the app creates it with default values on first run.
