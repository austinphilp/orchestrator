# Runtime Refactor Plan: Decompose and Replace `orchestrator-runtime`

## Status
- Date: 2026-02-22
- Scope: planning/spec document
- Decision: replace `crates/orchestrator-runtime` entirely (no compatibility shim)
- Implementation update (RRP26 A01): scaffold crates added for `orchestrator-worker-protocol`, `orchestrator-worker-lifecycle`, `orchestrator-worker-eventbus`, `orchestrator-worker-scheduler`, and `orchestrator-worker-runtime`; protocol/event/error trait boundaries and minimal registry/scheduler/event envelope scaffolds are in place; `crates/orchestrator-runtime` is feature-frozen.
- Implementation update (RRP26 A02): canonical worker protocol IDs/events/errors/session+backend trait contracts are owned by `orchestrator-worker-protocol`; `orchestrator-runtime` re-exports compatibility aliases for existing consumers while protocol imports are adopted downstream where feasible.
- Implementation update (RRP26 A03): `backend-opencode` now depends on `orchestrator-worker-protocol` for worker runtime contracts and implements `WorkerSessionControl`, `WorkerSessionStreamSource`, and `WorkerBackendInfo` directly; `orchestrator-runtime::WorkerBackend` now has a blanket impl over that trait set for transitional compatibility.
- Implementation update (RRP26 A04): `backend-codex` now depends on `orchestrator-worker-protocol`, implements `WorkerSessionControl` + `WorkerSessionStreamSource` + `WorkerBackendInfo` directly, and codex harness contract tests target the protocol traits with a legacy-runtime bridge compile check.
- Implementation update (RRP26 A05): `orchestrator-worker-eventbus` now provides core publish/fanout APIs with `WorkerEventEnvelope` sequencing, bounded non-blocking broadcast queues, `subscribe_session(session_id)` + `subscribe_all()` consumption paths, and explicit `remove_session(session_id)` teardown support with fanout correctness tests.
- Implementation update (RRP26 A06): `orchestrator-worker-eventbus` now owns lag/drop accounting for session/global subscribers, emits synthetic stderr truncation markers for lagged session subscriptions, exposes eventbus perf snapshots (drop/lag/clone/dispatch counters), and minimizes publish-path clones based on active receiver classes.
- Implementation update (RRP26 A07): `orchestrator-worker-lifecycle` now owns the lifecycle state machine (`Starting`/`Running`/`Done`/`Crashed`/`Killed`) and focus visibility registry transitions, including duplicate reservation checks, spawn rollback/session-id mismatch safeguards, terminal teardown transitions that release handles and clear focus, and lifecycle-focused unit coverage for spawn success/failure and terminal behavior.
- Implementation update (RRP26 A08): `orchestrator-worker-lifecycle` now exposes async lifecycle control APIs (`spawn`/`kill`/`send_input`/`resize`/`respond_to_needs_input`/`focus_session`/`set_visibility`/`list`/`status`) and owns one stream-ingestion task per active session that publishes backend events into `orchestrator-worker-eventbus`; stream close emits synthetic `Done`, stream errors emit synthetic `Crashed`, terminal transitions preserve sticky parity semantics, and `orchestrator-worker-runtime` + `orchestrator-app` now consume this composition path.

## Why This Refactor
`orchestrator-runtime` currently combines too many concerns:
- backend protocol contract/types
- worker lifecycle/session registry
- event fanout/subscription/backpressure
- periodic checkpoint scheduling
- performance counters across all of the above

This creates tight coupling, hard-to-test behavior, and scaling limits as worker count grows.

## Objectives
- Remove `crates/orchestrator-runtime`.
- Replace it with focused crates with single responsibility.
- Support at least 200 concurrent workers in one orchestrator process.
- Keep runtime transient/in-memory; persistence remains owned by a dedicated persistence layer after normalization.
- Preserve required user-visible behavior (terminal status transitions, focus-aware checkpoint suppression, lag markers).

## Non-Goals
- No strict API compatibility with existing `orchestrator-runtime` public types.
- No distributed multi-node orchestration in this phase.
- No runtime-level durable event WAL in this phase.

## Target Crate Topology
Create the following crates:

1. `orchestrator-worker-protocol`
- Owns shared protocol types and backend contracts only.
- No lifecycle registry, no fanout implementation, no scheduling.

