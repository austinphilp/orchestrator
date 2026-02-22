# Current Gaps, Dormant Capabilities, and Forward Path

## Scope

- This document identifies features/architecture that appear to be unutilized or only partially utilized in the current runtime wiring.
- It also proposes a concrete path forward to simplify the system and increase delivery velocity.
- Assumption: the harness migration plan in `docs/harness-server-migration-plan.md` is partially complete (runtime + UI streaming migration completed; backend server-mode migration still in progress).

## Dormant or partially utilized areas

## 1) Core workflow automation planner is present but not clearly wired into runtime

- `orchestrator-domain/src/workflow_automation.rs` contains automation planning/policy structures.
- Runtime command and UI flow appear centered on direct command dispatch paths rather than planner-driven execution.
- Impact:
  - Planning logic can drift from actual behavior.
  - Team pays maintenance cost for logic that may not govern production flow.

## 2) Runtime worker/session manager migration is now active but still in cleanup phase

- `orchestrator-runtime/src/worker_manager.rs` is now wired into app startup/runtime through the `WorkerManagerBackend` adapter.
- UI terminal flow is fed through frontend-controller events and no longer directly subscribes to backend terminal streams.
- Remaining work:
  - remove stale fallback paths and tests that model deprecated UI-direct backend behavior,
  - document the final ownership boundary between runtime coordinator, app frontend controller, and UI projection/render loop.

## 3) GitHub adapter capabilities exceed currently exposed app command surface

- `orchestrator-github` implements multiple operations (draft PR, mark ready, reviewer requests, review queue support).
- App runtime command dispatch appears to expose only a subset (`workflow.approve_pr_ready`, `github.open_review_tabs`).
- Impact:
  - Useful capabilities exist but are unreachable from normal workflow.
  - Inconsistent expectation between adapter surface and product behavior.

## 4) Linear workflow sync helper appears not integrated into primary orchestration loop

- `integration-linear` contains workflow transition sync facilities.
- Wiring from app/core transition pipeline into these sync helpers is not clearly present in primary flow.
- Impact:
  - Possible mismatch between local workflow state and Linear ticket state.
  - Additional hidden/manual sync burden.

## 5) Supervisor toolset is incomplete relative to provider abstraction

- Provider abstractions support full ticket operations, including creation.
- Supervisor command tools in app dispatch currently include read/update/comment flows but no clear `create_ticket` tool.
- Impact:
  - Supervisor cannot execute full ticket lifecycle from conversation.
  - Human/manual step remains for ticket creation.

## 6) Backend capability metadata appears broader than active product usage

- OpenCode backend supports optional capabilities and structured events.
- Not all advertised capability branches appear consumed by UI/app behavior.
- Impact:
  - Interface surface area is larger than currently needed.
  - Harder to reason about what is guaranteed vs best-effort.

## 7) Post-migration legacy artifacts remain in tree

- Backend server/session migration is now the primary remaining legacy area; other PTY/snapshot-era artifacts have been removed from terminal/runtime paths.
- Impact:
  - Architectural narrative does not match implementation footprint.
  - Increases risk of accidental coupling to stale paths.

## What this means right now

The codebase already has a strong modular foundation, but it is in a transitional shape:

1. Stable core architecture exists (event/projection-driven orchestration).
2. Product-visible behavior is narrower than internal adapter/capability surface.
3. Legacy/transitional runtime structures still increase cognitive load and maintenance risk.

## Recommended forward path

## Phase 1: Define and enforce the active architecture boundary

1. Decide the authoritative execution model (post-migration backend flow).
2. Mark non-authoritative runtime paths as deprecated in-code.
3. Add one architecture decision record in `docs/` naming active vs legacy paths.

## Phase 2: Align exposed features with implemented adapters

1. Choose for each dormant capability: `wire` or `delete`.
2. If keeping GitHub extended operations, expose them through command dispatch and UI affordances.
3. If keeping supervisor-first workflows, add missing high-value tools (for example ticket creation).

## Phase 3: Close provider synchronization gaps

1. Wire core workflow transitions to provider sync hooks (especially Linear).
2. Document mapping contracts and failure behavior near integration code and in docs.

## Phase 4: Remove dead/transitional code aggressively

1. Remove migration leftovers that are no longer operationally used.
2. Trim backend capability branches that have no consumer.
3. Keep only abstractions with at least one active production path.

## Phase 5: Guardrails for future changes

1. Require every new feature to identify:
  - owning crate
  - event/projection impact
  - external integration impact
  - env var additions in `AGENTS.md` and `.env-template`
2. Keep docs in lockstep with architectural decisions to prevent drift.

## Suggested immediate decisions

1. Confirm whether `workflow_automation` should become active or be removed.
2. Confirm whether `WorkerManager` remains part of long-term runtime.
3. Confirm desired GitHub command surface in product UI.
4. Confirm supervisor scope: assistive-only vs full ticket lifecycle actor.
5. Confirm cleanup timeline for post-migration legacy code.
