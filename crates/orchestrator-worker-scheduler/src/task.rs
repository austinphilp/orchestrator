use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestrator_worker_lifecycle::{WorkerLifecycle, WorkerSessionState, WorkerSessionVisibility};
use orchestrator_worker_protocol::error::WorkerRuntimeError;
use orchestrator_worker_protocol::ids::WorkerSessionId;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

pub type WorkerSchedulerTaskId = u64;
const DEFAULT_DISABLE_AFTER_CONSECUTIVE_FAILURES: u32 = 3;
const WORKER_SCHEDULER_DIAGNOSTIC_BUFFER: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerTargetSelector {
    Session(WorkerSessionId),
    RunningSessions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerFailurePolicy {
    Retry,
    Disable {
        max_consecutive_failures: u32,
    },
    Backoff {
        initial_backoff: Duration,
        max_backoff: Duration,
        disable_after_consecutive_failures: Option<u32>,
    },
}

impl Default for WorkerSchedulerFailurePolicy {
    fn default() -> Self {
        Self::Disable {
            max_consecutive_failures: DEFAULT_DISABLE_AFTER_CONSECUTIVE_FAILURES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerAction {
    Checkpoint {
        prompt: Vec<u8>,
        suppress_when_focused: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSchedulerTask {
    pub selector: WorkerSchedulerTargetSelector,
    pub interval: Duration,
    pub action: WorkerSchedulerAction,
    pub failure_policy: WorkerSchedulerFailurePolicy,
}

impl WorkerSchedulerTask {
    pub fn checkpoint_for_session(
        session_id: WorkerSessionId,
        interval: Duration,
        prompt: Vec<u8>,
    ) -> Self {
        Self {
            selector: WorkerSchedulerTargetSelector::Session(session_id),
            interval,
            action: WorkerSchedulerAction::Checkpoint {
                prompt,
                suppress_when_focused: true,
            },
            failure_policy: WorkerSchedulerFailurePolicy::default(),
        }
    }

    pub fn with_failure_policy(mut self, failure_policy: WorkerSchedulerFailurePolicy) -> Self {
        self.failure_policy = failure_policy;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerSchedulerExecutionOutcome {
    Continue,
    CancelTask,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct WorkerSchedulerExecutionMetrics {
    checkpoint_send_attempts: u64,
    checkpoint_send_failures: u64,
    checkpoint_skips_focused: u64,
    checkpoint_skips_non_running: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkerSchedulerExecutionReport {
    outcome: WorkerSchedulerExecutionOutcome,
    metrics: WorkerSchedulerExecutionMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerDiagnosticEventKind {
    TaskFailure {
        consecutive_failures: u32,
    },
    TaskBackoffScheduled {
        consecutive_failures: u32,
        backoff: Duration,
    },
    TaskDisabled {
        consecutive_failures: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSchedulerDiagnosticEvent {
    pub task_id: WorkerSchedulerTaskId,
    pub selector: WorkerSchedulerTargetSelector,
    pub kind: WorkerSchedulerDiagnosticEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerSchedulerPerfSnapshot {
    pub task_registrations_total: u64,
    pub task_cancellations_total: u64,
    pub task_dispatches_total: u64,
    pub checkpoint_send_attempts_total: u64,
    pub checkpoint_send_failures_total: u64,
    pub checkpoint_skips_focused_total: u64,
    pub checkpoint_skips_non_running_total: u64,
    pub task_failures_total: u64,
    pub task_backoff_schedules_total: u64,
    pub task_disables_total: u64,
    pub diagnostic_events_total: u64,
    pub diagnostic_events_dropped_total: u64,
}

#[derive(Debug, Default)]
struct WorkerSchedulerPerfCounters {
    task_registrations_total: AtomicU64,
    task_cancellations_total: AtomicU64,
    task_dispatches_total: AtomicU64,
    checkpoint_send_attempts_total: AtomicU64,
    checkpoint_send_failures_total: AtomicU64,
    checkpoint_skips_focused_total: AtomicU64,
    checkpoint_skips_non_running_total: AtomicU64,
    task_failures_total: AtomicU64,
    task_backoff_schedules_total: AtomicU64,
    task_disables_total: AtomicU64,
    diagnostic_events_total: AtomicU64,
    diagnostic_events_dropped_total: AtomicU64,
}

impl WorkerSchedulerPerfCounters {
    fn snapshot(&self) -> WorkerSchedulerPerfSnapshot {
        WorkerSchedulerPerfSnapshot {
            task_registrations_total: self.task_registrations_total.load(AtomicOrdering::Relaxed),
            task_cancellations_total: self.task_cancellations_total.load(AtomicOrdering::Relaxed),
            task_dispatches_total: self.task_dispatches_total.load(AtomicOrdering::Relaxed),
            checkpoint_send_attempts_total: self
                .checkpoint_send_attempts_total
                .load(AtomicOrdering::Relaxed),
            checkpoint_send_failures_total: self
                .checkpoint_send_failures_total
                .load(AtomicOrdering::Relaxed),
            checkpoint_skips_focused_total: self
                .checkpoint_skips_focused_total
                .load(AtomicOrdering::Relaxed),
            checkpoint_skips_non_running_total: self
                .checkpoint_skips_non_running_total
                .load(AtomicOrdering::Relaxed),
            task_failures_total: self.task_failures_total.load(AtomicOrdering::Relaxed),
            task_backoff_schedules_total: self
                .task_backoff_schedules_total
                .load(AtomicOrdering::Relaxed),
            task_disables_total: self.task_disables_total.load(AtomicOrdering::Relaxed),
            diagnostic_events_total: self.diagnostic_events_total.load(AtomicOrdering::Relaxed),
            diagnostic_events_dropped_total: self
                .diagnostic_events_dropped_total
                .load(AtomicOrdering::Relaxed),
        }
    }

    fn record_execution(&self, metrics: WorkerSchedulerExecutionMetrics) {
        self.checkpoint_send_attempts_total
            .fetch_add(metrics.checkpoint_send_attempts, AtomicOrdering::Relaxed);
        self.checkpoint_send_failures_total
            .fetch_add(metrics.checkpoint_send_failures, AtomicOrdering::Relaxed);
        self.checkpoint_skips_focused_total
            .fetch_add(metrics.checkpoint_skips_focused, AtomicOrdering::Relaxed);
        self.checkpoint_skips_non_running_total.fetch_add(
            metrics.checkpoint_skips_non_running,
            AtomicOrdering::Relaxed,
        );
    }

    fn add_cancellations(&self, count: usize) {
        if count == 0 {
            return;
        }
        self.task_cancellations_total
            .fetch_add(count as u64, AtomicOrdering::Relaxed);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkerSchedulerRegisteredTask {
    task: WorkerSchedulerTask,
    version: u64,
    consecutive_failures: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkerSchedulerTimingQueueEntry {
    due_at: Instant,
    task_id: WorkerSchedulerTaskId,
    version: u64,
}

impl Ord for WorkerSchedulerTimingQueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .due_at
            .cmp(&self.due_at)
            .then_with(|| other.task_id.cmp(&self.task_id))
            .then_with(|| other.version.cmp(&self.version))
    }
}

impl PartialOrd for WorkerSchedulerTimingQueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default)]
struct WorkerSchedulerState {
    next_task_id: WorkerSchedulerTaskId,
    tasks: HashMap<WorkerSchedulerTaskId, WorkerSchedulerRegisteredTask>,
    timing_queue: BinaryHeap<WorkerSchedulerTimingQueueEntry>,
}

impl WorkerSchedulerState {
    fn next_id(&mut self) -> WorkerSchedulerTaskId {
        self.next_task_id = self
            .next_task_id
            .checked_add(1)
            .expect("worker scheduler task id space exhausted");
        self.next_task_id
    }

    fn remove_stale_heap_head(&mut self) {
        while let Some(head) = self.timing_queue.peek() {
            let keep = self
                .tasks
                .get(&head.task_id)
                .map(|task| task.version == head.version)
                .unwrap_or(false);
            if keep {
                break;
            }
            let _ = self.timing_queue.pop();
        }
    }
}

struct WorkerSchedulerInner {
    lifecycle: WorkerLifecycle,
    state: Mutex<WorkerSchedulerState>,
    wake: Notify,
    loop_handle: Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
    diagnostics_tx: broadcast::Sender<WorkerSchedulerDiagnosticEvent>,
    perf: WorkerSchedulerPerfCounters,
}

#[derive(Clone)]
pub struct WorkerScheduler {
    inner: Arc<WorkerSchedulerInner>,
}

impl WorkerScheduler {
    pub fn new(lifecycle: WorkerLifecycle) -> Self {
        let (diagnostics_tx, _diagnostics_rx) =
            broadcast::channel(WORKER_SCHEDULER_DIAGNOSTIC_BUFFER);
        Self {
            inner: Arc::new(WorkerSchedulerInner {
                lifecycle,
                state: Mutex::new(WorkerSchedulerState::default()),
                wake: Notify::new(),
                loop_handle: Mutex::new(None),
                closed: AtomicBool::new(false),
                diagnostics_tx,
                perf: WorkerSchedulerPerfCounters::default(),
            }),
        }
    }

    fn ensure_loop_started(&self) -> Result<(), WorkerRuntimeError> {
        if self.inner.closed.load(AtomicOrdering::Relaxed) {
            return Err(WorkerRuntimeError::Process(
                "worker scheduler is closed".to_owned(),
            ));
        }
        let mut guard = self
            .inner
            .loop_handle
            .lock()
            .expect("worker scheduler loop handle lock poisoned");
        if guard.is_some() {
            return Ok(());
        }
        if self.inner.closed.load(AtomicOrdering::Relaxed) {
            return Err(WorkerRuntimeError::Process(
                "worker scheduler is closed".to_owned(),
            ));
        }

        let loop_handle = tokio::runtime::Handle::try_current()
            .map_err(|_| {
                WorkerRuntimeError::Process(
                    "worker scheduler requires an active tokio runtime".to_owned(),
                )
            })?
            .spawn({
                let task_inner = Arc::clone(&self.inner);
                async move {
                    run_scheduler_loop(task_inner).await;
                }
            });
        *guard = Some(loop_handle);
        Ok(())
    }

    pub fn register(
        &self,
        task: WorkerSchedulerTask,
    ) -> Result<WorkerSchedulerTaskId, WorkerRuntimeError> {
        if task.interval.is_zero() {
            return Err(WorkerRuntimeError::Configuration(
                "worker scheduler interval must be greater than zero".to_owned(),
            ));
        }
        if let WorkerSchedulerFailurePolicy::Backoff {
            initial_backoff,
            max_backoff,
            disable_after_consecutive_failures,
        } = &task.failure_policy
        {
            if initial_backoff.is_zero() {
                return Err(WorkerRuntimeError::Configuration(
                    "worker scheduler initial backoff must be greater than zero".to_owned(),
                ));
            }
            if max_backoff < initial_backoff {
                return Err(WorkerRuntimeError::Configuration(
                    "worker scheduler max backoff must be >= initial backoff".to_owned(),
                ));
            }
            if let Some(max_failures) = disable_after_consecutive_failures {
                if *max_failures == 0 {
                    return Err(WorkerRuntimeError::Configuration(
                        "worker scheduler backoff disable threshold must be greater than zero"
                            .to_owned(),
                    ));
                }
            }
        }
        if let WorkerSchedulerFailurePolicy::Disable {
            max_consecutive_failures,
        } = &task.failure_policy
        {
            if *max_consecutive_failures == 0 {
                return Err(WorkerRuntimeError::Configuration(
                    "worker scheduler disable threshold must be greater than zero".to_owned(),
                ));
            }
        }

        self.ensure_loop_started()?;

        let mut state = self
            .inner
            .state
            .lock()
            .expect("worker scheduler state lock poisoned");
        let task_id = state.next_id();
        let due_at = Instant::now() + task.interval;
        state.tasks.insert(
            task_id,
            WorkerSchedulerRegisteredTask {
                task,
                version: 0,
                consecutive_failures: 0,
            },
        );
        state.timing_queue.push(WorkerSchedulerTimingQueueEntry {
            due_at,
            task_id,
            version: 0,
        });
        drop(state);
        self.inner
            .perf
            .task_registrations_total
            .fetch_add(1, AtomicOrdering::Relaxed);
        self.inner.wake.notify_one();
        Ok(task_id)
    }

    pub fn cancel(&self, task_id: WorkerSchedulerTaskId) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("worker scheduler state lock poisoned");
        let removed = state.tasks.remove(&task_id).is_some();
        drop(state);
        if removed {
            self.inner.perf.add_cancellations(1);
            self.inner.wake.notify_one();
        }
        removed
    }

    pub fn cancel_for_session(&self, session_id: &WorkerSessionId) -> usize {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("worker scheduler state lock poisoned");
        let mut removed = 0usize;
        state.tasks.retain(|_, registered| {
            let matches = matches!(
                &registered.task.selector,
                WorkerSchedulerTargetSelector::Session(target) if target == session_id
            );
            if matches {
                removed += 1;
            }
            !matches
        });
        drop(state);
        if removed > 0 {
            self.inner.perf.add_cancellations(removed);
            self.inner.wake.notify_one();
        }
        removed
    }

    pub fn task_count(&self) -> usize {
        let state = self
            .inner
            .state
            .lock()
            .expect("worker scheduler state lock poisoned");
        state.tasks.len()
    }

    pub fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorkerSchedulerDiagnosticEvent> {
        self.inner.diagnostics_tx.subscribe()
    }

    pub fn perf_snapshot(&self) -> WorkerSchedulerPerfSnapshot {
        self.inner.perf.snapshot()
    }

    pub fn close(&self) {
        if self.inner.closed.swap(true, AtomicOrdering::Relaxed) {
            return;
        }
        let mut guard = self
            .inner
            .loop_handle
            .lock()
            .expect("worker scheduler loop handle lock poisoned");
        if let Some(handle) = guard.take() {
            handle.abort();
        }
        drop(guard);
        let mut state = self
            .inner
            .state
            .lock()
            .expect("worker scheduler state lock poisoned");
        state.tasks.clear();
        state.timing_queue.clear();
        drop(state);
        self.inner.wake.notify_waiters();
    }
}

impl Drop for WorkerScheduler {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) != 1 {
            return;
        }
        self.close();
    }
}

async fn run_scheduler_loop(inner: Arc<WorkerSchedulerInner>) {
    loop {
        if inner.closed.load(AtomicOrdering::Relaxed) {
            break;
        }

        let due = {
            let mut state = inner
                .state
                .lock()
                .expect("worker scheduler state lock poisoned");
            state.remove_stale_heap_head();
            match state.timing_queue.peek() {
                Some(entry) => {
                    let next_task_id = entry.task_id;
                    let next_version = entry.version;
                    let next_due_at = entry.due_at;
                    let now = Instant::now();
                    if next_due_at <= now {
                        Some((
                            state
                                .timing_queue
                                .pop()
                                .expect("checked queue head should exist")
                                .task_id,
                            next_version,
                            Duration::ZERO,
                        ))
                    } else {
                        Some((next_task_id, next_version, next_due_at - now))
                    }
                }
                None => None,
            }
        };

        match due {
            Some((task_id, task_version, Duration::ZERO)) => {
                dispatch_ready_task(&inner, task_id, task_version).await;
            }
            Some((_task_id, _task_version, wait_for)) => {
                tokio::select! {
                    _ = tokio::time::sleep(wait_for) => {}
                    _ = inner.wake.notified() => {}
                }
            }
            None => {
                inner.wake.notified().await;
            }
        }
    }
}

async fn dispatch_ready_task(
    inner: &Arc<WorkerSchedulerInner>,
    task_id: WorkerSchedulerTaskId,
    task_version: u64,
) {
    let task = {
        let state = inner
            .state
            .lock()
            .expect("worker scheduler state lock poisoned");
        match state.tasks.get(&task_id) {
            Some(task) if task.version == task_version => task.task.clone(),
            _ => return,
        }
    };

    inner
        .perf
        .task_dispatches_total
        .fetch_add(1, AtomicOrdering::Relaxed);
    let report = execute_task(&inner.lifecycle, &task).await;
    inner.perf.record_execution(report.metrics);

    let mut state = inner
        .state
        .lock()
        .expect("worker scheduler state lock poisoned");
    let Some(registered) = state.tasks.get_mut(&task_id) else {
        return;
    };
    if registered.version != task_version {
        return;
    }

    let mut diagnostics = Vec::new();
    let mut queue_next_due_at: Option<Instant> = None;
    let mut queue_next_version: Option<u64> = None;
    let mut remove_task = false;
    let mut increment_disable_count = false;
    let mut increment_backoff_count = false;

    match report.outcome {
        WorkerSchedulerExecutionOutcome::CancelTask => {
            remove_task = true;
        }
        WorkerSchedulerExecutionOutcome::Continue => {
            registered.consecutive_failures = 0;
            registered.version = registered.version.saturating_add(1);
            queue_next_due_at = Some(Instant::now() + registered.task.interval);
            queue_next_version = Some(registered.version);
        }
        WorkerSchedulerExecutionOutcome::Failed => {
            inner
                .perf
                .task_failures_total
                .fetch_add(1, AtomicOrdering::Relaxed);
            registered.consecutive_failures = registered.consecutive_failures.saturating_add(1);
            diagnostics.push(WorkerSchedulerDiagnosticEvent {
                task_id,
                selector: registered.task.selector.clone(),
                kind: WorkerSchedulerDiagnosticEventKind::TaskFailure {
                    consecutive_failures: registered.consecutive_failures,
                },
            });

            match &registered.task.failure_policy {
                WorkerSchedulerFailurePolicy::Retry => {
                    registered.version = registered.version.saturating_add(1);
                    queue_next_due_at = Some(Instant::now() + registered.task.interval);
                    queue_next_version = Some(registered.version);
                }
                WorkerSchedulerFailurePolicy::Disable {
                    max_consecutive_failures,
                } => {
                    if registered.consecutive_failures >= *max_consecutive_failures {
                        diagnostics.push(WorkerSchedulerDiagnosticEvent {
                            task_id,
                            selector: registered.task.selector.clone(),
                            kind: WorkerSchedulerDiagnosticEventKind::TaskDisabled {
                                consecutive_failures: registered.consecutive_failures,
                            },
                        });
                        remove_task = true;
                        increment_disable_count = true;
                    } else {
                        registered.version = registered.version.saturating_add(1);
                        queue_next_due_at = Some(Instant::now() + registered.task.interval);
                        queue_next_version = Some(registered.version);
                    }
                }
                WorkerSchedulerFailurePolicy::Backoff {
                    initial_backoff,
                    max_backoff,
                    disable_after_consecutive_failures,
                } => {
                    if disable_after_consecutive_failures
                        .is_some_and(|limit| registered.consecutive_failures >= limit)
                    {
                        diagnostics.push(WorkerSchedulerDiagnosticEvent {
                            task_id,
                            selector: registered.task.selector.clone(),
                            kind: WorkerSchedulerDiagnosticEventKind::TaskDisabled {
                                consecutive_failures: registered.consecutive_failures,
                            },
                        });
                        remove_task = true;
                        increment_disable_count = true;
                    } else {
                        registered.version = registered.version.saturating_add(1);
                        let backoff = next_backoff_duration(
                            registered.consecutive_failures,
                            *initial_backoff,
                            *max_backoff,
                        );
                        queue_next_due_at = Some(Instant::now() + backoff);
                        queue_next_version = Some(registered.version);
                        increment_backoff_count = true;
                        diagnostics.push(WorkerSchedulerDiagnosticEvent {
                            task_id,
                            selector: registered.task.selector.clone(),
                            kind: WorkerSchedulerDiagnosticEventKind::TaskBackoffScheduled {
                                consecutive_failures: registered.consecutive_failures,
                                backoff,
                            },
                        });
                    }
                }
            }
        }
    }

    if remove_task {
        state.tasks.remove(&task_id);
    }
    if let (Some(due_at), Some(version)) = (queue_next_due_at, queue_next_version) {
        state.timing_queue.push(WorkerSchedulerTimingQueueEntry {
            due_at,
            task_id,
            version,
        });
    }
    drop(state);

    if increment_disable_count {
        inner
            .perf
            .task_disables_total
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
    if increment_backoff_count {
        inner
            .perf
            .task_backoff_schedules_total
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
    for event in diagnostics {
        emit_diagnostic_event(inner, event);
    }
}

fn emit_diagnostic_event(inner: &WorkerSchedulerInner, event: WorkerSchedulerDiagnosticEvent) {
    inner
        .perf
        .diagnostic_events_total
        .fetch_add(1, AtomicOrdering::Relaxed);
    if inner.diagnostics_tx.send(event).is_err() {
        inner
            .perf
            .diagnostic_events_dropped_total
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
}

fn next_backoff_duration(
    consecutive_failures: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> Duration {
    if consecutive_failures <= 1 {
        return initial_backoff;
    }

    let mut current = initial_backoff;
    for _ in 1..consecutive_failures {
        current = current.saturating_mul(2);
        if current >= max_backoff {
            return max_backoff;
        }
    }
    current.min(max_backoff)
}

fn is_non_running_lifecycle_send_error(error: &WorkerRuntimeError) -> bool {
    match error {
        WorkerRuntimeError::SessionNotFound(_) => true,
        WorkerRuntimeError::Process(message) => {
            message.contains("still starting") || message.contains("not running")
        }
        _ => false,
    }
}

async fn execute_task(
    lifecycle: &WorkerLifecycle,
    task: &WorkerSchedulerTask,
) -> WorkerSchedulerExecutionReport {
    match &task.action {
        WorkerSchedulerAction::Checkpoint {
            prompt,
            suppress_when_focused,
        } => {
            execute_checkpoint_task(lifecycle, &task.selector, prompt, *suppress_when_focused).await
        }
    }
}

async fn execute_checkpoint_task(
    lifecycle: &WorkerLifecycle,
    selector: &WorkerSchedulerTargetSelector,
    prompt: &[u8],
    suppress_when_focused: bool,
) -> WorkerSchedulerExecutionReport {
    match selector {
        WorkerSchedulerTargetSelector::Session(session_id) => {
            let snapshot = lifecycle.snapshot(session_id).await;
            let Some(snapshot) = snapshot else {
                return WorkerSchedulerExecutionReport {
                    outcome: WorkerSchedulerExecutionOutcome::CancelTask,
                    metrics: WorkerSchedulerExecutionMetrics {
                        checkpoint_skips_non_running: 1,
                        ..WorkerSchedulerExecutionMetrics::default()
                    },
                };
            };
            if snapshot.state != WorkerSessionState::Running {
                return WorkerSchedulerExecutionReport {
                    outcome: WorkerSchedulerExecutionOutcome::CancelTask,
                    metrics: WorkerSchedulerExecutionMetrics {
                        checkpoint_skips_non_running: 1,
                        ..WorkerSchedulerExecutionMetrics::default()
                    },
                };
            }
            if suppress_when_focused && snapshot.visibility == WorkerSessionVisibility::Focused {
                return WorkerSchedulerExecutionReport {
                    outcome: WorkerSchedulerExecutionOutcome::Continue,
                    metrics: WorkerSchedulerExecutionMetrics {
                        checkpoint_skips_focused: 1,
                        ..WorkerSchedulerExecutionMetrics::default()
                    },
                };
            }
            match lifecycle.send_input(session_id, prompt).await {
                Ok(()) => WorkerSchedulerExecutionReport {
                    outcome: WorkerSchedulerExecutionOutcome::Continue,
                    metrics: WorkerSchedulerExecutionMetrics {
                        checkpoint_send_attempts: 1,
                        ..WorkerSchedulerExecutionMetrics::default()
                    },
                },
                Err(error) => {
                    if is_non_running_lifecycle_send_error(&error) {
                        WorkerSchedulerExecutionReport {
                            outcome: WorkerSchedulerExecutionOutcome::CancelTask,
                            metrics: WorkerSchedulerExecutionMetrics {
                                checkpoint_send_attempts: 1,
                                checkpoint_skips_non_running: 1,
                                ..WorkerSchedulerExecutionMetrics::default()
                            },
                        }
                    } else {
                        WorkerSchedulerExecutionReport {
                            outcome: WorkerSchedulerExecutionOutcome::Failed,
                            metrics: WorkerSchedulerExecutionMetrics {
                                checkpoint_send_attempts: 1,
                                checkpoint_send_failures: 1,
                                ..WorkerSchedulerExecutionMetrics::default()
                            },
                        }
                    }
                }
            }
        }
        WorkerSchedulerTargetSelector::RunningSessions => {
            let sessions = lifecycle.list_sessions().await;
            let mut metrics = WorkerSchedulerExecutionMetrics::default();
            let mut has_failures = false;

            for session in sessions {
                if session.state != WorkerSessionState::Running {
                    metrics.checkpoint_skips_non_running =
                        metrics.checkpoint_skips_non_running.saturating_add(1);
                    continue;
                }
                if suppress_when_focused && session.visibility == WorkerSessionVisibility::Focused {
                    metrics.checkpoint_skips_focused =
                        metrics.checkpoint_skips_focused.saturating_add(1);
                    continue;
                }

                metrics.checkpoint_send_attempts =
                    metrics.checkpoint_send_attempts.saturating_add(1);
                match lifecycle.send_input(&session.session_id, prompt).await {
                    Ok(()) => {}
                    Err(error) if is_non_running_lifecycle_send_error(&error) => {
                        metrics.checkpoint_skips_non_running =
                            metrics.checkpoint_skips_non_running.saturating_add(1);
                    }
                    Err(_) => {
                        metrics.checkpoint_send_failures =
                            metrics.checkpoint_send_failures.saturating_add(1);
                        has_failures = true;
                    }
                }
            }

            if has_failures {
                WorkerSchedulerExecutionReport {
                    outcome: WorkerSchedulerExecutionOutcome::Failed,
                    metrics,
                }
            } else {
                WorkerSchedulerExecutionReport {
                    outcome: WorkerSchedulerExecutionOutcome::Continue,
                    metrics,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use orchestrator_worker_eventbus::WorkerEventBus;
    use orchestrator_worker_lifecycle::{WorkerLifecycle, WorkerSessionState};
    use orchestrator_worker_protocol::backend::{
        WorkerBackendCapabilities, WorkerBackendInfo, WorkerBackendKind, WorkerEventStream,
        WorkerEventSubscription, WorkerSessionControl, WorkerSessionStreamSource,
    };
    use orchestrator_worker_protocol::error::{WorkerRuntimeError, WorkerRuntimeResult};
    use orchestrator_worker_protocol::event::{
        WorkerDoneEvent, WorkerEvent, WorkerNeedsInputAnswer,
    };
    use orchestrator_worker_protocol::ids::WorkerSessionId;
    use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout};

    use super::{
        WorkerScheduler, WorkerSchedulerDiagnosticEventKind, WorkerSchedulerFailurePolicy,
        WorkerSchedulerState, WorkerSchedulerTargetSelector, WorkerSchedulerTask,
    };

    type StreamMessage = WorkerRuntimeResult<Option<WorkerEvent>>;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Default)]
    struct MockBackend {
        state: Mutex<MockBackendState>,
    }

    #[derive(Default)]
    struct MockBackendState {
        sessions: HashMap<WorkerSessionId, MockSession>,
    }

    struct MockSession {
        event_tx: Option<mpsc::UnboundedSender<StreamMessage>>,
        event_rx: Option<mpsc::UnboundedReceiver<StreamMessage>>,
        sent_input: Vec<Vec<u8>>,
        send_input_failure: Option<MockSendInputFailure>,
        send_input_attempts: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MockSendInputFailure {
        remaining: usize,
        error: WorkerRuntimeError,
    }

    struct MockEventStream {
        receiver: mpsc::UnboundedReceiver<StreamMessage>,
    }

    impl MockBackend {
        fn sent_input(&self, session_id: &WorkerSessionId) -> Vec<Vec<u8>> {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .sent_input
                .clone()
        }

        fn send_input_attempts(&self, session_id: &WorkerSessionId) -> usize {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .send_input_attempts
        }

        fn set_send_input_failures(&self, session_id: &WorkerSessionId, failures_remaining: usize) {
            self.set_send_input_failure_error(
                session_id,
                failures_remaining,
                WorkerRuntimeError::Process("simulated scheduled send_input failure".to_owned()),
            );
        }

        fn set_send_input_failure_error(
            &self,
            session_id: &WorkerSessionId,
            failures_remaining: usize,
            error: WorkerRuntimeError,
        ) {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(session_id).expect("session exists");
            session.send_input_failure = (failures_remaining > 0).then_some(MockSendInputFailure {
                remaining: failures_remaining,
                error,
            });
        }

        fn emit_event(&self, session_id: &WorkerSessionId, event: WorkerEvent) {
            let sender = {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .event_tx
                    .as_ref()
                    .expect("session stream sender")
                    .clone()
            };
            sender.send(Ok(Some(event))).expect("send mock event");
        }
    }

    #[async_trait]
    impl WorkerSessionControl for MockBackend {
        async fn spawn(
            &self,
            spec: WorkerSpawnRequest,
        ) -> WorkerRuntimeResult<WorkerSessionHandle> {
            let mut state = self.state.lock().expect("lock backend state");
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            state.sessions.insert(
                spec.session_id.clone(),
                MockSession {
                    event_tx: Some(event_tx),
                    event_rx: Some(event_rx),
                    sent_input: Vec::new(),
                    send_input_failure: None,
                    send_input_attempts: 0,
                },
            );
            Ok(WorkerSessionHandle {
                session_id: spec.session_id,
                backend: WorkerBackendKind::OpenCode,
            })
        }

        async fn kill(&self, _session: &WorkerSessionHandle) -> WorkerRuntimeResult<()> {
            Ok(())
        }

        async fn send_input(
            &self,
            session: &WorkerSessionHandle,
            input: &[u8],
        ) -> WorkerRuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                WorkerRuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.send_input_attempts = session.send_input_attempts.saturating_add(1);
            if let Some(failure) = session.send_input_failure.as_mut() {
                if failure.remaining > 0 {
                    failure.remaining -= 1;
                    return Err(failure.error.clone());
                }
            }
            session.sent_input.push(input.to_vec());
            Ok(())
        }

        async fn respond_to_needs_input(
            &self,
            _session: &WorkerSessionHandle,
            _prompt_id: &str,
            _answers: &[WorkerNeedsInputAnswer],
        ) -> WorkerRuntimeResult<()> {
            Ok(())
        }

        async fn resize(
            &self,
            _session: &WorkerSessionHandle,
            _cols: u16,
            _rows: u16,
        ) -> WorkerRuntimeResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl WorkerSessionStreamSource for MockBackend {
        async fn subscribe(
            &self,
            session: &WorkerSessionHandle,
        ) -> WorkerRuntimeResult<WorkerEventStream> {
            let mut state = self.state.lock().expect("lock backend state");
            let receiver = state
                .sessions
                .get_mut(&session.session_id)
                .expect("session exists")
                .event_rx
                .take()
                .expect("single stream subscriber");
            Ok(Box::new(MockEventStream { receiver }))
        }
    }

    #[async_trait]
    impl WorkerBackendInfo for MockBackend {
        fn kind(&self) -> WorkerBackendKind {
            WorkerBackendKind::OpenCode
        }

        fn capabilities(&self) -> WorkerBackendCapabilities {
            WorkerBackendCapabilities {
                structured_events: true,
                supports_background: true,
                ..WorkerBackendCapabilities::default()
            }
        }

        async fn health_check(&self) -> WorkerRuntimeResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl WorkerEventSubscription for MockEventStream {
        async fn next_event(&mut self) -> WorkerRuntimeResult<Option<WorkerEvent>> {
            match self.receiver.recv().await {
                Some(event) => event,
                None => Ok(None),
            }
        }
    }

    fn spawn_request(session_id: &str) -> WorkerSpawnRequest {
        WorkerSpawnRequest {
            session_id: WorkerSessionId::new(session_id),
            workdir: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(session_id),
            model: None,
            instruction_prelude: None,
            environment: Vec::new(),
        }
    }

    async fn wait_for_sent_input_count(
        backend: &MockBackend,
        session_id: &WorkerSessionId,
        expected: usize,
    ) {
        timeout(TEST_TIMEOUT, async {
            loop {
                if backend.sent_input(session_id).len() >= expected {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for scheduled input");
    }

    async fn wait_for_send_input_attempt_count(
        backend: &MockBackend,
        session_id: &WorkerSessionId,
        expected: usize,
    ) {
        timeout(TEST_TIMEOUT, async {
            loop {
                if backend.send_input_attempts(session_id) >= expected {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for scheduled input attempts");
    }

    async fn assert_sent_input_count_stable(
        backend: &MockBackend,
        session_id: &WorkerSessionId,
        expected: usize,
        window: Duration,
    ) {
        let start = Instant::now();
        while start.elapsed() < window {
            assert_eq!(
                backend.sent_input(session_id).len(),
                expected,
                "scheduled input changed during stability window",
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn assert_send_input_attempt_count_stable(
        backend: &MockBackend,
        session_id: &WorkerSessionId,
        expected: usize,
        window: Duration,
    ) {
        let start = Instant::now();
        while start.elapsed() < window {
            assert_eq!(
                backend.send_input_attempts(session_id),
                expected,
                "scheduled input attempts changed during stability window",
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_status(
        lifecycle: &WorkerLifecycle,
        session_id: &WorkerSessionId,
        expected: WorkerSessionState,
    ) {
        timeout(TEST_TIMEOUT, async {
            loop {
                if lifecycle.status(session_id).await.expect("status") == expected {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for status transition");
    }

    async fn wait_for_task_count(scheduler: &WorkerScheduler, expected: usize) {
        timeout(TEST_TIMEOUT, async {
            loop {
                if scheduler.task_count() == expected {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for scheduler task count");
    }

    #[test]
    #[should_panic(expected = "worker scheduler task id space exhausted")]
    fn register_panics_when_task_id_space_is_exhausted() {
        let mut state = WorkerSchedulerState::default();
        state.next_task_id = u64::MAX;
        let _ = state.next_id();
    }

    #[test]
    fn register_rejects_when_no_tokio_runtime_is_active() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend, Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle);

        let error = scheduler
            .register(WorkerSchedulerTask::checkpoint_for_session(
                WorkerSessionId::new("sess-no-runtime"),
                Duration::from_millis(30),
                b"Emit a checkpoint now.\n".to_vec(),
            ))
            .expect_err("register should fail without active tokio runtime");
        assert!(matches!(
            error,
            WorkerRuntimeError::Process(message)
                if message.contains("active tokio runtime")
        ));
    }

    #[tokio::test]
    async fn register_and_cancel_task_round_trips() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend, Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle);
        let task = WorkerSchedulerTask::checkpoint_for_session(
            WorkerSessionId::new("sess-1"),
            Duration::from_millis(50),
            b"Emit a checkpoint now.\n".to_vec(),
        );

        let task_id = scheduler.register(task).expect("register task");
        assert_eq!(scheduler.task_count(), 1);
        assert!(scheduler.cancel(task_id));
        assert!(!scheduler.cancel(task_id));
        assert_eq!(scheduler.task_count(), 0);
        scheduler.close();
    }

    #[tokio::test]
    async fn register_rejects_when_scheduler_is_closed() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend, Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle);
        scheduler.close();

        let error = scheduler
            .register(WorkerSchedulerTask::checkpoint_for_session(
                WorkerSessionId::new("sess-closed"),
                Duration::from_millis(30),
                b"Emit a checkpoint now.\n".to_vec(),
            ))
            .expect_err("register should fail when scheduler is closed");
        assert!(matches!(
            error,
            WorkerRuntimeError::Process(message) if message.contains("closed")
        ));
        assert_eq!(scheduler.task_count(), 0);
    }

    #[test]
    fn register_rejects_zero_disable_threshold() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend, Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle);

        let error = scheduler
            .register(
                WorkerSchedulerTask::checkpoint_for_session(
                    WorkerSessionId::new("sess-invalid-disable-threshold"),
                    Duration::from_millis(30),
                    b"Emit a checkpoint now.\n".to_vec(),
                )
                .with_failure_policy(WorkerSchedulerFailurePolicy::Disable {
                    max_consecutive_failures: 0,
                }),
            )
            .expect_err("register should fail for zero disable threshold");
        assert!(matches!(
            error,
            WorkerRuntimeError::Configuration(message) if message.contains("disable threshold")
        ));
    }

    #[test]
    fn register_rejects_zero_backoff_disable_threshold() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend, Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle);

        let error = scheduler
            .register(
                WorkerSchedulerTask::checkpoint_for_session(
                    WorkerSessionId::new("sess-invalid-backoff-disable-threshold"),
                    Duration::from_millis(30),
                    b"Emit a checkpoint now.\n".to_vec(),
                )
                .with_failure_policy(WorkerSchedulerFailurePolicy::Backoff {
                    initial_backoff: Duration::from_millis(20),
                    max_backoff: Duration::from_millis(40),
                    disable_after_consecutive_failures: Some(0),
                }),
            )
            .expect_err("register should fail for zero backoff disable threshold");
        assert!(matches!(
            error,
            WorkerRuntimeError::Configuration(message)
                if message.contains("backoff disable threshold")
        ));
    }

    #[tokio::test]
    async fn periodic_checkpoint_dispatches_for_running_session() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-periodic-running"))
            .await
            .expect("spawn running session");

        scheduler
            .register(WorkerSchedulerTask::checkpoint_for_session(
                handle.session_id.clone(),
                Duration::from_millis(40),
                b"Emit a checkpoint now.\n".to_vec(),
            ))
            .expect("register checkpoint task");

        wait_for_sent_input_count(&backend, &handle.session_id, 2).await;
        let sent = backend.sent_input(&handle.session_id);
        assert!(sent
            .iter()
            .all(|entry| entry == b"Emit a checkpoint now.\n"));
        scheduler.close();
    }

    #[tokio::test]
    async fn periodic_checkpoint_skips_while_focused_and_resumes_when_unfocused() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-focused"))
            .await
            .expect("spawn focused session");

        lifecycle
            .focus_session(Some(&handle.session_id))
            .await
            .expect("focus session");
        scheduler
            .register(WorkerSchedulerTask::checkpoint_for_session(
                handle.session_id.clone(),
                Duration::from_millis(35),
                b"Emit a checkpoint now.\n".to_vec(),
            ))
            .expect("register checkpoint task");

        assert_sent_input_count_stable(&backend, &handle.session_id, 0, Duration::from_millis(140))
            .await;

        lifecycle.focus_session(None).await.expect("clear focus");
        wait_for_sent_input_count(&backend, &handle.session_id, 1).await;
        let perf = scheduler.perf_snapshot();
        assert!(perf.checkpoint_skips_focused_total >= 1);
        scheduler.close();
    }

    #[tokio::test]
    async fn running_sessions_selector_counts_focused_and_non_running_skips() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());

        let running = lifecycle
            .spawn(spawn_request("scheduler-running-focused"))
            .await
            .expect("spawn running session");
        let done = lifecycle
            .spawn(spawn_request("scheduler-running-done"))
            .await
            .expect("spawn done session");

        lifecycle
            .focus_session(Some(&running.session_id))
            .await
            .expect("focus running session");
        backend.emit_event(
            &done.session_id,
            WorkerEvent::Done(WorkerDoneEvent {
                summary: Some("finished".to_owned()),
            }),
        );
        wait_for_status(&lifecycle, &done.session_id, WorkerSessionState::Done).await;

        scheduler
            .register(WorkerSchedulerTask {
                selector: WorkerSchedulerTargetSelector::RunningSessions,
                interval: Duration::from_millis(25),
                action: super::WorkerSchedulerAction::Checkpoint {
                    prompt: b"Emit a checkpoint now.\n".to_vec(),
                    suppress_when_focused: true,
                },
                failure_policy: WorkerSchedulerFailurePolicy::Retry,
            })
            .expect("register running selector task");

        timeout(TEST_TIMEOUT, async {
            loop {
                let perf = scheduler.perf_snapshot();
                if perf.checkpoint_skips_focused_total >= 1
                    && perf.checkpoint_skips_non_running_total >= 1
                {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for focused/non-running skip metrics");

        assert_eq!(backend.sent_input(&running.session_id).len(), 0);
        assert_eq!(backend.sent_input(&done.session_id).len(), 0);

        lifecycle.focus_session(None).await.expect("clear focus");
        wait_for_sent_input_count(&backend, &running.session_id, 1).await;
        scheduler.close();
    }

    #[tokio::test]
    async fn terminal_transition_cancels_session_targeted_task() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-terminal-cancel"))
            .await
            .expect("spawn terminal session");

        scheduler
            .register(
                WorkerSchedulerTask::checkpoint_for_session(
                    handle.session_id.clone(),
                    Duration::from_millis(30),
                    b"Emit a checkpoint now.\n".to_vec(),
                )
                .with_failure_policy(WorkerSchedulerFailurePolicy::Retry),
            )
            .expect("register checkpoint task");
        wait_for_sent_input_count(&backend, &handle.session_id, 1).await;

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent {
                summary: Some("finished".to_owned()),
            }),
        );
        wait_for_status(&lifecycle, &handle.session_id, WorkerSessionState::Done).await;

        timeout(TEST_TIMEOUT, async {
            loop {
                if scheduler.task_count() == 0 {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for scheduler cancellation");

        let sent_before = backend.sent_input(&handle.session_id).len();
        assert_sent_input_count_stable(
            &backend,
            &handle.session_id,
            sent_before,
            Duration::from_millis(120),
        )
        .await;
        scheduler.close();
    }

    #[tokio::test]
    async fn running_sessions_selector_dispatches_to_all_background_sessions() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());

        let session_a = lifecycle
            .spawn(spawn_request("scheduler-all-running-a"))
            .await
            .expect("spawn session a");
        let session_b = lifecycle
            .spawn(spawn_request("scheduler-all-running-b"))
            .await
            .expect("spawn session b");

        scheduler
            .register(WorkerSchedulerTask {
                selector: WorkerSchedulerTargetSelector::RunningSessions,
                interval: Duration::from_millis(35),
                action: super::WorkerSchedulerAction::Checkpoint {
                    prompt: b"Emit a checkpoint now.\n".to_vec(),
                    suppress_when_focused: true,
                },
                failure_policy: WorkerSchedulerFailurePolicy::Retry,
            })
            .expect("register running selector task");

        wait_for_sent_input_count(&backend, &session_a.session_id, 1).await;
        wait_for_sent_input_count(&backend, &session_b.session_id, 1).await;
        scheduler.close();
    }

    #[tokio::test]
    async fn session_selector_non_running_send_error_cancels_without_failure() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-non-running-send-error"))
            .await
            .expect("spawn session");
        backend.set_send_input_failure_error(
            &handle.session_id,
            1,
            WorkerRuntimeError::Process("worker session is not running: simulated".to_owned()),
        );

        scheduler
            .register(WorkerSchedulerTask::checkpoint_for_session(
                handle.session_id.clone(),
                Duration::from_millis(20),
                b"Emit a checkpoint now.\n".to_vec(),
            ))
            .expect("register checkpoint task");

        wait_for_send_input_attempt_count(&backend, &handle.session_id, 1).await;
        wait_for_task_count(&scheduler, 0).await;
        assert_send_input_attempt_count_stable(
            &backend,
            &handle.session_id,
            1,
            Duration::from_millis(120),
        )
        .await;

        let perf = scheduler.perf_snapshot();
        assert_eq!(perf.task_failures_total, 0);
        assert_eq!(perf.task_disables_total, 0);
        assert_eq!(perf.checkpoint_send_attempts_total, 1);
        assert_eq!(perf.checkpoint_send_failures_total, 0);
        assert_eq!(perf.checkpoint_skips_non_running_total, 1);
        assert_eq!(perf.diagnostic_events_total, 0);
        scheduler.close();
    }

    #[tokio::test]
    async fn running_sessions_non_running_send_error_does_not_trigger_failure_policy() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-running-race"))
            .await
            .expect("spawn running session");
        backend.set_send_input_failure_error(
            &handle.session_id,
            1,
            WorkerRuntimeError::SessionNotFound(handle.session_id.as_str().to_owned()),
        );

        scheduler
            .register(WorkerSchedulerTask {
                selector: WorkerSchedulerTargetSelector::RunningSessions,
                interval: Duration::from_millis(20),
                action: super::WorkerSchedulerAction::Checkpoint {
                    prompt: b"Emit a checkpoint now.\n".to_vec(),
                    suppress_when_focused: false,
                },
                failure_policy: WorkerSchedulerFailurePolicy::Disable {
                    max_consecutive_failures: 1,
                },
            })
            .expect("register running selector task");

        wait_for_send_input_attempt_count(&backend, &handle.session_id, 2).await;
        wait_for_sent_input_count(&backend, &handle.session_id, 1).await;
        assert_eq!(scheduler.task_count(), 1);

        let perf = scheduler.perf_snapshot();
        assert_eq!(perf.task_failures_total, 0);
        assert_eq!(perf.task_disables_total, 0);
        assert_eq!(perf.checkpoint_send_failures_total, 0);
        assert!(perf.checkpoint_skips_non_running_total >= 1);
        assert_eq!(perf.diagnostic_events_total, 0);
        scheduler.close();
    }

    #[tokio::test]
    async fn repeated_checkpoint_failures_disable_task_after_threshold_and_emit_diagnostics() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-disable-repeated-failures"))
            .await
            .expect("spawn failure session");
        backend.set_send_input_failures(&handle.session_id, 10);
        let mut diagnostics = scheduler.subscribe_diagnostics();

        scheduler
            .register(WorkerSchedulerTask::checkpoint_for_session(
                handle.session_id.clone(),
                Duration::from_millis(20),
                b"Emit a checkpoint now.\n".to_vec(),
            ))
            .expect("register checkpoint task");

        wait_for_send_input_attempt_count(&backend, &handle.session_id, 3).await;
        wait_for_task_count(&scheduler, 0).await;
        assert_send_input_attempt_count_stable(
            &backend,
            &handle.session_id,
            3,
            Duration::from_millis(120),
        )
        .await;
        assert_eq!(backend.sent_input(&handle.session_id).len(), 0);

        let mut saw_failure = false;
        let mut saw_disabled = false;
        timeout(TEST_TIMEOUT, async {
            loop {
                let event = diagnostics.recv().await.expect("diagnostic event");
                match event.kind {
                    WorkerSchedulerDiagnosticEventKind::TaskFailure { .. } => {
                        saw_failure = true;
                    }
                    WorkerSchedulerDiagnosticEventKind::TaskDisabled { .. } => {
                        saw_disabled = true;
                    }
                    WorkerSchedulerDiagnosticEventKind::TaskBackoffScheduled { .. } => {}
                }
                if saw_failure && saw_disabled {
                    return;
                }
            }
        })
        .await
        .expect("timed out waiting for failure/disable diagnostic events");

        let perf = scheduler.perf_snapshot();
        assert_eq!(perf.task_failures_total, 3);
        assert_eq!(perf.task_disables_total, 1);
        assert_eq!(perf.checkpoint_send_attempts_total, 3);
        assert_eq!(perf.checkpoint_send_failures_total, 3);
        scheduler.close();
    }

    #[tokio::test]
    async fn backoff_failure_policy_schedules_backoff_and_then_disables() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let handle = lifecycle
            .spawn(spawn_request("scheduler-backoff-failures"))
            .await
            .expect("spawn backoff session");
        backend.set_send_input_failures(&handle.session_id, 10);
        let mut diagnostics = scheduler.subscribe_diagnostics();

        scheduler
            .register(
                WorkerSchedulerTask::checkpoint_for_session(
                    handle.session_id.clone(),
                    Duration::from_millis(15),
                    b"Emit a checkpoint now.\n".to_vec(),
                )
                .with_failure_policy(WorkerSchedulerFailurePolicy::Backoff {
                    initial_backoff: Duration::from_millis(40),
                    max_backoff: Duration::from_millis(80),
                    disable_after_consecutive_failures: Some(3),
                }),
            )
            .expect("register backoff task");

        wait_for_send_input_attempt_count(&backend, &handle.session_id, 3).await;
        wait_for_task_count(&scheduler, 0).await;
        assert_eq!(backend.sent_input(&handle.session_id).len(), 0);

        let mut saw_backoff = false;
        let mut saw_disabled = false;
        timeout(TEST_TIMEOUT, async {
            loop {
                let event = diagnostics.recv().await.expect("diagnostic event");
                match event.kind {
                    WorkerSchedulerDiagnosticEventKind::TaskBackoffScheduled { .. } => {
                        saw_backoff = true;
                    }
                    WorkerSchedulerDiagnosticEventKind::TaskDisabled { .. } => {
                        saw_disabled = true;
                    }
                    WorkerSchedulerDiagnosticEventKind::TaskFailure { .. } => {}
                }
                if saw_backoff && saw_disabled {
                    return;
                }
            }
        })
        .await
        .expect("timed out waiting for backoff/disable diagnostic events");

        let perf = scheduler.perf_snapshot();
        assert_eq!(perf.task_failures_total, 3);
        assert_eq!(perf.task_backoff_schedules_total, 2);
        assert_eq!(perf.task_disables_total, 1);
        assert_eq!(perf.checkpoint_send_attempts_total, 3);
        assert_eq!(perf.checkpoint_send_failures_total, 3);
        scheduler.close();
    }
}