2. `orchestrator-worker-lifecycle`
- Owns session lifecycle state machine and registry.
- Controls spawn/kill/input/resize/needs-input routing and focus/visibility state.
- Emits lifecycle transition events to the eventbus boundary.

3. `orchestrator-worker-eventbus`
- Owns event publish/fanout/consume interfaces.
- Owns backpressure and lag behavior.
- Supports per-session and global subscriptions.

4. `orchestrator-worker-scheduler`
- Owns timer-driven actions (checkpoint prompts, quotas).
- Shared scheduler across sessions; no per-session ticker tasks.

5. `orchestrator-worker-runtime`
- Thin composition layer wiring protocol+lifecycle+eventbus+scheduler.
- Exposes cohesive runtime facade used by orchestrator-app (including merged kernel modules) and backend wiring.

Delete:
- `crates/orchestrator-runtime`

## Ownership Boundaries
### `orchestrator-worker-protocol`
Owns:
- ID types:
  - `WorkerSessionId`
  - `WorkerArtifactId`
- backend kinds/capabilities:
  - `WorkerBackendKind`
  - `WorkerBackendCapabilities`
- spawn/session types:
  - `WorkerSpawnRequest`
  - `WorkerSessionHandle`
- canonical event model:
  - `WorkerEvent`
  - `WorkerOutputEvent`, `WorkerCheckpointEvent`, `WorkerNeedsInputEvent`, `WorkerDoneEvent`, `WorkerCrashedEvent`, etc.
- base runtime error/result:
  - `WorkerRuntimeError`
  - `WorkerRuntimeResult<T>`
- backend traits:
  - `WorkerSessionControl`
  - `WorkerSessionStreamSource`
  - `WorkerBackendInfo`

Does not own:
- session registry
- subscriber buffers/channels
- periodic task management

### `orchestrator-worker-lifecycle`
Owns:
- lifecycle state model:
  - `Starting`, `Running`, `Done`, `Crashed`, `Killed`
- session visibility:
  - `Focused`, `Background`
- registry and transitions:
  - duplicate session reservation checks
  - session ID mismatch safeguards
  - terminal transition behavior
  - teardown behavior
- control surface:
  - `spawn`
  - `kill`
  - `send_input`
  - `resize`
  - `respond_to_needs_input`
  - `focus_session`
  - `set_visibility`
  - `list_sessions`
  - `status`

Does not own:
- subscription multiplexing/fanout
- lag/drop policies
- timer loops

### `orchestrator-worker-eventbus`
Owns:
- envelopes:
  - `WorkerEventEnvelope { session_id, sequence, received_at_monotonic_nanos, event }`
- publish interface:
  - `publish(session_id, event)`
- consumption interfaces:
  - `subscribe_session(session_id)`
  - `subscribe_all()`
  - `remove_session(session_id)`
- buffer/backpressure policy:
  - bounded queues
  - lag accounting
  - truncation marker behavior for per-session subscribers
- clone minimization strategy based on receiver presence

Does not own:
- spawn/kill/session control
- backend process IO
- scheduler

### `orchestrator-worker-scheduler`
Owns:
- shared periodic scheduler
- registration/cancellation APIs
- session-targeted periodic actions
- checkpoint policy:
  - cadence
  - prompt payload
  - focus suppression
  - failure handling

Does not own:
- core lifecycle state machine
- event storage/fanout

### `orchestrator-worker-runtime`
Owns:
- dependency wiring/composition only
- facade APIs consumed by orchestrator-app integration
- aggregated perf snapshot across subcomponents

Does not own:
- protocol primitives
- lifecycle logic
- fanout logic
- scheduling logic

## High-Level Runtime Flow
1. App/backend requests `spawn`.
2. Lifecycle reserves session ID, invokes backend spawn, validates handle.
3. Lifecycle starts one ingestion task for backend stream.
4. Each backend event is wrapped as `WorkerEventEnvelope` and published into eventbus.
5. Eventbus fans out to:
- per-session subscriptions
- global subscriptions
6. On `Done`/`Crashed`:
- lifecycle transitions terminal state
- lifecycle tears down scheduler entries for that session
- eventbus streams eventually close naturally
7. Scheduler periodically submits checkpoint prompt requests through lifecycle control APIs, skipping focused sessions.

