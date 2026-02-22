mod runtime_stream_coordinator {
    use std::sync::Arc;

    use async_trait::async_trait;
    use orchestrator_core::{
        BackendCapabilities, BackendEvent, BackendKind, RuntimeResult, SessionHandle,
        SessionLifecycle, SpawnSpec, WorkerBackend, WorkerEventStream, WorkerEventSubscription,
        WorkerManagerConfig,
    };
    use orchestrator_worker_protocol::backend::{
        WorkerBackendInfo, WorkerSessionControl, WorkerSessionStreamSource,
    };
    use orchestrator_worker_protocol::event::WorkerNeedsInputAnswer as BackendNeedsInputAnswer;
    use orchestrator_worker_runtime::{
        WorkerRuntime, WorkerRuntimeBackend, WorkerRuntimeConfig, WorkerRuntimeSchedulerConfig,
        WorkerRuntimeSessionSubscription,
    };

    #[derive(Clone)]
    pub struct WorkerManagerBackend {
        runtime: Arc<WorkerRuntime>,
        backend: Arc<dyn WorkerBackend + Send + Sync>,
    }

    impl WorkerManagerBackend {
        pub fn new(backend: Arc<dyn WorkerBackend + Send + Sync>) -> Self {
            Self::with_config(backend, WorkerManagerConfig::default())
        }

        pub fn with_config(
            backend: Arc<dyn WorkerBackend + Send + Sync>,
            config: WorkerManagerConfig,
        ) -> Self {
            let runtime_backend: Arc<dyn WorkerRuntimeBackend> =
                Arc::new(WorkerBackendProtocolAdapter::new(backend.clone()));
            let runtime = Arc::new(WorkerRuntime::with_config(
                runtime_backend,
                WorkerRuntimeConfig {
                    session_event_buffer: config.session_event_buffer,
                    global_event_buffer: config.global_event_buffer,
                    scheduler: WorkerRuntimeSchedulerConfig {
                        checkpoint_prompt_interval: config.checkpoint_prompt_interval,
                        checkpoint_prompt_message: config.checkpoint_prompt_message,
                    },
                },
            ));
            Self { runtime, backend }
        }
    }

    struct WorkerBackendProtocolAdapter {
        backend: Arc<dyn WorkerBackend + Send + Sync>,
    }

    impl WorkerBackendProtocolAdapter {
        fn new(backend: Arc<dyn WorkerBackend + Send + Sync>) -> Self {
            Self { backend }
        }
    }

    #[async_trait]
    impl WorkerSessionControl for WorkerBackendProtocolAdapter {
        async fn spawn(
            &self,
            spec: orchestrator_worker_protocol::session::WorkerSpawnRequest,
        ) -> RuntimeResult<orchestrator_worker_protocol::session::WorkerSessionHandle> {
            self.backend.spawn(spec).await
        }

        async fn kill(
            &self,
            session: &orchestrator_worker_protocol::session::WorkerSessionHandle,
        ) -> RuntimeResult<()> {
            self.backend.kill(session).await
        }

        async fn send_input(
            &self,
            session: &orchestrator_worker_protocol::session::WorkerSessionHandle,
            input: &[u8],
        ) -> RuntimeResult<()> {
            self.backend.send_input(session, input).await
        }

        async fn respond_to_needs_input(
            &self,
            session: &orchestrator_worker_protocol::session::WorkerSessionHandle,
            prompt_id: &str,
            answers: &[BackendNeedsInputAnswer],
        ) -> RuntimeResult<()> {
            self.backend
                .respond_to_needs_input(session, prompt_id, answers)
                .await
        }

        async fn resize(
            &self,
            session: &orchestrator_worker_protocol::session::WorkerSessionHandle,
            cols: u16,
            rows: u16,
        ) -> RuntimeResult<()> {
            self.backend.resize(session, cols, rows).await
        }
    }

    #[async_trait]
    impl WorkerSessionStreamSource for WorkerBackendProtocolAdapter {
        async fn subscribe(
            &self,
            session: &orchestrator_worker_protocol::session::WorkerSessionHandle,
        ) -> RuntimeResult<orchestrator_worker_protocol::backend::WorkerEventStream> {
            self.backend.subscribe(session).await
        }

        async fn harness_session_id(
            &self,
            session: &orchestrator_worker_protocol::session::WorkerSessionHandle,
        ) -> RuntimeResult<Option<String>> {
            self.backend.harness_session_id(session).await
        }
    }

    #[async_trait]
    impl WorkerBackendInfo for WorkerBackendProtocolAdapter {
        fn kind(&self) -> orchestrator_worker_protocol::backend::WorkerBackendKind {
            self.backend.kind()
        }

        fn capabilities(&self) -> orchestrator_worker_protocol::backend::WorkerBackendCapabilities {
            self.backend.capabilities()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            self.backend.health_check().await
        }
    }

    struct WorkerRuntimeSessionStream {
        subscription: WorkerRuntimeSessionSubscription,
    }

    #[async_trait]
    impl WorkerEventSubscription for WorkerRuntimeSessionStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            Ok(self
                .subscription
                .next_event()
                .await
                .map(|envelope| envelope.event))
        }
    }

    #[async_trait]
    impl SessionLifecycle for WorkerManagerBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.runtime.spawn(spec).await
        }

        async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
            self.runtime.kill(&session.session_id).await
        }

        async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
            self.runtime.send_input(&session.session_id, input).await
        }

        async fn respond_to_needs_input(
            &self,
            session: &SessionHandle,
            prompt_id: &str,
            answers: &[BackendNeedsInputAnswer],
        ) -> RuntimeResult<()> {
            self.runtime
                .respond_to_needs_input(&session.session_id, prompt_id, answers)
                .await
        }

        async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()> {
            self.runtime.resize(&session.session_id, cols, rows).await
        }
    }

    #[async_trait]
    impl WorkerBackend for WorkerManagerBackend {
        fn kind(&self) -> BackendKind {
            self.runtime.backend_kind()
        }

        fn capabilities(&self) -> BackendCapabilities {
            self.runtime.backend_capabilities()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            self.runtime.health_check().await
        }

        async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
            let subscription = self.runtime.subscribe_session(&session.session_id).await?;
            Ok(Box::new(WorkerRuntimeSessionStream { subscription }))
        }

        async fn harness_session_id(
            &self,
            session: &SessionHandle,
        ) -> RuntimeResult<Option<String>> {
            self.backend.harness_session_id(session).await
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use async_trait::async_trait;
        use orchestrator_core::{
            BackendOutputEvent, BackendOutputStream, RuntimeError, RuntimeSessionId,
        };
        use tokio::sync::mpsc;
        use tokio::time::timeout;

        use super::*;

        type StreamMessage = RuntimeResult<Option<BackendEvent>>;

        #[derive(Default)]
        struct MockBackend {
            state: Mutex<MockBackendState>,
        }

        #[derive(Default)]
        struct MockBackendState {
            sessions: HashMap<RuntimeSessionId, MockSession>,
        }

        struct MockSession {
            event_tx: Option<mpsc::UnboundedSender<StreamMessage>>,
            event_rx: Option<mpsc::UnboundedReceiver<StreamMessage>>,
            sent_input: Vec<Vec<u8>>,
            needs_input_responses: Vec<(String, Vec<BackendNeedsInputAnswer>)>,
            killed: bool,
        }

        struct MockEventStream {
            receiver: mpsc::UnboundedReceiver<StreamMessage>,
        }

        impl MockBackend {
            fn emit_event(&self, session_id: &RuntimeSessionId, event: BackendEvent) {
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
                sender.send(Ok(Some(event))).expect("emit mock event");
            }

            fn emit_error(&self, session_id: &RuntimeSessionId, error: RuntimeError) {
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
                sender.send(Err(error)).expect("emit mock stream error");
            }

            fn sent_input(&self, session_id: &RuntimeSessionId) -> Vec<Vec<u8>> {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .sent_input
                    .clone()
            }

            fn needs_input_responses(
                &self,
                session_id: &RuntimeSessionId,
            ) -> Vec<(String, Vec<BackendNeedsInputAnswer>)> {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .needs_input_responses
                    .clone()
            }

            fn is_killed(&self, session_id: &RuntimeSessionId) -> bool {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .killed
            }
        }

        #[async_trait]
        impl SessionLifecycle for MockBackend {
            async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
                let mut state = self.state.lock().expect("lock backend state");
                if state.sessions.contains_key(&spec.session_id) {
                    return Err(RuntimeError::Configuration(format!(
                        "duplicate mock session id: {}",
                        spec.session_id.as_str()
                    )));
                }
                let (event_tx, event_rx) = mpsc::unbounded_channel();
                state.sessions.insert(
                    spec.session_id.clone(),
                    MockSession {
                        event_tx: Some(event_tx),
                        event_rx: Some(event_rx),
                        sent_input: Vec::new(),
                        needs_input_responses: Vec::new(),
                        killed: false,
                    },
                );
                Ok(SessionHandle {
                    session_id: spec.session_id,
                    backend: BackendKind::OpenCode,
                })
            }

            async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
                let mut state = self.state.lock().expect("lock backend state");
                let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                    RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
                })?;
                session.killed = true;
                Ok(())
            }

            async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
                let mut state = self.state.lock().expect("lock backend state");
                let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                    RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
                })?;
                session.sent_input.push(input.to_vec());
                Ok(())
            }

            async fn respond_to_needs_input(
                &self,
                session: &SessionHandle,
                prompt_id: &str,
                answers: &[BackendNeedsInputAnswer],
            ) -> RuntimeResult<()> {
                let mut state = self.state.lock().expect("lock backend state");
                let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                    RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
                })?;
                session
                    .needs_input_responses
                    .push((prompt_id.to_owned(), answers.to_vec()));
                Ok(())
            }

            async fn resize(
                &self,
                _session: &SessionHandle,
                _cols: u16,
                _rows: u16,
            ) -> RuntimeResult<()> {
                Ok(())
            }
        }

        #[async_trait]
        impl WorkerBackend for MockBackend {
            fn kind(&self) -> BackendKind {
                BackendKind::OpenCode
            }

            fn capabilities(&self) -> BackendCapabilities {
                BackendCapabilities {
                    structured_events: true,
                    session_export: false,
                    diff_provider: false,
                    supports_background: true,
                }
            }

            async fn health_check(&self) -> RuntimeResult<()> {
                Ok(())
            }

            async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
                let mut state = self.state.lock().expect("lock backend state");
                let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                    RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
                })?;
                let receiver = session.event_rx.take().ok_or_else(|| {
                    RuntimeError::Process("mock backend supports only one subscription".to_owned())
                })?;
                Ok(Box::new(MockEventStream { receiver }))
            }
        }

        #[async_trait]
        impl WorkerEventSubscription for MockEventStream {
            async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
                match self.receiver.recv().await {
                    Some(event) => event,
                    None => Ok(None),
                }
            }
        }

        fn spawn_spec(session_id: &str) -> SpawnSpec {
            SpawnSpec {
                session_id: RuntimeSessionId::new(session_id),
                workdir: std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(session_id),
                model: None,
                instruction_prelude: None,
                environment: Vec::new(),
            }
        }

        const TEST_TIMEOUT: Duration = Duration::from_secs(2);

        #[tokio::test]
        async fn coordinator_fans_out_single_backend_stream_to_multiple_subscribers() {
            let backend = Arc::new(MockBackend::default());
            let coordinator = WorkerManagerBackend::new(backend.clone());
            let handle = coordinator
                .spawn(spawn_spec("wm-coordinator-fanout"))
                .await
                .expect("spawn session");

            let mut sub_a = coordinator.subscribe(&handle).await.expect("subscribe A");
            let mut sub_b = coordinator.subscribe(&handle).await.expect("subscribe B");

            let event = BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: b"hello from coordinator".to_vec(),
            });
            backend.emit_event(&handle.session_id, event.clone());

            assert_eq!(
                sub_a.next_event().await.expect("event A"),
                Some(event.clone())
            );
            assert_eq!(sub_b.next_event().await.expect("event B"), Some(event));
        }

        #[tokio::test]
        async fn coordinator_delegates_lifecycle_input_and_needs_input_responses() {
            let backend = Arc::new(MockBackend::default());
            let coordinator = WorkerManagerBackend::new(backend.clone());
            let handle = coordinator
                .spawn(spawn_spec("wm-coordinator-lifecycle"))
                .await
                .expect("spawn session");

            coordinator
                .send_input(&handle, b"resume")
                .await
                .expect("send input");
            coordinator
                .respond_to_needs_input(
                    &handle,
                    "prompt-1",
                    &[BackendNeedsInputAnswer {
                        question_id: "prompt-1".to_owned(),
                        answers: vec!["yes".to_owned()],
                    }],
                )
                .await
                .expect("respond needs input");
            coordinator.kill(&handle).await.expect("kill session");

            assert_eq!(
                backend.sent_input(&handle.session_id),
                vec![b"resume".to_vec()]
            );
            assert_eq!(
                backend.needs_input_responses(&handle.session_id),
                vec![(
                    "prompt-1".to_owned(),
                    vec![BackendNeedsInputAnswer {
                        question_id: "prompt-1".to_owned(),
                        answers: vec!["yes".to_owned()],
                    }]
                )]
            );
            assert!(backend.is_killed(&handle.session_id));
        }

        #[tokio::test]
        async fn coordinator_rejects_subscribe_for_missing_session() {
            let backend = Arc::new(MockBackend::default());
            let coordinator = WorkerManagerBackend::new(backend);
            let missing = SessionHandle {
                session_id: RuntimeSessionId::new("wm-coordinator-missing"),
                backend: BackendKind::OpenCode,
            };

            assert!(matches!(
                coordinator.subscribe(&missing).await,
                Err(RuntimeError::SessionNotFound(_))
            ));
        }

        #[tokio::test]
        async fn coordinator_with_config_applies_session_buffer_capacity() {
            let backend = Arc::new(MockBackend::default());
            let coordinator = WorkerManagerBackend::with_config(
                backend.clone(),
                WorkerManagerConfig {
                    session_event_buffer: 1,
                    global_event_buffer: 1,
                    checkpoint_prompt_interval: None,
                    checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                    perf_metrics_enabled: true,
                },
            );
            let handle = coordinator
                .spawn(spawn_spec("wm-coordinator-buffer"))
                .await
                .expect("spawn session");
            let mut subscriber = coordinator.subscribe(&handle).await.expect("subscribe");

            for chunk in ["one", "two", "three"] {
                backend.emit_event(
                    &handle.session_id,
                    BackendEvent::Output(BackendOutputEvent {
                        stream: BackendOutputStream::Stdout,
                        bytes: chunk.as_bytes().to_vec(),
                    }),
                );
            }

            let marker = timeout(TEST_TIMEOUT, subscriber.next_event())
                .await
                .expect("marker timeout")
                .expect("marker result")
                .expect("marker event");
            match marker {
                BackendEvent::Output(output) => {
                    assert_eq!(output.stream, BackendOutputStream::Stderr);
                    assert!(String::from_utf8_lossy(&output.bytes).contains("output truncated"));
                }
                other => panic!("expected truncation marker, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn coordinator_propagates_stream_failures_to_subscribers() {
            let backend = Arc::new(MockBackend::default());
            let coordinator = WorkerManagerBackend::new(backend.clone());
            let handle = coordinator
                .spawn(spawn_spec("wm-coordinator-failure"))
                .await
                .expect("spawn session");
            let mut subscriber = coordinator.subscribe(&handle).await.expect("subscribe");

            backend.emit_error(
                &handle.session_id,
                RuntimeError::Process("simulated stream failure".to_owned()),
            );

            let next = subscriber.next_event().await.expect("event result");
            match next {
                Some(BackendEvent::Crashed(payload)) => {
                    assert!(payload.reason.contains("simulated stream failure"));
                }
                other => panic!("expected crashed event, got {other:?}"),
            }
        }
    }
}

pub use runtime_stream_coordinator::WorkerManagerBackend;
