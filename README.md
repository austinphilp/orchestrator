# orchestrator

Rust workspace for the Orchestrator bootstrap architecture.

## Workspace crates

- `orchestrator-app`: composition root and app/controller contracts.
- `orchestrator`: process entrypoint binary.
- `orchestrator-core`: shared domain models, traits, and errors.
- `orchestrator-worker-protocol`: canonical worker protocol IDs/events/errors and backend traits.
- `orchestrator-worker-lifecycle`: worker session lifecycle state machine and control APIs.
- `orchestrator-worker-eventbus`: worker event fanout and backpressure semantics.
- `orchestrator-worker-scheduler`: shared scheduling and checkpoint policy.
- `orchestrator-worker-runtime`: runtime composition facade for app wiring.
- `orchestrator-harness`: harness contracts, provider factory, and OpenCode/Codex provider implementations.
- `orchestrator-ui`: minimal ratatui-based interface loop.
- `orchestrator-supervisor`: OpenRouter supervisor adapter.
- `orchestrator-ticketing`: ticketing contracts, provider factory, and Linear/Shortcut implementations.
- `orchestrator-vcs`: local git operations contracts, provider factory, and git CLI implementation.
- `orchestrator-vcs-repos`: repo host contracts, provider factory, and GitHub `gh` implementation.

## Quick start

```bash
cargo check --workspace
cargo test --workspace
cargo ci-check
cargo ci-test
cargo run -p orchestrator
```

Configuration:
- Set `ORCHESTRATOR_CONFIG` to a path containing TOML config (for example `/path/to/config.toml`).
- If `ORCHESTRATOR_CONFIG` is not set, the app defaults to `~/.config/orchestrator/config.toml`.
- When the resolved config file does not exist, the app creates it with default values on first run.
- Runtime configuration is TOML-first (non-secret settings), with environment reserved for API keys and config path.
- Optional CLI overrides:
  - `--ticketing-provider ticketing.linear|ticketing.shortcut`
  - `--harness-provider harness.opencode|harness.codex`
  - `--vcs-provider vcs.git_cli`
  - `--vcs-repo-provider vcs_repos.github_gh_cli`

Example `config.toml`:

```toml
workspace = "/home/user/.local/share/orchestrator/workspace"
worktrees_root = "/home/user/.local/share/orchestrator/workspace"
event_store_path = "/home/user/.local/share/orchestrator/orchestrator-events.db"
ticketing_provider = "ticketing.linear"
harness_provider = "harness.codex"
vcs_provider = "vcs.git_cli"
vcs_repo_provider = "vcs_repos.github_gh_cli"

[supervisor]
model = "c/claude-haiku-4.5"
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
  { workflow_state = "PRDrafted", linear_state = "In Review" },
  { workflow_state = "AwaitingYourReview", linear_state = "In Review" },
  { workflow_state = "ReadyForReview", linear_state = "In Review" },
  { workflow_state = "InReview", linear_state = "In Review" },
  { workflow_state = "PendingMerge", linear_state = "In Review" },
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
pr_pipeline_poll_interval_secs = 15

[database]
max_connections = 8
busy_timeout_ms = 5000
wal_enabled = true
synchronous = "NORMAL"
chunk_event_flush_ms = 250

[ui]
theme = "nord"
ticket_picker_priority_states = ["In Progress", "Final Approval", "Todo", "Backlog"]
transcript_line_limit = 100
background_session_refresh_secs = 15
session_info_background_refresh_secs = 15
```

Directory layout defaults:
- Worktrees: `~/.local/share/orchestrator/workspace/<ticket-worktree>`
- Event store + logs: `~/.local/share/orchestrator/`
- Config: `~/.config/orchestrator/config.toml`