## Scaling Model (200 Workers)
- One stream-ingestion task per active session.
- Shared eventbus with bounded buffers.
- Shared scheduler task for all periodic actions.
- No per-session periodic timer tasks.
- Hot-path locking minimized:
  - avoid global write locks in fanout path
  - use atomics for high-frequency counters
- Memory safety controls:
  - fixed-size buffers
  - explicit drop accounting
  - no unbounded queue growth

## Backpressure and Lag Policy
Default policy:
- Session stream lag:
  - dropped events are counted
  - inject synthetic truncation marker event to session stream stderr output
- Global stream lag:
  - dropped events counted
  - no synthetic output event; subscriber continues from latest available
- `publish` never blocks indefinitely waiting for slow subscribers.

Perf counters:
- events received/published/dropped
- lagged errors by stream class
- truncation marker count
- clone ops count
- dispatch nanos
- receiver counts

## Scheduler Design
- Implement shared scheduler with central tick loop and min-heap timing queue.
- Task descriptor:
  - task ID
  - target selector (session ID or selector fn)
  - interval
  - action kind
  - failure policy (retry, backoff, disable)
- Checkpoint action:
  - skip if target session focused
  - skip if session not running
  - send prompt bytes via lifecycle `send_input`
  - on repeated failures: disable task for that session and emit diagnostic metric/event

## Public API Changes
Current imports from `orchestrator-runtime` in:
- `orchestrator-app`
- `orchestrator-ui`
- `backend-opencode`
- `backend-codex`

will be replaced with imports from the new crates.

Breaking changes expected:
- type and trait names will change to new crate namespaces
- `WorkerBackend` monolith trait replaced by trait set:
  - `WorkerSessionControl`
  - `WorkerSessionStreamSource`
  - `WorkerBackendInfo`
- runtime manager shape replaced by composed runtime facade

No compatibility wrapper is planned.

## Migration Plan (Big-Bang)
### Phase 1: Scaffold and Protocol Extraction
- Add new crates to workspace.
- Move/copy protocol types and error/result model into `orchestrator-worker-protocol`.
- Freeze old runtime crate (no new feature work).

### Phase 2: Backend Porting
- Update `backend-opencode` to implement new protocol traits.
- Update `backend-codex` to implement new protocol traits.
- Ensure backend contract tests compile and pass against protocol crate.

### Phase 3: Eventbus Implementation
- Build `orchestrator-worker-eventbus`.
- Port lag/truncation semantics and clone optimization behavior.
- Implement global + session subscriptions.
- Add eventbus-focused tests.

### Phase 4: Lifecycle Implementation
- Build `orchestrator-worker-lifecycle`.
- Implement registry transitions and control APIs.
- Hook stream ingestion into eventbus publisher.
- Add lifecycle-focused tests.

### Phase 5: Scheduler Implementation
- Build shared `orchestrator-worker-scheduler`.
- Implement checkpoint scheduling policy and focus suppression.
- Integrate scheduler cancellation on terminal session states.

### Phase 6: Composition and Integration
- Build `orchestrator-worker-runtime` facade.
- Replace old runtime stream coordinator wiring in app.
- Update merged orchestrator-app kernel module imports/exports.
- Update `orchestrator-ui` runtime test stubs and imports.

### Phase 7: Removal and Cleanup
- Remove `crates/orchestrator-runtime`.
- Remove old references from all Cargo manifests and source files.
- Delete dead tests and utilities tied to old crate.

### Phase 8: Verification
- Run full workspace build and tests.
- Validate no compiler warnings.
- Run perf scenarios at 50/100/200 worker loads.

## Test Plan
### Unit Tests
Lifecycle:
- spawn success path
- spawn failure rollback
- session-id mismatch handling
- kill behavior and post-kill input rejection
- focus switching semantics across multiple sessions
- terminal transition handling

Eventbus:
- publish -> session/global fanout correctness
- lag handling for session subscribers
- lag handling for global subscribers
- truncation marker emission semantics
- receiver-count dependent clone behavior

Scheduler:
- periodic task dispatch timing behavior
- focus suppression logic
- terminal session cancellation
- repeated send failure policy behavior

### Integration Tests
- Harness provider contract tests (OpenCode and Codex implementations) against new protocol traits.
- end-to-end runtime flow:
  - spawn -> output -> done
  - spawn -> stream error -> crashed
  - checkpoint prompt in background, pause while focused

