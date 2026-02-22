//! Worker runtime composition layer for lifecycle, eventbus, and scheduler crates.

use std::sync::Arc;
use std::time::Duration;

use orchestrator_worker_eventbus::{
    WorkerEventBus, WorkerEventBusConfig, WorkerEventBusPerfSnapshot, WorkerEventEnvelope,
    WorkerGlobalEventSubscription, WorkerSessionEventSubscription, DEFAULT_GLOBAL_BUFFER_CAPACITY,
    DEFAULT_SESSION_BUFFER_CAPACITY,
};
use orchestrator_worker_lifecycle::{
    WorkerLifecycle, WorkerLifecycleBackend, WorkerLifecyclePerfSnapshot, WorkerLifecycleSummary,
    WorkerSessionState, WorkerSessionVisibility,
};
use orchestrator_worker_protocol::backend::{WorkerBackendCapabilities, WorkerBackendKind};
use orchestrator_worker_protocol::error::WorkerRuntimeResult;
use orchestrator_worker_protocol::event::{WorkerEvent, WorkerNeedsInputAnswer};
use orchestrator_worker_protocol::ids::WorkerSessionId;
use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
use orchestrator_worker_scheduler::{
    WorkerScheduler, WorkerSchedulerDiagnosticEvent, WorkerSchedulerPerfSnapshot,
    WorkerSchedulerTask,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const DEFAULT_CHECKPOINT_PROMPT_INTERVAL_SECS: u64 = 120;
const DEFAULT_CHECKPOINT_PROMPT_MESSAGE: &str = "Emit a checkpoint now.";

pub trait WorkerRuntimeBackend: WorkerLifecycleBackend {}

impl<T> WorkerRuntimeBackend for T where T: WorkerLifecycleBackend + ?Sized {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimePerfSnapshot {
    pub lifecycle: WorkerLifecyclePerfSnapshot,
    pub eventbus: WorkerEventBusPerfSnapshot,
    pub scheduler: WorkerSchedulerPerfSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimeEventEnvelope {
    pub session_id: WorkerSessionId,
    pub sequence: u64,
    pub received_at_monotonic_nanos: u64,
    pub event: WorkerEvent,
}

impl From<WorkerEventEnvelope> for WorkerRuntimeEventEnvelope {
    fn from(value: WorkerEventEnvelope) -> Self {
        Self {
            session_id: value.session_id,
            sequence: value.sequence,
            received_at_monotonic_nanos: value.received_at_monotonic_nanos,
            event: value.event,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimeConfig {
    pub session_event_buffer: usize,
    pub global_event_buffer: usize,
    pub scheduler: WorkerRuntimeSchedulerConfig,
}

impl Default for WorkerRuntimeConfig {
    fn default() -> Self {
        Self {
            session_event_buffer: DEFAULT_SESSION_BUFFER_CAPACITY,
            global_event_buffer: DEFAULT_GLOBAL_BUFFER_CAPACITY,
            scheduler: WorkerRuntimeSchedulerConfig::default(),
        }
    }
}

pub struct WorkerRuntimeSessionSubscription {
    inner: WorkerSessionEventSubscription,
}

impl WorkerRuntimeSessionSubscription {
    pub async fn next_event(&mut self) -> Option<WorkerRuntimeEventEnvelope> {
        self.inner.next_event().await.map(Into::into)
    }
}

pub struct WorkerRuntimeGlobalSubscription {
    inner: WorkerGlobalEventSubscription,
}

impl WorkerRuntimeGlobalSubscription {
    pub async fn next_event(&mut self) -> Option<WorkerRuntimeEventEnvelope> {
        self.inner.next_event().await.map(Into::into)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimeSchedulerConfig {
    pub checkpoint_prompt_interval: Option<Duration>,
    pub checkpoint_prompt_message: String,
}

impl Default for WorkerRuntimeSchedulerConfig {
    fn default() -> Self {
        Self {
            checkpoint_prompt_interval: Some(Duration::from_secs(
                DEFAULT_CHECKPOINT_PROMPT_INTERVAL_SECS,
            )),
            checkpoint_prompt_message: DEFAULT_CHECKPOINT_PROMPT_MESSAGE.to_owned(),
        }
    }
}

impl WorkerRuntimeSchedulerConfig {
    fn checkpoint_schedule(&self) -> Option<WorkerRuntimeCheckpointSchedule> {
        let interval = self.checkpoint_prompt_interval?;
        if interval.is_zero() {
            return None;
        }

        let prompt = self.checkpoint_prompt_message.trim();
        if prompt.is_empty() {
            return None;
        }
        let mut prompt_bytes = prompt.as_bytes().to_vec();
        if !prompt_bytes.ends_with(b"\n") {
            prompt_bytes.push(b'\n');
        }

        Some(WorkerRuntimeCheckpointSchedule {
            interval,
            prompt: prompt_bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkerRuntimeCheckpointSchedule {
    interval: Duration,
    prompt: Vec<u8>,
}

pub struct WorkerRuntime {
    lifecycle: WorkerLifecycle,
    eventbus: Arc<WorkerEventBus>,
    scheduler: WorkerScheduler,
    checkpoint_schedule: Option<WorkerRuntimeCheckpointSchedule>,
    terminal_scheduler_sync_task: JoinHandle<()>,
}

impl WorkerRuntime {
    pub fn new(backend: Arc<dyn WorkerRuntimeBackend>) -> Self {
        Self::with_config(backend, WorkerRuntimeConfig::default())
    }

    pub fn with_config(
        backend: Arc<dyn WorkerRuntimeBackend>,
        config: WorkerRuntimeConfig,
    ) -> Self {
        Self::with_eventbus_and_scheduler_config(
            backend,
            WorkerEventBus::new(WorkerEventBusConfig {
                session_buffer_capacity: config.session_event_buffer.max(1),
                global_buffer_capacity: config.global_event_buffer.max(1),
            }),
            config.scheduler,
        )
    }

    pub fn with_eventbus(backend: Arc<dyn WorkerRuntimeBackend>, eventbus: WorkerEventBus) -> Self {
        Self::with_eventbus_and_scheduler_config(
            backend,
            eventbus,
            WorkerRuntimeSchedulerConfig::default(),
        )
    }

    pub fn with_eventbus_and_scheduler_config(
        backend: Arc<dyn WorkerRuntimeBackend>,
        eventbus: WorkerEventBus,
        scheduler_config: WorkerRuntimeSchedulerConfig,
    ) -> Self {
        let backend: Arc<dyn WorkerLifecycleBackend> = backend;
        let eventbus = Arc::new(eventbus);
        let lifecycle = WorkerLifecycle::new(backend, Arc::clone(&eventbus));
        let scheduler = WorkerScheduler::new(lifecycle.clone());
        let checkpoint_schedule = scheduler_config.checkpoint_schedule();
        let mut terminal_events = eventbus.subscribe_all();
        let scheduler_for_terminal = scheduler.clone();
        let terminal_scheduler_sync_task = tokio::spawn(async move {
            while let Some(envelope) = terminal_events.next_event().await {
                if matches!(
                    envelope.event,
                    WorkerEvent::Done(_) | WorkerEvent::Crashed(_)
                ) {
                    scheduler_for_terminal.cancel_for_session(&envelope.session_id);
                }
            }
        });

        Self {
            lifecycle,
            eventbus,
            scheduler,
            checkpoint_schedule,
            terminal_scheduler_sync_task,
        }
    }

    pub fn backend_kind(&self) -> WorkerBackendKind {
        self.lifecycle.backend_kind()
    }

    pub fn backend_capabilities(&self) -> WorkerBackendCapabilities {
        self.lifecycle.backend_capabilities()
    }

    pub async fn health_check(&self) -> WorkerRuntimeResult<()> {
        self.lifecycle.health_check().await
    }

    pub async fn spawn(
        &self,
        spec: WorkerSpawnRequest,
    ) -> WorkerRuntimeResult<WorkerSessionHandle> {
        let handle = self.lifecycle.spawn(spec).await?;

        if let Some(checkpoint_schedule) = &self.checkpoint_schedule {
            if let Err(error) =
                self.scheduler
                    .register(WorkerSchedulerTask::checkpoint_for_session(
                        handle.session_id.clone(),
                        checkpoint_schedule.interval,
                        checkpoint_schedule.prompt.clone(),
                    ))
            {
                let _ = self.lifecycle.kill(&handle.session_id).await;
                return Err(error);
            }
        }

        Ok(handle)
    }

    pub async fn kill(&self, session_id: &WorkerSessionId) -> WorkerRuntimeResult<()> {
        let result = self.lifecycle.kill(session_id).await;
        self.scheduler.cancel_for_session(session_id);
        result
    }

    pub async fn send_input(
        &self,
        session_id: &WorkerSessionId,
        input: &[u8],
    ) -> WorkerRuntimeResult<()> {
        self.lifecycle.send_input(session_id, input).await
    }

    pub async fn respond_to_needs_input(
        &self,
        session_id: &WorkerSessionId,
        prompt_id: &str,
        answers: &[WorkerNeedsInputAnswer],
    ) -> WorkerRuntimeResult<()> {
        self.lifecycle
            .respond_to_needs_input(session_id, prompt_id, answers)
            .await
    }

    pub async fn resize(
        &self,
        session_id: &WorkerSessionId,
        cols: u16,
        rows: u16,
    ) -> WorkerRuntimeResult<()> {
        self.lifecycle.resize(session_id, cols, rows).await
    }

    pub async fn focus_session(
        &self,
        focused: Option<&WorkerSessionId>,
    ) -> WorkerRuntimeResult<()> {
        self.lifecycle.focus_session(focused).await
    }

    pub async fn set_visibility(
        &self,
        session_id: &WorkerSessionId,
        visibility: WorkerSessionVisibility,
    ) -> WorkerRuntimeResult<()> {
        self.lifecycle.set_visibility(session_id, visibility).await
    }

    pub async fn list_sessions(&self) -> Vec<WorkerLifecycleSummary> {
        self.lifecycle.list_sessions().await
    }

    pub async fn status(
        &self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerSessionState> {
        self.lifecycle.status(session_id).await
    }

    pub async fn subscribe_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerRuntimeSessionSubscription> {
        let subscription = self.lifecycle.subscribe_session(session_id).await?;
        Ok(WorkerRuntimeSessionSubscription {
            inner: subscription,
        })
    }

    pub fn subscribe_all(&self) -> WorkerRuntimeGlobalSubscription {
        WorkerRuntimeGlobalSubscription {
            inner: self.eventbus.subscribe_all(),
        }
    }

    pub fn scheduler(&self) -> &WorkerScheduler {
        &self.scheduler
    }

    pub fn subscribe_scheduler_diagnostics(
        &self,
    ) -> broadcast::Receiver<WorkerSchedulerDiagnosticEvent> {
        self.scheduler.subscribe_diagnostics()
    }

    pub async fn perf_snapshot(&self) -> WorkerRuntimePerfSnapshot {
        let lifecycle = self.lifecycle.perf_snapshot().await;
        WorkerRuntimePerfSnapshot {
            lifecycle,
            eventbus: self.eventbus.perf_snapshot(),
            scheduler: self.scheduler.perf_snapshot(),
        }
    }
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.terminal_scheduler_sync_task.abort();
        self.scheduler.close();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use orchestrator_worker_eventbus::WorkerEventBus;
    use orchestrator_worker_lifecycle::{WorkerSessionState, WorkerSessionVisibility};
    use orchestrator_worker_protocol::backend::{
        WorkerBackendCapabilities, WorkerBackendInfo, WorkerBackendKind, WorkerEventStream,
        WorkerEventSubscription, WorkerSessionControl, WorkerSessionStreamSource,
    };
    use orchestrator_worker_protocol::error::{WorkerRuntimeError, WorkerRuntimeResult};
    use orchestrator_worker_protocol::event::{
        WorkerDoneEvent, WorkerEvent, WorkerNeedsInputAnswer, WorkerOutputEvent, WorkerOutputStream,
    };
    use orchestrator_worker_protocol::ids::WorkerSessionId;
    use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout, Duration};

    use super::{WorkerRuntime, WorkerRuntimeSchedulerConfig};

    type StreamMessage = WorkerRuntimeResult<Option<WorkerEvent>>;

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
        fn emit_event(&self, session_id: &WorkerSessionId, event: WorkerEvent) {
            let sender = {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .event_tx
                    .as_ref()
                    .expect("stream sender")
                    .clone()
            };
            sender.send(Ok(Some(event))).expect("emit event");
        }

        fn sent_input(&self, session_id: &WorkerSessionId) -> Vec<Vec<u8>> {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .sent_input
                .clone()
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
                orchestrator_worker_protocol::error::WorkerRuntimeError::SessionNotFound(
                    session.session_id.as_str().to_owned(),
                )
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
                .expect("single subscriber stream");
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

    async fn wait_for_status(
        runtime: &WorkerRuntime,
        session_id: &WorkerSessionId,
        expected: WorkerSessionState,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let status = runtime.status(session_id).await.expect("status");
            if status == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for status transition to {expected:?}; observed {status:?}"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_sent_input_count(
        backend: &MockBackend,
        session_id: &WorkerSessionId,
        expected: usize,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let count = backend.sent_input(session_id).len();
            if count >= expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for scheduled send_input count {expected}; observed {count}"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn runtime_wires_lifecycle_controls_and_eventbus_stream() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
            backend.clone(),
            WorkerEventBus::default(),
            WorkerRuntimeSchedulerConfig {
                checkpoint_prompt_interval: None,
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
            },
        );

        let handle = runtime
            .spawn(spawn_request("runtime-session"))
            .await
            .expect("spawn session");
        let mut subscription = runtime
            .subscribe_session(&handle.session_id)
            .await
            .expect("subscribe session");

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Output(WorkerOutputEvent {
                stream: WorkerOutputStream::Stdout,
                bytes: b"runtime-output".to_vec(),
            }),
        );
        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent { summary: None }),
        );

        let output = timeout(Duration::from_secs(2), subscription.next_event())
            .await
            .expect("output timeout")
            .expect("output event");
        assert!(matches!(output.event, WorkerEvent::Output(_)));

        let done = timeout(Duration::from_secs(2), subscription.next_event())
            .await
            .expect("done timeout")
            .expect("done event");
        assert!(matches!(done.event, WorkerEvent::Done(_)));

        let closed = timeout(Duration::from_secs(2), subscription.next_event())
            .await
            .expect("close timeout");
        assert!(closed.is_none());

        assert_eq!(
            runtime.status(&handle.session_id).await.expect("status"),
            WorkerSessionState::Done
        );
    }

    #[tokio::test]
    async fn runtime_global_subscription_exposes_runtime_envelope_shape() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
            backend.clone(),
            WorkerEventBus::default(),
            WorkerRuntimeSchedulerConfig {
                checkpoint_prompt_interval: None,
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
            },
        );
        let mut global_subscription = runtime.subscribe_all();
        let handle = runtime
            .spawn(spawn_request("runtime-global-envelope"))
            .await
            .expect("spawn session");

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Output(WorkerOutputEvent {
                stream: WorkerOutputStream::Stdout,
                bytes: b"global-envelope".to_vec(),
            }),
        );

        let envelope = timeout(Duration::from_secs(2), global_subscription.next_event())
            .await
            .expect("global subscription timeout")
            .expect("global subscription event");
        assert_eq!(envelope.session_id, handle.session_id);
        assert_eq!(envelope.sequence, 1);
        assert!(envelope.received_at_monotonic_nanos > 0);
        match envelope.event {
            WorkerEvent::Output(output) => assert_eq!(output.bytes, b"global-envelope"),
            other => panic!("expected output event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn runtime_focus_visibility_controls_follow_lifecycle_state() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::new(backend.clone());
        let session_a = runtime
            .spawn(spawn_request("runtime-focus-a"))
            .await
            .expect("spawn a");
        let session_b = runtime
            .spawn(spawn_request("runtime-focus-b"))
            .await
            .expect("spawn b");

        runtime
            .focus_session(Some(&session_a.session_id))
            .await
            .expect("focus a");
        runtime
            .set_visibility(&session_b.session_id, WorkerSessionVisibility::Focused)
            .await
            .expect("focus b");

        let sessions = runtime.list_sessions().await;
        let a = sessions
            .iter()
            .find(|summary| summary.session_id == session_a.session_id)
            .expect("session a");
        let b = sessions
            .iter()
            .find(|summary| summary.session_id == session_b.session_id)
            .expect("session b");
        assert_eq!(a.visibility, WorkerSessionVisibility::Background);
        assert_eq!(b.visibility, WorkerSessionVisibility::Focused);
    }

    #[tokio::test]
    async fn runtime_rejects_subscriptions_for_missing_or_closed_sessions() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
            backend.clone(),
            WorkerEventBus::default(),
            WorkerRuntimeSchedulerConfig {
                checkpoint_prompt_interval: None,
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
            },
        );

        let missing = WorkerSessionId::new("runtime-missing");
        assert!(matches!(
            runtime.subscribe_session(&missing).await,
            Err(orchestrator_worker_protocol::error::WorkerRuntimeError::SessionNotFound(_))
        ));

        let handle = runtime
            .spawn(spawn_request("runtime-closed"))
            .await
            .expect("spawn");
        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent { summary: None }),
        );
        wait_for_status(&runtime, &handle.session_id, WorkerSessionState::Done).await;
        assert!(matches!(
            runtime.subscribe_session(&handle.session_id).await,
            Err(orchestrator_worker_protocol::error::WorkerRuntimeError::Process(_))
        ));
    }

    #[tokio::test]
    async fn runtime_scheduler_registers_checkpoint_task_and_cancels_on_done() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
            backend.clone(),
            WorkerEventBus::default(),
            WorkerRuntimeSchedulerConfig {
                checkpoint_prompt_interval: Some(Duration::from_millis(35)),
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
            },
        );

        let handle = runtime
            .spawn(spawn_request("runtime-scheduler-terminal"))
            .await
            .expect("spawn session");
        wait_for_sent_input_count(&backend, &handle.session_id, 1).await;
        let snapshot = runtime.perf_snapshot().await;
        assert_eq!(snapshot.lifecycle.spawn_requests_total, 1);
        assert_eq!(snapshot.lifecycle.spawn_success_total, 1);
        assert_eq!(snapshot.lifecycle.running_sessions, 1);
        assert_eq!(snapshot.lifecycle.active_stream_ingestion_tasks, 1);
        assert!(snapshot.scheduler.task_dispatches_total >= 1);
        assert!(snapshot.scheduler.checkpoint_send_attempts_total >= 1);
        assert_eq!(runtime.scheduler().task_count(), 1);

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent {
                summary: Some("done".to_owned()),
            }),
        );
        wait_for_status(&runtime, &handle.session_id, WorkerSessionState::Done).await;

        timeout(Duration::from_secs(2), async {
            loop {
                if runtime.scheduler().task_count() == 0 {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for scheduler task cancellation");

        let terminal_snapshot = runtime.perf_snapshot().await;
        assert_eq!(terminal_snapshot.lifecycle.done_sessions, 1);
        assert_eq!(terminal_snapshot.lifecycle.active_stream_ingestion_tasks, 0);
        assert_eq!(
            terminal_snapshot.lifecycle.terminal_transitions_done_total,
            1
        );
    }

    #[tokio::test]
    async fn runtime_spawn_rolls_back_when_scheduler_registration_fails() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
            backend.clone(),
            WorkerEventBus::default(),
            WorkerRuntimeSchedulerConfig {
                checkpoint_prompt_interval: Some(Duration::from_millis(35)),
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
            },
        );
        runtime.scheduler().close();

        let session_id = WorkerSessionId::new("runtime-scheduler-register-failure");
        let spawn_error = runtime
            .spawn(spawn_request(session_id.as_str()))
            .await
            .expect_err("spawn should fail when scheduler is closed");
        assert!(matches!(
            spawn_error,
            WorkerRuntimeError::Process(message) if message.contains("scheduler is closed")
        ));
        wait_for_status(&runtime, &session_id, WorkerSessionState::Killed).await;
        assert_eq!(runtime.scheduler().task_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn perf_fanout_overhead_50_100_200_workers() {
        for workers in [50usize, 100, 200] {
            let backend = Arc::new(MockBackend::default());
            let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
                backend.clone(),
                WorkerEventBus::default(),
                WorkerRuntimeSchedulerConfig {
                    checkpoint_prompt_interval: None,
                    checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                },
            );

            let mut subscriptions = Vec::with_capacity(workers);
            let mut session_ids = Vec::with_capacity(workers);
            for idx in 0..workers {
                let handle = runtime
                    .spawn(spawn_request(&format!("perf-fanout-{workers}-{idx}")))
                    .await
                    .expect("spawn perf fanout worker");
                let subscription = runtime
                    .subscribe_session(&handle.session_id)
                    .await
                    .expect("subscribe perf fanout worker");
                subscriptions.push(subscription);
                session_ids.push(handle.session_id);
            }

            let started = tokio::time::Instant::now();
            for session_id in &session_ids {
                backend.emit_event(
                    session_id,
                    WorkerEvent::Output(WorkerOutputEvent {
                        stream: WorkerOutputStream::Stdout,
                        bytes: b"fanout".to_vec(),
                    }),
                );
            }

            for subscription in &mut subscriptions {
                let event = timeout(Duration::from_secs(5), subscription.next_event())
                    .await
                    .expect("fanout receive timeout")
                    .expect("fanout receive event");
                assert!(matches!(event.event, WorkerEvent::Output(_)));
            }

            let elapsed_ms = started.elapsed().as_millis();
            let snapshot = runtime.perf_snapshot().await;
            eprintln!(
                "perf fanout workers={workers} elapsed_ms={elapsed_ms} stream_events_forwarded_total={} events_published_total={} queue_pressure_dropped_events_total={} output_truncation_markers_total={}",
                snapshot.lifecycle.stream_events_forwarded_total,
                snapshot.eventbus.events_published_total,
                snapshot.eventbus.queue_pressure_dropped_events_total,
                snapshot.eventbus.output_truncation_markers_total,
            );

            assert_eq!(snapshot.lifecycle.running_sessions as usize, workers);
            assert_eq!(snapshot.eventbus.events_published_total as usize, workers);

            for session_id in &session_ids {
                runtime
                    .kill(session_id)
                    .await
                    .expect("kill perf fanout worker");
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn perf_scheduler_overhead_50_100_200_workers() {
        for workers in [50usize, 100, 200] {
            let backend = Arc::new(MockBackend::default());
            let runtime = WorkerRuntime::with_eventbus_and_scheduler_config(
                backend.clone(),
                WorkerEventBus::default(),
                WorkerRuntimeSchedulerConfig {
                    checkpoint_prompt_interval: Some(Duration::from_millis(35)),
                    checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                },
            );

            let mut session_ids = Vec::with_capacity(workers);
            for idx in 0..workers {
                let handle = runtime
                    .spawn(spawn_request(&format!("perf-scheduler-{workers}-{idx}")))
                    .await
                    .expect("spawn perf scheduler worker");
                session_ids.push(handle.session_id);
            }

            let started = tokio::time::Instant::now();
            timeout(Duration::from_secs(10), async {
                loop {
                    let snapshot = runtime.perf_snapshot().await;
                    if snapshot.scheduler.checkpoint_send_attempts_total as usize >= workers {
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("scheduler perf timeout");

            let elapsed_ms = started.elapsed().as_millis();
            let snapshot = runtime.perf_snapshot().await;
            eprintln!(
                "perf scheduler workers={workers} elapsed_ms={elapsed_ms} task_dispatches_total={} checkpoint_send_attempts_total={} checkpoint_send_failures_total={} active_tasks={}",
                snapshot.scheduler.task_dispatches_total,
                snapshot.scheduler.checkpoint_send_attempts_total,
                snapshot.scheduler.checkpoint_send_failures_total,
                runtime.scheduler().task_count(),
            );

            assert!(snapshot.scheduler.checkpoint_send_attempts_total as usize >= workers);
            assert_eq!(snapshot.scheduler.checkpoint_send_failures_total, 0);
            assert_eq!(runtime.scheduler().task_count(), workers);

            for session_id in &session_ids {
                runtime
                    .kill(session_id)
                    .await
                    .expect("kill perf scheduler worker");
            }
        }
    }
}
