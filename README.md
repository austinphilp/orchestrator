# orchestrator

Rust workspace for the Orchestrator bootstrap architecture.

## Workspace crates

- `orchestrator-app`: composition root and process entrypoint.
- `orchestrator-core`: shared domain models, traits, and errors.
- `orchestrator-runtime`: session lifecycle interfaces and runtime boundaries.
- `backend-opencode`: OpenCode `WorkerBackend` adapter over event-stream sessions.
- `backend-codex`: Codex-compatible `WorkerBackend` adapter (uses `backend-opencode` transport).
- `orchestrator-ui`: minimal ratatui-based interface loop.
- `orchestrator-supervisor`: OpenRouter supervisor adapter.
- `orchestrator-github`: `gh` CLI adapter with process abstraction.
- `integration-git`: Git CLI adapter for repository discovery and worktree lifecycle.
- `integration-linear`: Linear GraphQL adapter with polling-backed ticket cache.
- `integration-shortcut`: Shortcut adapter for tickets.

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
- Choose runtime providers via config or environment:
  - `ORCHESTRATOR_TICKETING_PROVIDER=linear|shortcut` (defaults to `linear`).
  - `ORCHESTRATOR_HARNESS_PROVIDER=opencode|codex` (defaults to `codex`).
  - `ORCHESTRATOR_OPENCODE_SERVER_BASE_URL` (optional Opencode harness server URL override).
  - `ORCHESTRATOR_CODEX_SERVER_BASE_URL` is deprecated/unsupported (Codex uses app-server JSON-RPC over stdio).
  - `ORCHESTRATOR_HARNESS_SERVER_STARTUP_TIMEOUT_SECS` (optional managed harness server health-check timeout in seconds).
- Optional CLI overrides:
  - `--ticketing-provider linear|shortcut`
  - `--harness-provider opencode|codex`
