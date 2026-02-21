# AP-260: Background Session Stream Alternatives

## Scope

This document investigates alternatives to always-subscribed background session streams for orchestrator terminal sessions.

Ticket: AP-260 ([Linear](https://linear.app/austinphilp/issue/AP-260/perfinvestigation-background-session-stream-alternatives))

## Current architecture (as implemented)

1. UI-level per-session subscriptions are opened in `UiShellState::ensure_terminal_stream_and_report`.
   - File: `crates/orchestrator-ui/src/ui/shell_state.rs`
   - Behavior: once a session stream is opened, it remains subscribed until stream end/failure/archive.
2. Background output is deferred then periodically flushed by UI refresh interval.
   - File: `crates/orchestrator-ui/src/ui/shell_state.rs`
   - Related config: `ui.background_session_refresh_secs` (2-15s clamp, default 15s).
3. `WorkerManager` already supports centralized stream multiplexing (`subscribe_all` and per-session fanout), but app/UI runtime path does not currently use it.
   - File: `crates/orchestrator-runtime/src/worker_manager.rs`
   - App path currently wires UI directly to backend (`with_worker_backend(...)`) in `crates/orchestrator-app/src/main.rs`.
4. Backend details:
   - OpenCode: session relay task per spawned session; backend `subscribe` is broadcast receiver attach only.
     - File: `crates/backend-opencode/src/lib.rs`
   - Codex: single app-server reader loop plus per-session broadcast with bounded replay history.
     - File: `crates/backend-codex/src/lib.rs`

## Alternatives evaluated

### A) Active-session-only subscriptions

Only active/focused session is subscribed. Background sessions do not stream output/events continuously.

### B) Multiplexed coordinator

Use a runtime-owned coordinator as single subscription surface for UI. Keep session ingest centralized and fan out status/output with bounded policy.

### C) Hybrid status channel + deferred output fetch

Keep lightweight always-on status feed (turn state, needs input, done/crashed), but fetch output on demand (active session switch/manual expand).

## Decision matrix

Scoring legend: `Low` is best for cost, best for risk surface; `High` is worst for cost/risk. Latency row is user-perceived timeliness where `Low` means low latency (best).

| Dimension | A: Active-only | B: Multiplexed coordinator | C: Hybrid status + deferred fetch |
|---|---|---|---|
| CPU overhead (multi-session) | Low | Medium-Low | Low |
| Memory overhead (steady state) | Low | Medium | Low-Medium |
| Active session output latency | Low | Low | Medium (fetch/hydration path) |
| Background status latency (`NeedsInput`, `Done`, `Crashed`) | High risk (unless separate status path added) | Low | Low |
| Implementation complexity | Medium | Medium | High |
| Backend parity risk (OpenCode + Codex) | Medium | Low-Medium | High |
| Failure/consistency risk | Medium-High | Low-Medium | High |
| Fit with current code shape | Medium | High | Low-Medium |

## Recommendation

Recommend **B: Multiplexed coordinator**.

Rationale:

1. Preserves low-latency status signaling for background sessions without requiring UI-owned stream task per viewed session.
2. Aligns with existing `WorkerManager` architecture that already models centralized ingest + multiplexing.
3. Avoids introducing new backend fetch APIs immediately, reducing parity and correctness risk.
4. Keeps active-session latency low and predictable while allowing bounded buffering policy for background output.

## Proposed target behavior

1. Runtime layer owns session ingest and multiplexing lifecycle.
2. UI consumes one coordinator feed instead of spawning per-session stream tasks directly.
3. Events split into two lanes:
   - Status lane (loss-intolerant): `TurnState`, `NeedsInput`, `Done`, `Crashed`, `Checkpoint`.
   - Output lane (loss-tolerant bounded): `Output` with explicit truncation markers if needed.
4. Background session output remains deferred in UI, but transport/subscription ownership moves out of UI.

## Migration plan

### Phase 1: Introduce coordinator plumbing

1. Wire app runtime to instantiate/use `WorkerManager` (or equivalent coordinator abstraction) as source of truth for stream delivery.
2. Preserve current `WorkerBackend` contract; no new backend API required in this phase.
3. Add targeted tests for global/per-session fanout behavior at runtime boundary.

### Phase 2: Move UI to coordinator feed

1. Replace direct `worker_backend.subscribe(...)` calls in `UiShellState` with coordinator subscriptions.
2. Keep existing terminal UX semantics:
   - offscreen needs-input does not auto-switch view,
   - deferred offscreen output flush behavior,
   - stream failure/end handling.
3. Add bounded output buffering policy and explicit overflow markers.

### Phase 3: Perf hardening and optional hybrid prep

1. Add lightweight internal metrics for session/event pressure (queue depth, dropped-output markers, fanout lag).
2. Validate thresholds under 1/5/20 session synthetic scenarios.
3. Re-evaluate Hybrid (C) only if coordinator path still exceeds target budget.

## Acceptance criteria mapping (AP-260)

1. Decision matrix with CPU/memory/latency tradeoffs: **covered** in table above.
2. Recommended architecture and migration plan documented: **covered** in recommendation and migration sections.
3. Follow-up implementation tickets proposed: **completed (`AP-270` to `AP-273`) and linked from AP-260**.

## Non-goals for AP-260

1. No runtime behavior changes in this ticket.
2. No new env vars in this ticket.
3. No full local build/test sweep (verification is intended for follow-up implementation PRs + CI).
