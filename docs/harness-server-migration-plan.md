# Plan: Migrate session execution from PTY to server-backed harnesses

- Status: Draft implementation plan
- Target branch: migration to server-backed Opencode/Codex runtime
- Owner: orchestrator-runtime, backend-opencode, backend-codex, orchestrator-ui

## Current implementation status (as of 2026-02-18)

- [x] Runtime API and `WorkerManager` polling/snapshot cache path removed.
- [x] UI terminal view switched to stream-driven output append buffer.
- [x] Snapshot contract removed from `WorkerBackend` trait and stubs.
- [x] Terminal output wiring in golden fixtures updated for streaming UX.
- [ ] Backends still use process-spawn execution with stream parsing; true server/session/thread API migration remains.

## 0) Execution model

- Implement in phases behind clear commits so each phase can be validated independently.
- Prefer server-side session semantics over terminal emulation; do not preserve PTY behaviors unless they map directly to stream events.
- Backward compatibility is not required unless explicitly approved by users.

### 0.1 Immediate decision checklist

- [ ] Confirm Opencode and Codex server docs for session/thread + stream endpoints.
- [ ] Add explicit stream event schema in shared runtime/core types before backend implementation.
- [ ] Decide whether backends share a common normalization helper or each keeps its own converter.
- [ ] Agree on migration flag strategy (`runtime mode` toggle?) before removing PTY paths.

## 1) Objective

Replace PTY-centric runtime session execution with server-backed harness sessions for both Opencode and Codex, using streaming events for near-live terminal UX.

## 2) Current state summary

Prior versions were snapshot- and PTY-based across:

- runtime (`snapshot`, `PtyManager`, terminal emulator)
- backends (`backend-opencode` and `backend-codex` thin PTY wrappers)
- UI terminal rendering (`TerminalSnapshot` polling and static redraw assumptions)

Current status in this migration step:

- The runtime and UI layers are snapshot-free and event-stream driven.
- Backends parse command output streams and emit normalized `BackendEvent` variants directly, without terminal snapshot polling.
- Remaining work is to re-home session lifecycle onto dedicated server/session/thread APIs for each harness.

## 3) Target architecture

- Introduce one long-lived server process per app/backend (or reuse where already guaranteed) for session orchestration.
- Maintain per-session identifiers and reuse server sessions/threads for that runtime session.
- Consume server event streams as the canonical live output source.
- Normalize raw server events into a single `BackendEvent` domain before fan-out.
- Feed normalized events through a shared runtime event bus and dispatch to session/UI subscriptions.

## 4) Concrete code changes

### 4.1 Runtime crate (`crates/orchestrator-runtime`)

1. Remove `TerminalSnapshot` from public runtime API.
2. Remove `snapshot` from `WorkerBackend` trait.
3. Remove `mod pty_manager;`, `mod terminal_emulator;` and all re-exports.
4. Remove snapshot polling/cache machinery from `worker_manager.rs`:
   - remove `CachedSnapshot`
   - remove `snapshot` and interval-throttle config entries tied to snapshots
   - remove background snapshot refresh calls in tests
5. Keep event forwarding as primary contract:
   - backend events are subscribed and forwarded in real-time.
   - session/global event subscriptions remain available.
6. Keep/retain checkpoints, kills, status tracking and lifecycle events.

### 4.2 Opencode backend (`crates/backend-opencode`)

1. Replace PTY-based process with one Opencode server client/worker that:
   - starts/reuses an opencode server process
   - creates sessions/threads via server REST (or equivalent documented API)
   - subscribes to server event stream (SSE if available)
2. `spawn` creates and tracks opencode session ID; map to runtime session IDs.
3. `send_input` posts user message to session endpoint.
4. `kill` closes session cleanly via server API.
5. Parse/normalize server payloads into `BackendEvent`.
6. Stream those events through the existing backend subscription interface.

### 4.3 Codex backend (`crates/backend-codex`)

1. Replace PTY execution with Codex app-server integration.
2. Manage one app-server process for the app lifetime, with per-session thread IDs.
3. `spawn` initializes/ensures thread handle.
4. `send_input` sends messages over app-server protocol.
5. `kill` terminates session/thread.
6. Consume server event stream and normalize into `BackendEvent` variants.

### 4.4 UI (`crates/orchestrator-ui`)