### Performance Tests
Ignored-by-default perf tests:
- fanout overhead at 50, 100, 200 sessions
- scheduler overhead at 50, 100, 200 sessions
- subscription churn with repeated subscribe/unsubscribe cycles
- 5-minute soak at 200 sessions with bounded memory assertions

## Acceptance Criteria
- `crates/orchestrator-runtime` removed from workspace.
- New crates exist with clean single-responsibility boundaries.
- `orchestrator-app`, `orchestrator-ui`, and harness backends compile against new crates only.
- Full tests pass; no compiler warnings.
- Behavior parity for required semantics:
  - done/crashed lifecycle behavior
  - session/global subscription behavior
  - focus-aware checkpoint suppression
  - lag truncation marker behavior for session streams
- Perf suite demonstrates stable behavior at 200 workers.

## Risks and Mitigations
Risk: Big-bang cutover causes broad compile breakage.
- Mitigation: strict phase ordering, landable intermediate branches, nightly rebase discipline.

Risk: Event semantics drift during rewrite.
- Mitigation: preserve behavior tests first; port existing expected semantics into new test suites.

Risk: Scheduler starvation under heavy load.
- Mitigation: bounded work-per-tick, queue metrics, and fairness checks in perf tests.

Risk: Backend trait split introduces integration regressions.
- Mitigation: harness provider contract tests run before orchestrator-app integration.

## Defaults Locked for This Plan
- Migration style: big-bang cutover.
- Scale target: 200 concurrent workers.
- Compatibility policy: no compatibility shim for old runtime APIs.
- Event durability: dedicated persistence-layer ownership only (post-normalization).
- Crate topology: protocol + lifecycle + eventbus + scheduler + runtime composition.

---

## Provider Crate Consolidation Plan
This section captures the next refactor wave: consolidate provider implementations by provider type, with one crate per type and explicit interfaces inside each crate.

### Goals
- Replace fragmented provider crates with type-grouped crates.
- Enforce a clean structure inside each crate:
  - `interface` module with required trait contracts and shared provider DTOs.
  - `providers` module with isolated per-provider implementations.
- Keep providers isolated from each other even when in the same crate.
- Use explicit provider factories (no dynamic registry).

### Locked Decisions
- Migration strategy: big-bang cutover.
- Interface ownership: interfaces live in each provider-type crate, not in orchestrator-app kernel modules.
- VCS shape: keep separate interfaces for local VCS and repo/code-host APIs.
- Target crate set: 4 crates.
- Crate naming: short names (`orchestrator-*` style), not `orchestrator-providers-*`.
- Provider enum ownership: move provider-kind enums to provider-type crates.
- Provider selection keys: rename to namespaced values.
- Harness sequencing: target the new runtime protocol directly.
- Cycle break approach: provider DTOs/errors move fully into provider crates; orchestrator-app error boundary adapts at integration edges.

### Target Provider Crates
1. `orchestrator-harness`
- Interface module for harness providers and harness events/session control contracts.
- Providers:
  - `opencode`
  - `codex`

2. `orchestrator-ticketing`
- Interface module for ticketing providers and ticketing DTOs.
- Providers:
  - `linear`
  - `shortcut`

3. `orchestrator-vcs`
- Interface module for local VCS/worktree operations.
- Providers:
  - `git_cli`

4. `orchestrator-vcs-repos`
- Interface module for remote repository/code-host operations.
- Providers:
  - `github_gh_cli`

Out of scope in this phase:
- `orchestrator-supervisor` remains separate.

### Current -> Target Mapping
- `backend-opencode` -> `orchestrator-harness::providers::opencode`
- `backend-codex` -> `orchestrator-harness::providers::codex`
- `integration-linear` -> `orchestrator-ticketing::providers::linear`
- `integration-shortcut` -> `orchestrator-ticketing::providers::shortcut`
- `integration-git` -> `orchestrator-vcs::providers::git_cli`
- `orchestrator-github` -> `orchestrator-vcs-repos::providers::github_gh_cli`

### Internal Layout Standard (All Provider-Type Crates)
- `src/interface/mod.rs`: trait(s), provider kind enum, shared request/response models, crate-local error type.
- `src/providers/<provider_name>/...`: provider-specific implementation, config, transport, tests.
- `src/factory.rs`: explicit provider name to implementation constructor mapping.
- `src/lib.rs`: re-export interface items and selected provider types.

