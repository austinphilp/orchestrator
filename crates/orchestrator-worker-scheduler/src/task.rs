use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestrator_worker_lifecycle::{WorkerLifecycle, WorkerSessionState, WorkerSessionVisibility};
use orchestrator_worker_protocol::error::WorkerRuntimeError;
use orchestrator_worker_protocol::ids::WorkerSessionId;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

pub type WorkerSchedulerTaskId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerTargetSelector {
    Session(WorkerSessionId),
    RunningSessions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerFailurePolicy {
    Retry,
    Disable,
    Backoff {
        initial_backoff: Duration,
        max_backoff: Duration,
    },
}

impl Default for WorkerSchedulerFailurePolicy {
    fn default() -> Self {
        Self::Disable
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
            failure_policy: WorkerSchedulerFailurePolicy::Disable,
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
}

#[derive(Clone)]
pub struct WorkerScheduler {
    inner: Arc<WorkerSchedulerInner>,
}

impl WorkerScheduler {
    pub fn new(lifecycle: WorkerLifecycle) -> Self {
        Self {
            inner: Arc::new(WorkerSchedulerInner {
                lifecycle,
                state: Mutex::new(WorkerSchedulerState::default()),
                wake: Notify::new(),
                loop_handle: Mutex::new(None),
                closed: AtomicBool::new(false),
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
        } = task.failure_policy
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

    let outcome = execute_task(&inner.lifecycle, &task).await;

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

    match outcome {
        WorkerSchedulerExecutionOutcome::CancelTask => {
            state.tasks.remove(&task_id);
        }
        WorkerSchedulerExecutionOutcome::Continue => {
            registered.consecutive_failures = 0;
            registered.version = registered.version.saturating_add(1);
            let next_version = registered.version;
            let next_due_at = Instant::now() + registered.task.interval;
            state.timing_queue.push(WorkerSchedulerTimingQueueEntry {
                due_at: next_due_at,
                task_id,
                version: next_version,
            });
        }
        WorkerSchedulerExecutionOutcome::Failed => match registered.task.failure_policy {
            WorkerSchedulerFailurePolicy::Retry => {
                registered.version = registered.version.saturating_add(1);
                let next_version = registered.version;
                let next_due_at = Instant::now() + registered.task.interval;
                state.timing_queue.push(WorkerSchedulerTimingQueueEntry {
                    due_at: next_due_at,
                    task_id,
                    version: next_version,
                });
            }
            WorkerSchedulerFailurePolicy::Disable => {
                state.tasks.remove(&task_id);
            }
            WorkerSchedulerFailurePolicy::Backoff {
                initial_backoff,
                max_backoff,
            } => {
                registered.consecutive_failures = registered.consecutive_failures.saturating_add(1);
                registered.version = registered.version.saturating_add(1);
                let next_version = registered.version;
                let next_due_at = Instant::now()
                    + next_backoff_duration(
                        registered.consecutive_failures,
                        initial_backoff,
                        max_backoff,
                    );
                state.timing_queue.push(WorkerSchedulerTimingQueueEntry {
                    due_at: next_due_at,
                    task_id,
                    version: next_version,
                });
            }
        },
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

async fn execute_task(
    lifecycle: &WorkerLifecycle,
    task: &WorkerSchedulerTask,
) -> WorkerSchedulerExecutionOutcome {
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
) -> WorkerSchedulerExecutionOutcome {
    match selector {
        WorkerSchedulerTargetSelector::Session(session_id) => {
            let snapshot = lifecycle.snapshot(session_id).await;
            let Some(snapshot) = snapshot else {
                return WorkerSchedulerExecutionOutcome::CancelTask;
            };
            if snapshot.state != WorkerSessionState::Running {
                return WorkerSchedulerExecutionOutcome::CancelTask;
            }
            if suppress_when_focused && snapshot.visibility == WorkerSessionVisibility::Focused {
                return WorkerSchedulerExecutionOutcome::Continue;
            }
            match lifecycle.send_input(session_id, prompt).await {
                Ok(()) => WorkerSchedulerExecutionOutcome::Continue,
                Err(_) => WorkerSchedulerExecutionOutcome::Failed,
            }
        }
        WorkerSchedulerTargetSelector::RunningSessions => {
            let sessions = lifecycle.list_sessions().await;
            let mut has_failures = false;

            for session in sessions
                .into_iter()
                .filter(|session| session.state == WorkerSessionState::Running)
            {
                if suppress_when_focused && session.visibility == WorkerSessionVisibility::Focused {
                    continue;
                }

                if lifecycle
                    .send_input(&session.session_id, prompt)
                    .await
                    .is_err()
                {
                    has_failures = true;
                }
            }

            if has_failures {
                WorkerSchedulerExecutionOutcome::Failed
            } else {
                WorkerSchedulerExecutionOutcome::Continue
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
        WorkerScheduler, WorkerSchedulerFailurePolicy, WorkerSchedulerState,
        WorkerSchedulerTargetSelector, WorkerSchedulerTask,
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
}