1. Replace snapshot-driven terminal rendering with live event rendering.
2. Maintain output buffer per session and append new `Output` events in order.
3. Keep bottom input area in terminal focus mode:
   - `Enter` submits input to selected session
   - clear composer on submit
4. Remove snapshot polling calls and update key handling to avoid per-key passthrough.
5. `x` remains session kill.
6. Keep session list grouping by repo while listing open sessions.

### 4.5 Core/app/test adapters

1. Remove test-only `snapshot` implementations from stubs.
2. Replace snapshot-dependent tests with stream/event-driven assertions.
3. Add/update tests for:
   - live stream delivery
   - session isolation across multiple sessions
   - normalized event shape per backend contract
4. Remove PTY-specific test data and helper modules tied to snapshots.

## 9) Execution sequence (recommended)

1. Runtime API and event bus
   - Remove `TerminalSnapshot`, `snapshot` and PTY modules.
   - Collapse snapshot polling paths.
   - Introduce/verify a stable event shape for runtime consumers.
   - Files: `crates/orchestrator-runtime/src/lib.rs`, `crates/orchestrator-runtime/src/worker_manager.rs`, exports from `crates/orchestrator-domain/src/lib.rs`.
2. Backend protocol migration
   - Update Opencode backend first (session lifecycle + event stream).
   - Update Codex backend next.
   - Files: `crates/backend-opencode/src/lib.rs`, `crates/backend-codex/src/lib.rs`, test doubles in `crates/backend-*/tests`.
3. UI stream consumption
   - Replace snapshot-driven refresh with event append buffer.
   - Keep focused input interaction and kill semantics unchanged.
   - File: `crates/orchestrator-ui/src/lib.rs`.
4. Test and fixture migration
   - Move snapshot assertions to stream/event assertions.
   - Remove PTY-only fixtures/modules.
   - Files across `crates/*/tests` and golden fixtures touched by terminal assertions.
5. Dependency cleanup and stabilization
   - Remove PTY crates.
   - Update lockfiles if the workspace has them.
   - Run a final sweep for stale snapshot references.

## 10) Risk log

- Streaming event ordering differences may affect golden terminal snapshots.
- Session ID translation (runtime session id ↔ server thread/session id) can drift if lifecycle paths are incomplete.
- Mixed backend event payloads can desynchronize if normalization is centralized too late.
- Backward assumptions in app tests may fail once `snapshot` is removed from stubs.

## 11) Rollback strategy

- Keep PTY implementation available in git history until build-and-runtime checks pass.
- If runtime breakage appears, restore snapshot-based behavior only from the previous commit and migrate one backend at a time.

## 12) Definition of done

1. All PTY references removed from runtime and backends.
2. Snapshot API and tests removed from all `WorkerBackend` implementers and adapters.
3. Live stream is the only source of terminal output in UI.
4. Session lifecycle (spawn/submit/kill) stays stable across backends.
5. Build and test gates for changed crates pass with updated golden/event assertions.

## 5) Public API changes

- `WorkerBackend`:
  - remove `snapshot(&self, session) -> RuntimeResult<TerminalSnapshot>`
- `WorkerManager`:
  - remove snapshot APIs and configuration tied to snapshot polling
- Runtime exports:
  - remove `TerminalSnapshot` export
- Backend-specific session tracking structures:
  - store server session/thread IDs instead of PTY state handles.

## 6) Dependency cleanup

- Remove unused PTY dependencies and modules from runtime:
  - `portable-pty`
  - `vt100`
  - `pty_manager.rs`, `terminal_emulator.rs`
- Add required HTTP/event-stream dependencies as needed for server mode.

## 7) Acceptance criteria

1. `cargo build` succeeds (workspace) without snapshot/PTY references.
2. No PTY-only code paths remain in runtime and backends.
3. Opening terminal view shows streaming output, not static snapshots.
4. `Enter` submits messages through selected session stream.
5. `x` closes selected session and kills underlying processes.
6. Event bus remains usable with multiple simultaneous sessions and mixed backends.

## 8) Assumptions

- Opencode and Codex server modes expose documented session/thread creation and event-stream endpoints compatible with this architecture.
- Existing event normalization path (`BackendEvent` + core normalization utilities) is retained and extended, not replaced.
- Streamed outputs are sufficient for terminal display in this phase; no PTY cursor/geometry rendering is required immediately.
