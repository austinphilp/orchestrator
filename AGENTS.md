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
- `ORCHESTRATOR_SHORTCUT_API_KEY` (integration-shortcut): required for Shortcut ticketing.
- `ORCHESTRATOR_SHORTCUT_API_URL` (integration-shortcut): optional API endpoint override (defaults to `https://api.app.shortcut.com/api/v3`).
- `ORCHESTRATOR_SHORTCUT_FETCH_LIMIT` (integration-shortcut): optional fetch cap for Shortcut ticket sync.
- `ORCHESTRATOR_CODEX_BIN` (backend-codex): optional path to `codex` binary.
- `ORCHESTRATOR_OPENCODE_BIN` (backend-opencode): optional path to opencode binary.
- `ORCHESTRATOR_INSTRUCTION_PRELUDE` (backend-opencode): optional instruction prelude injected into child process env.
- `ORCHESTRATOR_TICKET_PICKER_PRIORITY_STATES` (orchestrator-ui): optional comma-separated ticket state ordering.
- `ORCHESTRATOR_UPDATE_GOLDENS` (orchestrator-ui): optional test helper toggle for golden snapshot updates.

## Development Policy

- Backward compatibility and legacy support are not required unless explicitly requested for a given change.
- Clean code, simplification, and removal of unused code blocks take priority over preserving compatibility.
- Before making any backwards-incompatible change, notify the user explicitly; otherwise, breaking changes are acceptable.
- Before handing off work, always verify the app is buildable, tests pass, and there are no compiler warnings.
