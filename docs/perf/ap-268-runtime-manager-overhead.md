# AP-268 Runtime Manager Overhead Investigation

## Scope
- Runtime manager overhead instrumentation and measurement for:
  - per-session checkpoint prompting tasks
  - session/event fanout loops
  - subscription lifecycle churn
- Targeted concurrency scale: 10, 20, and 30 sessions.

## Commands
- Unit and behavior checks:
  - `cargo test -p orchestrator-runtime worker_manager_`
- Perf scenarios (ignored tests):
  - `cargo test -p orchestrator-runtime perf_ -- --ignored --nocapture`

## Metrics captured
- Checkpoint loop:
  - `checkpoint_ticks_total`
  - `checkpoint_skips_focused_total`
  - `checkpoint_send_attempts_total`
  - `checkpoint_send_failures_total`
  - `checkpoint_loop_read_lock_wait_nanos_total`
  - `checkpoint_send_nanos_total`
- Fanout/event loop:
  - `events_received_total`
  - `events_forwarded_session_total`
  - `events_forwarded_global_total`
  - `event_clone_ops_total`
  - `event_dispatch_nanos_total`
  - `session_receiver_count_last`
  - `global_receiver_count_last`
- Subscription lifecycle:
  - `subscribe_session_calls_total`
  - `subscribe_global_calls_total`
  - `subscribe_session_failures_total`
  - `subscriber_lagged_errors_session_total`
  - `subscriber_lagged_errors_global_total`
  - `subscriber_closed_session_total`
  - `subscriber_closed_global_total`

## Results table
| Scenario | Sessions | Elapsed ms | Key observations |
|---|---:|---:|---|
| checkpoint_overhead | 10 | 181 | `checkpoint_ticks_total=60`, `checkpoint_send_attempts_total=60` |
| checkpoint_overhead | 20 | 181 | `checkpoint_ticks_total=101`, `checkpoint_send_attempts_total=101` |
| checkpoint_overhead | 30 | 184 | `checkpoint_ticks_total=150`, `checkpoint_send_attempts_total=150` |
| fanout_overhead | 10 | 124 | `events_received_total=400`, `event_dispatch_nanos_total=295745` |
| fanout_overhead | 20 | 127 | `events_received_total=800`, `event_dispatch_nanos_total=574956` |
| fanout_overhead | 30 | 124 | `events_received_total=1200`, `event_dispatch_nanos_total=969701` |
| subscription_lifecycle_churn | 10 | 0 | `subscribe_session_calls_total=500`, `subscribe_global_calls_total=50` |

## Ranked options
1. Shared checkpoint scheduler replacing per-session ticker tasks.
- Benefit: High (checkpoint activity scales linearly with session count).
- Cost: Medium.
- Risk: Medium (requires scheduler lifecycle semantics and focus-aware routing).

2. Fanout payload de-duplication (shared payload strategy).
- Benefit: Medium (event dispatch nanos increase with event volume and receiver fanout).
- Cost: Medium.
- Risk: Medium (event ownership/thread-safety changes).

3. Lazy/lifecycle-aware subscription management.
- Benefit: Medium-low (churn counters are measurable and easy to cap).
- Cost: Low.
- Risk: Low.

## Follow-up tickets
Create implementation tickets in Linear after the above ranking is finalized, then link them back to AP-268.

## AP-276 follow-up outcome (2026-02-22)

`AP-276` optimized fanout dispatch to skip clone/send work when no corresponding receivers are attached.

### Re-run command

- `cargo test -p orchestrator-runtime perf_fanout_overhead_10_20_30_sessions -- --ignored --nocapture`

### Updated fanout results

| Scenario | Sessions | Elapsed ms | `event_dispatch_nanos_total` | Delta vs AP-268 |
|---|---:|---:|---:|---:|
| fanout_overhead | 10 | 122 | 196843 | -33.4% |
| fanout_overhead | 20 | 121 | 374801 | -34.8% |
| fanout_overhead | 30 | 122 | 550043 | -43.3% |

### Observations

- `event_clone_ops_total` is now `0` in global-only fanout scenarios (previously one clone per event).
- Terminal done/crashed handling semantics remain unchanged and existing lifecycle tests stayed green.