### Cross-Crate Dependency Rules
- Provider crates must not depend on orchestrator-app kernel modules.
- Merged orchestrator-app depends on provider crates for provider interfaces and provider DTOs.
- Orchestrator-app error boundary converts provider crate errors into app-level orchestration errors at integration points.
- Harness crate depends on the new runtime protocol crate from the runtime decomposition effort.

### Public Interface Ownership After Consolidation
- `orchestrator-harness` owns:
  - harness provider traits
  - harness provider kind enum
  - harness spawn/session/event contract types
- `orchestrator-ticketing` owns:
  - ticketing provider trait
  - ticketing provider kind enum
  - ticketing request/response models
- `orchestrator-vcs` owns:
  - local VCS provider trait
  - worktree/repository request/response models
- `orchestrator-vcs-repos` owns:
  - code-host/repo provider traits
  - repo provider kind enum
  - pull request/review request/response models

### Provider Selection Key Migration
Update config/CLI selection values to namespaced keys:
- Harness:
  - `harness.opencode`
  - `harness.codex`
- Ticketing:
  - `ticketing.linear`
  - `ticketing.shortcut`
- VCS:
  - `vcs.git_cli`
- VCS repos:
  - `vcs_repos.github_gh_cli`

### Migration Plan (Provider Consolidation)
#### Phase 1: Scaffold New Crates
- Add `orchestrator-harness`, `orchestrator-ticketing`, `orchestrator-vcs`, `orchestrator-vcs-repos`.
- Add baseline module structure (`interface`, `providers`, `factory`).

#### Phase 2: Move Interfaces and DTOs
- Move provider traits and provider DTOs from the current `orchestrator-core` code into target provider crates.
- Introduce provider crate-local error types.
- Add conversion paths into orchestrator-app error boundary.

#### Phase 3: Port Implementations
- Move and isolate existing implementations under new `providers/*` modules.
- Preserve behavior and config validation semantics during move.

#### Phase 4: Rewire Composition Root
- Update `orchestrator-app` imports and factories to new crates.
- Switch provider matching to namespaced keys.
- Keep explicit match-based provider construction.

#### Phase 5: Delete Replaced Crates
- Remove old crates and references:
  - `backend-opencode`
  - `backend-codex`
  - `integration-linear`
  - `integration-shortcut`
  - `integration-git`
  - `orchestrator-github`

#### Phase 6: Verification and Cleanup
- Run full workspace build and tests.
- Remove dead imports and obsolete docs/config references.
- Ensure no compiler warnings.

### Provider Consolidation Test Plan
#### Unit Tests
- Factory resolution tests per crate (valid/invalid provider keys).
- Provider config validation tests per implementation.
- Interface contract behavior tests per implementation.

#### Integration Tests
- App startup wiring selects correct provider implementations by namespaced key.
- Harness lifecycle flows work through `orchestrator-harness` interface.
- Ticketing flows work for linear and shortcut through shared ticketing interface.
- VCS flows work for local git/worktrees.
- VCS repo flows work for GitHub PR/review operations.

#### Regression and Compatibility Checks
- Serialization and persistence checks for moved provider kind enums/types.
- Existing app/ui tests (plus migrated former core tests) updated to import new provider crates.
- Ensure behavior parity for existing provider operations after move.

### Provider Consolidation Acceptance Criteria
- Exactly four provider-type crates own provider interfaces and implementations.
- Provider interfaces/DTOs are no longer owned by the legacy `orchestrator-core` code.
- Old provider crates are removed from workspace and Cargo manifests.
- Orchestrator-app wiring uses only new provider crates.
- Namespaced provider keys are fully wired in config and CLI.
- Full tests pass and no compiler warnings remain.

---

## Orchestrator-App + Orchestrator-Core Consolidation Plan
This section captures the structural merge: delete `orchestrator-core` as a crate, move remaining kernel logic into `orchestrator-app`, and enforce domain-first module boundaries.

### Goals
- Remove `crates/orchestrator-core` as a standalone crate.
- Keep `orchestrator-app` as the single orchestration shell crate.
- Split logic into clear top-level modules by domain.
- Keep provider/runtime/workflow/store ownership in their dedicated crates.

