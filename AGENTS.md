# AGENTS

## Environment Variables

- Any environment variable introduced or consumed by code in this repository must be documented here and reflected in `.env-template` with an appropriate placeholder/example.
- For each variable, document purpose, owning subsystem, required/optional behavior, and expected format.

## Canonical environment variables

- `ORCHESTRATOR_CONFIG` (orchestrator-config, orchestrator-app): optional path to TOML config loaded by orchestrator-config and consumed by orchestrator-app.
- `OPENROUTER_API_KEY` (orchestrator-supervisor): required API key for supervisor.
- `LINEAR_API_KEY` (orchestrator-app, orchestrator-ticketing, scripts/ralph_loop.py, scripts/linear_graphql.py): required Linear API key.
- `ORCHESTRATOR_LINEAR_API_URL` (scripts/linear_graphql.py): optional Linear GraphQL endpoint override for script usage.
- `ORCHESTRATOR_HARNESS_SESSION_ID` (orchestrator-app, orchestrator-core, orchestrator-harness): optional harness thread/session identifier used to resume Codex sessions; automatically set from persisted runtime mappings and not typically configured manually.
- `ORCHESTRATOR_SHORTCUT_API_KEY` (orchestrator-app, orchestrator-ticketing): required for Shortcut ticketing.
- `ORCHESTRATOR_UPDATE_GOLDENS` (orchestrator-ui): optional test helper toggle for golden snapshot updates.

### Config TOML ownership

- Runtime settings previously controlled by `ORCHESTRATOR_*` non-secret env vars now live in `config.toml` under `ORCHESTRATOR_CONFIG`.
- This includes provider selection, supervisor model/base URL, Linear/Shortcut runtime options, git/github binary+safety controls, harness backend settings, SQLite pool/locking settings, and UI theme/ticket-priority ordering.

## Development Policy

- Backward compatibility and legacy support are not required unless explicitly requested for a given change.
- Clean code, simplification, and removal of unused code blocks take priority over preserving compatibility.
- Before making any backwards-incompatible change, notify the user explicitly; otherwise, breaking changes are acceptable.
- Before handing off work, always verify the app is buildable, tests pass, and there are no compiler warnings.

## Linear GraphQL helper in this repo

- For direct Linear API calls inside this repository/worktree, prefer repo-local `scripts/linear_graphql.py`.
- Resolve issues by identifier using `issue(id: $issueId)` (for example `AP-208`), not `issues(filter: { identifier: ... })`.

## Codex App-Server Protocol Notes

- The Codex app-server protocol is JSON-RPC, not REST.
- Supported transports are stdio and WebSocket.
- Do not implement Codex session lifecycle against REST paths like `/v1/sessions` for app-server mode.
- If a Codex session/create call returns HTML (for example Swagger docs), treat it as a wrong endpoint/protocol configuration issue.

## Codex item types reference (app-server)

- Canonical docs: `https://developers.openai.com/codex/app-server`
- `ThreadItem` types currently documented:
  - `userMessage`
  - `agentMessage`
  - `plan`
  - `reasoning`
  - `commandExecution`
  - `fileChange`
  - `mcpToolCall`
  - `collabToolCall`
  - `webSearch`
  - `imageView`
  - `enteredReviewMode`
  - `exitedReviewMode`
  - `contextCompaction`
- Turn input item types currently documented:
  - `text`
  - `image`
  - `localImage`
  - `skill`

## Codex -> orchestrator runtime mapping (current)

- Source of truth: `crates/orchestrator-harness/src/providers/codex/provider_impl.rs`.
- Message/plan deltas and completions (`item/agentMessage*`, `item/plan*`) map to plain `BackendEvent::Output` on `Stdout`.
- Reasoning/file-change/command deltas map to foldable transcript meta markers in `BackendEvent::Output` (prefix `[[orchestrator-meta|...]]`) for UI collapsing.
- Notification `error` maps to:
  - retriable: `BackendEvent::Output` on `Stderr`
  - non-retriable: `BackendEvent::Crashed`
- Notification `turn/completed` maps to:
  - failed turn: `BackendEvent::Crashed`
  - successful turn: `BackendEvent::Done`
- History hydration maps:
  - `userMessage` -> `BackendEvent::Output` (`you: ...`)
  - `agentMessage` and `plan` -> `BackendEvent::Output`
- Approval/tool request notifications currently auto-responded in backend transport layer:
  - `item/commandExecution/requestApproval` -> accept
  - `item/fileChange/requestApproval` -> accept
  - `item/tool/requestUserInput` -> surfaced as interactive `NeedsInput` prompt; requires user response via UI modal
  - `item/tool/call` -> unsupported tool-call stub response
