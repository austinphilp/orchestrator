# AP-272 Stream Pressure Instrumentation and Perf Thresholds

## Scope

Follow-up implementation for `AP-272` from the AP-260 migration plan.

Goals delivered:

- add stream-pressure instrumentation signals,
- provide reproducible 1/5/20 load runner,
- define explicit validation thresholds for CPU/RSS/latency regressions.

## Instrumentation added

`WorkerManager` perf snapshots now expose:

- fanout lag counters:
  - `subscriber_lagged_errors_session_total`
  - `subscriber_lagged_errors_global_total`
  - `subscriber_lagged_events_session_total`
  - `subscriber_lagged_events_global_total`
- queue pressure/drop counters:
  - `queue_pressure_dropped_events_total`
- output truncation marker counters:
  - `output_truncation_markers_total`
- per-session and global event-rate metrics:
  - `events_received_rate_per_sec_x1000`

Per-session lag now emits a truncation marker output event instead of failing the stream, so pressure is visible without tearing down UI event delivery.

## Reproducible scenario runners

- Pressure runner (1/5/20):
  - `cargo test -p orchestrator-runtime perf_stream_pressure_1_5_20_sessions -- --ignored --nocapture`
- Fanout runner (10/20/30 baseline for no-pressure path):
  - `cargo test -p orchestrator-runtime perf_fanout_overhead_10_20_30_sessions -- --ignored --nocapture`

## 2026-02-22 baseline

### Stream pressure (`perf_stream_pressure_1_5_20_sessions`)

| Sessions | Elapsed ms | `event_dispatch_nanos_total` | `queue_pressure_dropped_events_total` | `output_truncation_markers_total` |
|---|---:|---:|---:|---:|
| 1 | 102 | 81088 | 120 | 1 |
| 5 | 102 | 370401 | 728 | 5 |
| 20 | 102 | 1407861 | 3008 | 20 |

Shell-level runtime sample for the same run:

- Command:
  - `TIMEFMT='elapsed=%E user=%U sys=%S cpu=%P max_rss=%M'; time cargo test -p orchestrator-runtime perf_stream_pressure_1_5_20_sessions -- --ignored --nocapture`
- Observed:
  - `elapsed=0.61s`
  - `cpu=24%`
  - `max_rss=55` (zsh `%M` units)

### No-pressure fanout (`perf_fanout_overhead_10_20_30_sessions`)

| Sessions | Elapsed ms | `event_clone_ops_total` | `queue_pressure_dropped_events_total` | `output_truncation_markers_total` |
|---|---:|---:|---:|---:|
| 10 | 122 | 0 | 0 | 0 |
| 20 | 121 | 0 | 0 | 0 |
| 30 | 122 | 0 | 0 | 0 |

## Pass/fail thresholds

Use these thresholds for regression checks:

1. CPU/RSS envelope (pressure run):
- total wall-clock <= `1.0s`
- CPU <= `60%`
- max RSS <= `80` in zsh `%M` units

2. Latency envelope (pressure run):
- per scenario `elapsed_ms` <= `150`
- at 20 sessions, `event_dispatch_nanos_total` <= `2_000_000`

3. Pressure signal correctness (pressure run):
- `queue_pressure_dropped_events_total` > `0`
- `output_truncation_markers_total` >= session count for each run

4. No-pressure signal cleanliness (fanout run):
- `queue_pressure_dropped_events_total == 0`
- `output_truncation_markers_total == 0`
- `event_clone_ops_total == 0` in global-only fanout scenario

## Validation checklist

1. Run both ignored perf tests with `--nocapture`.
2. Compare JSON lines against thresholds above.
3. If any threshold fails, treat as perf regression and investigate fanout path, channel buffer sizing, and subscription lag behavior.