### Target Top-Level Modules in `orchestrator-app`
- `bootstrap`: startup/shutdown, config/env load, process lifecycle wiring.
- `composition`: dependency wiring and provider/runtime/workflow factory assembly.
- `controller`: frontend intents, command routing, runtime stream/event coordination.
- `jobs`: background pollers/reconcilers and task lifecycle management.
- `events`: canonical orchestration event schema/types used by the app kernel.
- `normalization`: runtime/provider event translation into canonical events.
- `projection`: event replay/apply and read-model state materialization.
- `attention`: inbox prioritization/scoring and batch surfacing.
- `commands`: stable command IDs, registry, parsing, and invocation types.
- `frontend`: frontend controller contracts/event stream models shared with UI.
- `error`: app-level orchestration error surface and cross-crate conversions.

### Module Boundary Rules
- `events` contains schema contracts only, not scoring/projection.
- `normalization` performs translation only, no persistence.
- `projection` performs state materialization only, no prioritization.
- `attention` performs scoring/ranking only, no event parsing.
- `composition` wires dependencies only, no business-rule ownership.
- `controller` coordinates flow execution, while business rules remain in dedicated workflow/ticket crates.

### What Moves Out (and stays out)
- Provider interfaces/DTOs: provider-type crates.
- Runtime contracts/lifecycle/fanout/scheduler: worker runtime crates.
- Workflow/ticket-selection logic: dedicated workflow/ticket crates.
- Event-store APIs/implementations: dedicated persistence crate(s).

### Migration Plan (Core/App Merge)
#### Phase 1: Scaffold Domain-First Module Tree
- Create top-level module layout under `crates/orchestrator-app/src/` per target list.
- Replace include-based aggregation with explicit module declarations.

#### Phase 2: Move Kernel Modules from `orchestrator-core` into `orchestrator-app`
- Move:
  - events schema
  - normalization logic
  - projection logic
  - attention/inbox logic
  - command registry/types
  - frontend contract types
  - core error types/conversions (renamed to app error boundary)

#### Phase 3: Rewrite Workspace Imports
- Replace all `orchestrator-core` imports with:
  - `orchestrator-app` kernel modules, or
  - new owning crates (providers/runtime/workflow/persistence).

#### Phase 4: Delete `orchestrator-core` Crate
- Remove crate from workspace and manifests.
- Remove obsolete re-exports and compatibility glue.

#### Phase 5: Verification
- Full workspace build/tests.
- Ensure no compiler warnings.
- Validate behavior parity for normalization/projection/attention/command/frontend contracts.

### App/Core Consolidation Test Plan
#### Unit Tests
- `events`: serialization/deserialization stability.
- `normalization`: mapping coverage for runtime/provider event variants.
- `projection`: apply/rebuild determinism and state correctness.
- `attention`: priority scoring and batch ordering determinism.
- `commands`: command ID uniqueness and parse/roundtrip behavior.
- `frontend`: event/intent schema roundtrip behavior.

#### Integration Tests
- Startup composition and controller flow wiring.
- Runtime event -> normalization -> projection -> attention pipeline behavior.
- Frontend controller stream behavior and command dispatch behavior.

### App/Core Consolidation Acceptance Criteria
- `orchestrator-core` crate removed from workspace.
- `orchestrator-app` contains the agreed top-level module boundaries.
- No legacy `orchestrator-core` imports remain.
- Full tests pass and no compiler warnings remain.

---

## Configuration Centralization + Strict MVC UI Plan
This section captures the final structural changes for config ownership and UI boundaries.

### Goals
- Centralize configuration into a single canonical owner.
- Remove duplicated config state/global setters across app and UI.
- Enforce strict MVC boundaries where `orchestrator-ui` is a view-only layer.
- Move all non-UI orchestration/business logic out of `orchestrator-ui`.

### Locked Decisions
- Config canonical home: new `orchestrator-config` crate.
- UI boundary: strict MVC (view-only UI crate).
- UI action contracts live in `orchestrator-app::controller::contracts`.
- UI config consumption via explicit constructor injection (no UI config singleton).
- Migration cadence: three waves by concern.

### Current-State Gaps (Observed)
- Config is currently split:
  - canonical TOML loading in app bootstrap
  - additional crate-local runtime config globals in app and UI
- UI crate still owns non-view concerns:
  - orchestration/provider action traits
  - workflow/merge/inbox action execution
  - polling/reconcile business logic

