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
- Runtime configuration is TOML-first (non-secret settings), with environment reserved for API keys and config path.
- Optional CLI overrides:
  - `--ticketing-provider linear|shortcut`
  - `--harness-provider opencode|codex`

Example `config.toml`:

```toml
workspace = "/home/user/.local/share/orchestrator/workspace"
event_store_path = "/home/user/.local/share/orchestrator/orchestrator-events.db"
ticketing_provider = "linear"
harness_provider = "codex"

[supervisor]
model = "openai/gpt-4o-mini"
openrouter_base_url = "https://openrouter.ai/api/v1"

[linear]
api_url = "https://api.linear.app/graphql"
sync_interval_secs = 60
fetch_limit = 100
sync_assigned_to_me = true
sync_states = []
workflow_comment_summaries = false
workflow_attach_pr_links = true
workflow_state_map = [
  { workflow_state = "Implementing", linear_state = "In Progress" },
  { workflow_state = "Testing", linear_state = "In Progress" },
  { workflow_state = "PRDrafted", linear_state = "In Review" },
  { workflow_state = "AwaitingYourReview", linear_state = "In Review" },
  { workflow_state = "ReadyForReview", linear_state = "In Review" },
  { workflow_state = "InReview", linear_state = "In Review" },
  { workflow_state = "Merging", linear_state = "In Review" },
  { workflow_state = "Done", linear_state = "Done" },
  { workflow_state = "Abandoned", linear_state = "Canceled" },
]

[shortcut]
api_url = "https://api.app.shortcut.com/api/v3"
fetch_limit = 100

[git]
binary = "git"
allow_delete_unmerged_branches = false
allow_destructive_automation = false
allow_force_push = false

[github]
binary = "gh"

[runtime]
allow_unsafe_command_paths = false
harness_server_startup_timeout_secs = 10
harness_log_raw_events = false
harness_log_normalized_events = false
opencode_binary = "opencode"
opencode_server_base_url = "http://127.0.0.1:8787"
codex_binary = "codex"

[ui]
theme = "nord"
ticket_picker_priority_states = ["In Progress", "Final Approval", "Todo", "Backlog"]
```
