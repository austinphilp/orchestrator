# AGENTS

## Environment Variables

- Any environment variable introduced or consumed by code in this repository must be documented here and reflected in `.env-template` with an appropriate placeholder/example.
- For each variable, document purpose, owning subsystem, required/optional behavior, and expected format.

## Canonical environment variables

- `ORCHESTRATOR_CONFIG` (orchestrator-app): optional path to TOML config.
- `OPENROUTER_API_KEY` (orchestrator-supervisor): required API key for supervisor.
- `OPENROUTER_BASE_URL` (orchestrator-supervisor): optional base URL override (defaults to `https://openrouter.ai/api/v1`).
- `ORCHESTRATOR_SUPERVISOR_MODEL` (orchestrator-ui): optional chat model override.
- `LINEAR_API_KEY` (integration-linear, scripts/ralph_loop.py): required Linear API key.
- `ORCHESTRATOR_LINEAR_API_URL` (integration-linear): optional Linear GraphQL endpoint override.
- `ORCHESTRATOR_LINEAR_SYNC_INTERVAL_SECS` (integration-linear): optional poll interval in seconds.
- `ORCHESTRATOR_LINEAR_FETCH_LIMIT` (integration-linear): optional fetch limit for synced issues.
- `ORCHESTRATOR_LINEAR_SYNC_ASSIGNED_TO_ME` (integration-linear): optional boolean.
- `ORCHESTRATOR_LINEAR_SYNC_STATES` (integration-linear): optional comma-separated Linear state filter.
- `ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP` (integration-linear): optional workflow→Linear map using `WorkflowState=LinearState` pairs.
- `ORCHESTRATOR_LINEAR_WORKFLOW_COMMENT_SUMMARIES` (integration-linear): optional boolean.
- `ORCHESTRATOR_LINEAR_WORKFLOW_ATTACH_PR_LINKS` (integration-linear): optional boolean.
- `ORCHESTRATOR_GH_BIN` (orchestrator-github): optional path to `gh` binary.
- `ORCHESTRATOR_GIT_BIN` (integration-git): optional path to `git` binary.
- `ORCHESTRATOR_GIT_ALLOW_DELETE_UNMERGED_BRANCHES` (integration-git): optional boolean.
- `ORCHESTRATOR_GIT_ALLOW_DESTRUCTIVE_AUTOMATION` (integration-git): optional boolean.
- `ORCHESTRATOR_GIT_ALLOW_FORCE_PUSH` (integration-git): optional boolean.
- `ORCHESTRATOR_ALLOW_UNSAFE_COMMAND_PATHS` (orchestrator-github, integration-git, backend-opencode): optional safety override boolean.
- `ORCHESTRATOR_TICKETING_PROVIDER` (orchestrator-app): optional ticketing provider override (`linear` or `shortcut`; default `linear`).
- `ORCHESTRATOR_HARNESS_PROVIDER` (orchestrator-app): optional harness/backend provider override (`opencode` or `codex`; default `codex`).
- `ORCHESTRATOR_HARNESS_SESSION_ID` (orchestrator-core, backend-codex): optional harness thread/session identifier used to resume Codex sessions; automatically set from persisted runtime mappings and not typically configured manually.
- `ORCHESTRATOR_SHORTCUT_API_KEY` (integration-shortcut): required for Shortcut ticketing.
- `ORCHESTRATOR_SHORTCUT_API_URL` (integration-shortcut): optional API endpoint override (defaults to `https://api.app.shortcut.com/api/v3`).
- `ORCHESTRATOR_SHORTCUT_FETCH_LIMIT` (integration-shortcut): optional fetch cap for Shortcut ticket sync.
- `ORCHESTRATOR_CODEX_BIN` (backend-codex): optional path to `codex` binary.
- `ORCHESTRATOR_OPENCODE_BIN` (backend-opencode): optional path to opencode binary.
- `ORCHESTRATOR_OPENCODE_SERVER_BASE_URL` (backend-opencode): optional base URL override for OpenCode harness server (defaults to managed local server at `http://127.0.0.1:8787`).
- `ORCHESTRATOR_CODEX_SERVER_BASE_URL` (backend-codex): deprecated/unsupported. Codex uses JSON-RPC app-server over stdio; setting this now causes a configuration error.
- `ORCHESTRATOR_HARNESS_SERVER_STARTUP_TIMEOUT_SECS` (backend-opencode, backend-codex): optional startup health-check timeout in seconds for managed harness server processes.
- `ORCHESTRATOR_HARNESS_LOG_RAW_EVENTS` (backend-opencode, backend-codex): optional boolean toggle to append raw harness event payload lines to `.orchestrator/logs/harness-raw.log`.
- `ORCHESTRATOR_HARNESS_LOG_NORMALIZED_EVENTS` (backend-opencode, backend-codex): optional boolean toggle to append normalized `BackendEvent` payloads to `.orchestrator/logs/harness-normalized.log`.
- `ORCHESTRATOR_TICKET_PICKER_PRIORITY_STATES` (orchestrator-ui): optional comma-separated ticket state ordering.
- `ORCHESTRATOR_UI_THEME` (orchestrator-ui): optional UI markdown theme override for terminal chat rendering (`nord` or `default`; defaults to `nord`).
- `ORCHESTRATOR_UPDATE_GOLDENS` (orchestrator-ui): optional test helper toggle for golden snapshot updates.

## Development Policy

- Backward compatibility and legacy support are not required unless explicitly requested for a given change.
- Clean code, simplification, and removal of unused code blocks take priority over preserving compatibility.
- Before making any backwards-incompatible change, notify the user explicitly; otherwise, breaking changes are acceptable.
- Before handing off work, always verify the app is buildable, tests pass, and there are no compiler warnings.

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

- Source of truth: `crates/backend-codex/src/lib.rs`.
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
  - `item/tool/requestUserInput` -> empty answers
  - `item/tool/call` -> unsupported tool-call stub response
