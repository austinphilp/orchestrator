//! Worker runtime composition layer for lifecycle, eventbus, and scheduler crates.

use std::sync::Arc;

use orchestrator_worker_eventbus::{
    WorkerEventBus, WorkerEventBusPerfSnapshot, WorkerGlobalEventSubscription,
    WorkerSessionEventSubscription,
};
use orchestrator_worker_lifecycle::{
    WorkerLifecycle, WorkerLifecycleBackend, WorkerLifecycleSummary, WorkerSessionState,
    WorkerSessionVisibility,
};
use orchestrator_worker_protocol::backend::{WorkerBackendCapabilities, WorkerBackendKind};
use orchestrator_worker_protocol::error::WorkerRuntimeResult;
use orchestrator_worker_protocol::event::WorkerNeedsInputAnswer;
use orchestrator_worker_protocol::ids::WorkerSessionId;
use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
use orchestrator_worker_scheduler::WorkerScheduler;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimePerfSnapshot {
    pub eventbus: WorkerEventBusPerfSnapshot,
    pub active_stream_ingestion_tasks: usize,
}

pub struct WorkerRuntime {
    lifecycle: WorkerLifecycle,
    eventbus: Arc<WorkerEventBus>,
    scheduler: WorkerScheduler,
}

impl WorkerRuntime {
    pub fn new(backend: Arc<dyn WorkerLifecycleBackend>) -> Self {
        Self::with_eventbus(backend, WorkerEventBus::default())
    }

    pub fn with_eventbus(
        backend: Arc<dyn WorkerLifecycleBackend>,
        eventbus: WorkerEventBus,
    ) -> Self {
        Self::with_components(backend, eventbus, WorkerScheduler::default())
    }

    pub fn with_components(
        backend: Arc<dyn WorkerLifecycleBackend>,
        eventbus: WorkerEventBus,
        scheduler: WorkerScheduler,
    ) -> Self {
        let eventbus = Arc::new(eventbus);
        let lifecycle = WorkerLifecycle::new(backend, Arc::clone(&eventbus));
        Self {
            lifecycle,
            eventbus,
            scheduler,
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
        self.lifecycle.spawn(spec).await
    }

    pub async fn kill(&self, session_id: &WorkerSessionId) -> WorkerRuntimeResult<()> {
        self.lifecycle.kill(session_id).await
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
    ) -> WorkerRuntimeResult<WorkerSessionEventSubscription> {
        self.lifecycle.subscribe_session(session_id).await
    }

    pub fn subscribe_all(&self) -> WorkerGlobalEventSubscription {
        self.eventbus.subscribe_all()
    }

    pub fn scheduler(&self) -> &WorkerScheduler {
        &self.scheduler
    }

    pub async fn perf_snapshot(&self) -> WorkerRuntimePerfSnapshot {
        WorkerRuntimePerfSnapshot {
            eventbus: self.eventbus.perf_snapshot(),
            active_stream_ingestion_tasks: self
                .lifecycle
                .task_snapshot()
                .await
                .active_stream_ingestion_tasks,
        }
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
    use orchestrator_worker_protocol::error::WorkerRuntimeResult;
    use orchestrator_worker_protocol::event::{
        WorkerDoneEvent, WorkerEvent, WorkerNeedsInputAnswer, WorkerOutputEvent, WorkerOutputStream,
    };
    use orchestrator_worker_protocol::ids::WorkerSessionId;
    use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
    use orchestrator_worker_scheduler::WorkerScheduler;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout, Duration};

    use super::WorkerRuntime;

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
            _session: &WorkerSessionHandle,
            _input: &[u8],
        ) -> WorkerRuntimeResult<()> {
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

    #[tokio::test]
    async fn runtime_wires_lifecycle_controls_and_eventbus_stream() {
        let backend = Arc::new(MockBackend::default());
        let runtime = WorkerRuntime::with_components(
            backend.clone(),
            WorkerEventBus::default(),
            WorkerScheduler::default(),
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
        let runtime = WorkerRuntime::new(backend.clone());

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
}
