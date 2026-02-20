# Orchestrator Architecture and Major Functionality

## Scope and assumptions

- This document describes the current repository architecture and major behavior as implemented in code.
- It assumes `docs/harness-server-migration-plan.md` has already been completed.
- The canonical environment variable list lives in `AGENTS.md` and `.env-template`; this doc describes how those variables map to runtime behavior.

## System model at a glance

The repository is a Rust workspace with a layered design:

1. Product/runtime composition layer
- `orchestrator-app`: composition root, startup/shutdown, provider/backend selection, command routing.
- `orchestrator-ui`: terminal UI, stateful views, keymaps, interaction modes.

2. Domain and state layer
- `orchestrator-core`: domain model, workflow states, commands/events, normalization, attention engine, projections, event store abstractions.

3. Harness/runtime layer
- `orchestrator-runtime`: session/runtime primitives (worker/session lifecycle and stream-first event handling).
- `backend-opencode`: harness adapter for OpenCode-style backend streams and annotations.
- `backend-codex`: Codex backend wrapper using same backend abstraction model.

4. Integration/adapters layer
- `integration-linear`: Linear ticket provider + polling/sync behavior.
- `integration-shortcut`: Shortcut ticket provider.
- `integration-git`: git operations used by workflow automation and helper commands.
- `orchestrator-github`: GitHub operations through `gh`.
- `orchestrator-supervisor`: LLM-backed supervisor query engine (OpenRouter-based).

## Fundamental architecture

## Runtime data flow

1. App startup (`orchestrator-app`)
- Reads config/env and resolves provider selection (`ticketing`, `harness`).
- Constructs adapters (ticket provider, backend, git/github/supervisor clients).
- Opens/initializes SQLite event store and reconstructs projection state.
- Starts UI loop and any background sync loops (for example, ticket polling).

2. User interaction (`orchestrator-ui`)
- UI emits intent-level actions/commands from keybindings and view interactions.
- App command dispatch translates these into domain commands/integration calls.

3. Domain updates (`orchestrator-core`)
- Commands produce domain events (workflow/ticket/session state transitions).
- Events persist to SQLite store and update read projection.
- Projection is rendered back into UI view models.

4. External effects (integrations/backends)
- Ticket state/metadata sync to provider APIs (Linear/Shortcut).
- Backend/harness events stream into domain normalization.
- Git/GitHub operations triggered from workflow commands.
- Supervisor requests stream model output into UI conversation state.

## State and persistence model

- Event-first model:
  - Event definitions in `orchestrator-core/src/events.rs`.
  - Projection/read model in `orchestrator-core/src/projection.rs`.
  - Domain status and workflow transitions in `orchestrator-core/src/status.rs` and `orchestrator-core/src/workflow.rs`.
- Persistence:
  - SQLite-backed event and mapping store in `orchestrator-core/src/store.rs`.
  - Startup rebuilds in-memory/projected state from persisted events.
- Normalization:
  - Backend event normalization in `orchestrator-core/src/normalization.rs` makes backend-specific output consumable by core/UI.

## Major functionality inventory

## Ticket orchestration

- Ticket provider abstraction supports external issue systems (Linear/Shortcut).
- Ticket picker logic and start/resume orchestration in:
  - `orchestrator-app/src/ticket_picker.rs`
  - `orchestrator-core/src/ticket_selection.rs`
- Ticket-focused workflow states and transitions implemented in core workflow modules.

## Workflow lifecycle and attention model

- Workflow states, transition constraints, and command handling:
  - `orchestrator-core/src/workflow.rs`
  - `orchestrator-core/src/commands.rs`
  - `orchestrator-core/src/events.rs`
- Attention ranking/lanes for prioritization:
  - `orchestrator-core/src/attention_engine.rs`

## Supervisor (LLM) support

- Query engine with streaming response handling:
  - `orchestrator-supervisor/src/query_engine.rs`
- Prompt/template management:
  - `orchestrator-supervisor/src/template_library.rs`
- Supervisor-integrated app command dispatch:
  - `orchestrator-app/src/command_dispatch.rs`

## Harness/backend integration

- Backend abstraction consumed by app/runtime.
- OpenCode backend:
  - Streams/parses structured markers (checkpoint, input requests, etc.).
  - Implemented in `backend-opencode/src/lib.rs`.
- Codex backend:
  - Wrapper/adaptation around shared backend model.
  - Implemented in `backend-codex/src/lib.rs`.

## Git and GitHub automation

- Local git operations abstraction:
  - `integration-git/src/lib.rs`
- GitHub operations via `gh`:
  - `orchestrator-github/src/lib.rs`
- App command path currently includes workflows such as PR-ready approval and opening review tabs in:
  - `orchestrator-app/src/command_dispatch.rs`
- PR completion flow includes:
  - Manual merge path: `AwaitingYourReview -> ReadyForReview -> InReview -> Merging -> Done`
    - During `workflow.merge_pr`, draft PRs are automatically converted to ready-for-review before merge is attempted.
  - External/direct GitHub merge path: `AwaitingYourReview -> ReadyForReview -> InReview -> Done`

## UI and interaction model

- Main TUI in `orchestrator-ui/src/lib.rs`:
  - Modal interaction (`Normal`, `Insert`, `Terminal`).
  - Center stack views (`Inbox`, focus card, terminal, inspectors, supervisor chat).
  - Active overlay dialogs (ticket picker, confirm dialogs, diff modal) take keyboard priority over terminal needs-input prompts; plan input remains pending until the user returns to session interaction.
  - Terminal/session feed output scrolling is bounded by rendered wrapped line counts so manual scroll and follow-tail behavior stay in sync.
- Keymap trie/which-key logic:
  - `orchestrator-ui/src/keymap.rs`
- Golden snapshot testing helpers:
  - `orchestrator-ui/src/golden_tests.rs`

## Configuration model

- Runtime behavior is TOML-first via `ORCHESTRATOR_CONFIG` (`config.toml`), with missing entries defaulted during boot.
- Environment variables are reserved for:
  - secrets/API keys (`OPENROUTER_API_KEY`, `LINEAR_API_KEY`, `ORCHESTRATOR_SHORTCUT_API_KEY`)
  - config path override (`ORCHESTRATOR_CONFIG`)
  - test/internal helpers (`ORCHESTRATOR_UPDATE_GOLDENS`, `ORCHESTRATOR_HARNESS_SESSION_ID`)
- Canonical env documentation remains in:
  - `AGENTS.md`
  - `.env-template`

## Architectural boundaries that matter for future work

1. Keep `orchestrator-core` pure and provider-agnostic.
2. Keep integration-specific API behavior inside integration crates.
3. Keep `orchestrator-app` as composition/coordination, not business-rule storage.
4. Keep UI state derived from core projection/events whenever possible.
5. Treat SQLite event store and event schema as the operational source of truth for local state continuity.
