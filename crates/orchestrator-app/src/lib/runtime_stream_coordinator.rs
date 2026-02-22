mod runtime_stream_coordinator {
    use std::sync::Arc;

    use async_trait::async_trait;
    use orchestrator_core::{
        BackendCapabilities, BackendEvent, BackendKind, BackendNeedsInputAnswer,
        ManagedSessionStatus, RuntimeError, RuntimeResult, RuntimeSessionId,
        SessionEventSubscription, SessionHandle, SessionLifecycle, SpawnSpec, WorkerBackend,
        WorkerEventStream, WorkerEventSubscription, WorkerManager, WorkerManagerConfig,
    };

    #[derive(Clone)]
    pub struct WorkerManagerBackend {
        manager: WorkerManager,
        backend: Arc<dyn WorkerBackend + Send + Sync>,
    }

    impl WorkerManagerBackend {
        pub fn new(backend: Arc<dyn WorkerBackend + Send + Sync>) -> Self {
            let mut config = WorkerManagerConfig::default();
            // Keep existing app semantics stable; app/runtime can opt into periodic prompts later.
            config.checkpoint_prompt_interval = None;
            Self::with_config(backend, config)
        }

        pub fn with_config(
            backend: Arc<dyn WorkerBackend + Send + Sync>,
            config: WorkerManagerConfig,
        ) -> Self {
            let manager_backend: Arc<dyn WorkerBackend> = backend.clone();
            let manager = WorkerManager::with_config(manager_backend, config);
            Self { manager, backend }
        }

        async fn ensure_running_session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<()> {
            match self.manager.session_status(session_id).await {
                Ok(ManagedSessionStatus::Running) => Ok(()),
                Ok(ManagedSessionStatus::Starting) => Err(RuntimeError::Process(format!(
                    "worker session is still starting: {}",
                    session_id.as_str()
                ))),
                Ok(status) => Err(RuntimeError::Process(format!(
                    "worker session is not running: {} ({status:?})",
                    session_id.as_str()
                ))),
                Err(error) => Err(error),
            }
        }
    }

    struct WorkerManagerSessionStream {
        subscription: SessionEventSubscription,
    }

    #[async_trait]
    impl WorkerEventSubscription for WorkerManagerSessionStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            self.subscription.next_event().await
        }
    }

    #[async_trait]
    impl SessionLifecycle for WorkerManagerBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.manager.spawn(spec).await
        }

        async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
            self.manager.kill(&session.session_id).await
        }

        async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
            self.manager.send_input(&session.session_id, input).await
        }

        async fn respond_to_needs_input(
            &self,
            session: &SessionHandle,
            prompt_id: &str,
            answers: &[BackendNeedsInputAnswer],
        ) -> RuntimeResult<()> {
            self.ensure_running_session(&session.session_id).await?;
            self.backend
                .respond_to_needs_input(session, prompt_id, answers)
                .await
        }

        async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()> {
            self.manager.resize(&session.session_id, cols, rows).await
        }
    }

    #[async_trait]
    impl WorkerBackend for WorkerManagerBackend {
        fn kind(&self) -> BackendKind {
            self.manager.backend_kind()
        }

        fn capabilities(&self) -> BackendCapabilities {
            self.manager.backend_capabilities()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            self.backend.health_check().await
        }

        async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
            let subscription = self.manager.subscribe(&session.session_id).await?;
            Ok(Box::new(WorkerManagerSessionStream { subscription }))
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

        use async_trait::async_trait;
        use orchestrator_core::{BackendOutputEvent, BackendOutputStream};
        use tokio::sync::mpsc;

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

            assert_eq!(backend.sent_input(&handle.session_id), vec![b"resume".to_vec()]);
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