### Target Architecture
#### `orchestrator-config` crate (new)
Owns:
- `OrchestratorConfig` schema (all runtime/provider/ui/database settings).
- path/env resolution (`ORCHESTRATOR_CONFIG`).
- parse/normalize/default behavior.
- typed config slices for consumers.

Public API:
- `load_from_env() -> Result<OrchestratorConfig, ConfigError>`
- `load_from_path(path) -> Result<OrchestratorConfig, ConfigError>`
- strongly typed sub-config views, including `UiViewConfig`.

#### `orchestrator-app` (controller/contracts owner)
Owns:
- UI-facing action contracts in `controller::contracts`.
- orchestration action execution (workflow, merge, inbox, polling, supervisor command dispatch).
- dependency wiring from config to runtime/provider/controller layers.

#### `orchestrator-ui` (view-only)
Owns only:
- rendering/layout/theming.
- input handling and view-state transitions.
- local ephemeral UI state needed for interaction.

Does not own:
- provider orchestration traits.
- workflow/merge/inbox command execution.
- business-policy decisions.
- config loading/global config stores.

### MVC Boundary Rules
- UI emits intents and renders state.
- Controller executes business/orchestration actions and emits UI-consumable events/state updates.
- UI never directly calls provider/runtime/workflow business APIs.
- View modules are pure presentation/interaction modules.

### UI Domain Module Split (Target)
- `ui/bootstrap`: terminal lifecycle + main UI loop bootstrap.
- `ui/view_state`: pure UI state structs/caches.
- `ui/input`: key routing + mode routing + command translation.
- `ui/render`:
  - `render/inbox`
  - `render/sessions`
  - `render/terminal`
  - `render/inspectors`
  - `render/overlays`
- `ui/features`:
  - `features/ticket_picker_view`
  - `features/supervisor_view`
  - `features/terminal_view`
  - `features/merge_review_view`
- `ui/theme`
- `ui/tests`

Structural constraints:
- remove `include!`-based flattening from UI crate root.
- use explicit `mod` hierarchy and bounded module visibility.

### Migration Plan (Three Waves)
#### Wave 1: Config Centralization
- Create `orchestrator-config`.
- Move config schema/path/env/default/normalize logic into it.
- Replace app/UI runtime config global setters with injected typed config.
- Inject `UiViewConfig` into `Ui` constructor/run path.

#### Wave 2: Strict MVC Extraction
- Move non-view traits/contracts out of UI into `orchestrator-app::controller::contracts`.
- Move orchestration action runners (workflow/merge/inbox/polling/supervisor command execution) out of UI.
- Keep UI on view intents + state rendering only.

#### Wave 3: UI Module Reorganization
- Split UI into domain modules listed above.
- Separate rendering/input/view-state/features by ownership.
- Trim UI dependency surface to view contracts + read model/event interfaces.

### Public API Changes
- Remove UI global runtime config API from `orchestrator-ui`.
- Add explicit UI config injection API:
  - `Ui::init_with_config(ui_config: UiViewConfig, ...)` (exact signature finalized during implementation).
- Add controller contract API module in app:
  - `orchestrator-app::controller::contracts`.
- New config API crate:
  - `orchestrator-config::{OrchestratorConfig, ConfigError, load_from_env, load_from_path}`.

### Test Plan
#### Wave 1 (config)
- config load from default path and explicit `ORCHESTRATOR_CONFIG`.
- normalize/default parity tests.
- invalid TOML and invalid value diagnostics tests.

#### Wave 2 (MVC boundary)
- UI compile-time checks: no orchestration action contracts remain in UI.
- controller contract integration tests:
  - workflow advance
  - merge finalize
  - inbox publish/resolve
  - supervisor command dispatch

#### Wave 3 (UI module split)
- rendering goldens by render submodule.
- input-routing tests by mode/overlay.
- view-state regression tests for terminal/sessions/inbox/inspector flows.

### Acceptance Criteria
- `orchestrator-config` is the only configuration source of truth.
- UI receives config only via explicit injection (no UI config singleton/global setters).
- `orchestrator-ui` contains only view concerns.
- All non-UI orchestration/business logic moved to app controller or dedicated owning crates.
- UI module tree is domain-structured and no `include!` flattening remains.
- full workspace tests pass with no compiler warnings.
