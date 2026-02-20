#[cfg(test)]
#[path = "../golden_tests.rs"]
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use orchestrator_core::{
        ArtifactId, ArtifactKind, ArtifactProjection, CoreError, InboxItemProjection,
        LlmProviderKind, LlmResponseStream, LlmResponseSubscription, LlmStreamChunk,
        OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
        SessionCheckpointPayload, SessionNeedsInputPayload, SessionProjection, StoredEventEnvelope,
        SupervisorQueryFinishedPayload, TicketProvider, UserRespondedPayload, WorkItemProjection,
        WorkflowState,
    };
    use orchestrator_runtime::{
        BackendCapabilities, BackendEvent, BackendKind, BackendNeedsInputEvent,
        BackendNeedsInputOption, BackendNeedsInputQuestion, RuntimeResult, RuntimeSessionId,
        SessionHandle, SessionLifecycle, WorkerEventStream,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug)]
    struct TestLlmStream {
        chunks: VecDeque<Result<LlmStreamChunk, CoreError>>,
    }

    #[async_trait]
    impl LlmResponseSubscription for TestLlmStream {
        async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
            match self.chunks.pop_front() {
                Some(Ok(chunk)) => Ok(Some(chunk)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        }
    }

    #[derive(Debug)]
    struct TestLlmProvider {
        chunks: Mutex<Option<Vec<Result<LlmStreamChunk, CoreError>>>>,
        cancelled_streams: Mutex<Vec<String>>,
    }

    impl TestLlmProvider {
        fn new(chunks: Vec<Result<LlmStreamChunk, CoreError>>) -> Self {
            Self {
                chunks: Mutex::new(Some(chunks)),
                cancelled_streams: Mutex::new(Vec::new()),
            }
        }

        fn cancelled_streams(&self) -> Vec<String> {
            self.cancelled_streams
                .lock()
                .expect("cancelled stream lock")
                .clone()
        }
    }

    #[async_trait]
    impl LlmProvider for TestLlmProvider {
        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::Other("test".to_owned())
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn stream_chat(
            &self,
            _request: LlmChatRequest,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            let chunks = self
                .chunks
                .lock()
                .expect("stream chunk lock")
                .take()
                .unwrap_or_default();
            let stream = TestLlmStream {
                chunks: chunks.into(),
            };
            Ok(("test-stream".to_owned(), Box::new(stream)))
        }

        async fn cancel_stream(&self, stream_id: &str) -> Result<(), CoreError> {
            self.cancelled_streams
                .lock()
                .expect("cancelled stream lock")
                .push(stream_id.to_owned());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TestSupervisorDispatcher {
        requests: Mutex<Vec<(UntypedCommandInvocation, SupervisorCommandContext)>>,
        chunks: Mutex<Option<Vec<Result<LlmStreamChunk, CoreError>>>>,
        cancelled_streams: Mutex<Vec<String>>,
    }

    impl TestSupervisorDispatcher {
        fn new(chunks: Vec<Result<LlmStreamChunk, CoreError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                chunks: Mutex::new(Some(chunks)),
                cancelled_streams: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(UntypedCommandInvocation, SupervisorCommandContext)> {
            self.requests
                .lock()
                .expect("dispatcher request lock")
                .clone()
        }

        fn cancelled_streams(&self) -> Vec<String> {
            self.cancelled_streams
                .lock()
                .expect("dispatcher cancel lock")
                .clone()
        }
    }

    #[async_trait]
    impl SupervisorCommandDispatcher for TestSupervisorDispatcher {
        async fn dispatch_supervisor_command(
            &self,
            invocation: UntypedCommandInvocation,
            context: SupervisorCommandContext,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            self.requests
                .lock()
                .expect("dispatcher request lock")
                .push((invocation, context));
            let chunks = self
                .chunks
                .lock()
                .expect("dispatcher stream lock")
                .take()
                .unwrap_or_default();
            Ok((
                "dispatcher-stream".to_owned(),
                Box::new(TestLlmStream {
                    chunks: chunks.into(),
                }),
            ))
        }

        async fn cancel_supervisor_command(&self, stream_id: &str) -> Result<(), CoreError> {
            self.cancelled_streams
                .lock()
                .expect("dispatcher cancel lock")
                .push(stream_id.to_owned());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TestTicketPickerProvider {
        tickets: Vec<TicketSummary>,
        created: Option<TicketSummary>,
    }

    #[async_trait]
    impl TicketPickerProvider for TestTicketPickerProvider {
        async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
            Ok(self.tickets.clone())
        }

        async fn start_or_resume_ticket(
            &self,
            _ticket: TicketSummary,
            _repository_override: Option<PathBuf>,
        ) -> Result<SelectedTicketFlowResult, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not used in this test provider".to_owned(),
            ))
        }

        async fn create_ticket_from_brief(
            &self,
            _brief: String,
        ) -> Result<TicketSummary, CoreError> {
            self.created.clone().ok_or_else(|| {
                CoreError::DependencyUnavailable("create unavailable in test provider".to_owned())
            })
        }

        async fn archive_ticket(&self, _ticket: TicketSummary) -> Result<(), CoreError> {
            Ok(())
        }

        async fn archive_session(
            &self,
            _session_id: WorkerSessionId,
        ) -> Result<Option<String>, CoreError> {
            Ok(None)
        }

        async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
            Ok(ProjectionState::default())
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_ticket_summary(id_suffix: &str, identifier: &str, state: &str) -> TicketSummary {
        TicketSummary {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, id_suffix),
            identifier: identifier.to_owned(),
            title: format!("{identifier} sample"),
            project: Some("Core".to_owned()),
            state: state.to_owned(),
            url: format!("https://linear.app/acme/issue/{identifier}"),
            assignee: Some("Austin".to_owned()),
            priority: Some(0),
            labels: Vec::new(),
            updated_at: "2026-02-19T00:00:00Z".to_owned(),
        }
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[derive(Default, Debug)]
    struct ManualTerminalBackend {
        spawned_session_ids: Arc<Mutex<Vec<RuntimeSessionId>>>,
    }

    impl ManualTerminalBackend {
        fn spawned_session_ids(&self) -> Vec<RuntimeSessionId> {
            self.spawned_session_ids
                .lock()
                .expect("spawned session IDs lock")
                .clone()
        }
    }

    struct EmptyEventStream;

    #[async_trait]
    impl orchestrator_runtime::WorkerEventSubscription for EmptyEventStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            Ok(None)
        }
    }

    #[async_trait]
    impl SessionLifecycle for ManualTerminalBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.spawned_session_ids
                .lock()
                .expect("spawned session IDs lock")
                .push(spec.session_id.clone());
            Ok(SessionHandle {
                session_id: spec.session_id,
                backend: BackendKind::OpenCode,
            })
        }

        async fn kill(&self, _session: &SessionHandle) -> RuntimeResult<()> {
            Ok(())
        }

        async fn send_input(&self, _session: &SessionHandle, _input: &[u8]) -> RuntimeResult<()> {
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
    impl WorkerBackend for ManualTerminalBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::OpenCode
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            Ok(())
        }

        async fn subscribe(&self, _session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
            Ok(Box::new(EmptyEventStream))
        }
    }

    fn sample_projection(with_session: bool) -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_item_id = InboxItemId::new("inbox-1");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: with_session.then_some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![],
            },
        );

        if with_session {
            projection.sessions.insert(
                session_id.clone(),
                SessionProjection {
                    id: session_id,
                    work_item_id: Some(work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
        }

        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Review PR readiness".to_owned(),
                resolved: false,
            },
        );

        projection
    }

    fn review_projection_without_pr_artifact() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-review");
        let session_id = WorkerSessionId::new("sess-review");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::InReview),
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![],
                artifacts: vec![],
            },
        );
        projection.sessions.insert(
            session_id,
            SessionProjection {
                id: WorkerSessionId::new("sess-review"),
                work_item_id: Some(work_item_id),
                status: Some(WorkerSessionStatus::Running),
                latest_checkpoint: None,
            },
        );

        projection
    }

    fn triage_projection() -> ProjectionState {
        let mut projection = ProjectionState::default();
        let rows = vec![
            (
                "wi-decision",
                "inbox-decision",
                InboxItemKind::NeedsDecision,
                "Pick API shape",
            ),
            (
                "wi-approval",
                "inbox-approval",
                InboxItemKind::NeedsApproval,
                "Approve PR ready",
            ),
            (
                "wi-review",
                "inbox-review",
                InboxItemKind::ReadyForReview,
                "Review draft PR",
            ),
            ("wi-fyi", "inbox-fyi", InboxItemKind::FYI, "Progress digest"),
        ];

        for (work_item_raw, inbox_item_raw, kind, title) in rows {
            let work_item_id = WorkItemId::new(work_item_raw);
            let inbox_item_id = InboxItemId::new(inbox_item_raw);

            projection.work_items.insert(
                work_item_id.clone(),
                WorkItemProjection {
                    id: work_item_id.clone(),
                    ticket_id: None,
                    project_id: None,
                    workflow_state: Some(WorkflowState::Implementing),
                    session_id: None,
                    worktree_id: None,
                    inbox_items: vec![inbox_item_id.clone()],
                    artifacts: vec![],
                },
            );

            projection.inbox_items.insert(
                inbox_item_id.clone(),
                InboxItemProjection {
                    id: inbox_item_id,
                    work_item_id,
                    kind,
                    title: title.to_owned(),
                    resolved: false,
                },
            );
        }

        projection
    }

    fn inspector_projection() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-inspector");
        let session_id = WorkerSessionId::new("sess-inspector");
        let inbox_item_id = InboxItemId::new("inbox-inspector");
        let diff_artifact_id = ArtifactId::new("artifact-diff");
        let test_artifact_id = ArtifactId::new("artifact-test");
        let pr_artifact_id = ArtifactId::new("artifact-pr");
        let chat_artifact_id = ArtifactId::new("artifact-chat");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Testing),
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![
                    diff_artifact_id.clone(),
                    test_artifact_id.clone(),
                    pr_artifact_id.clone(),
                    chat_artifact_id.clone(),
                ],
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: Some(test_artifact_id.clone()),
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id: work_item_id.clone(),
                kind: InboxItemKind::NeedsApproval,
                title: "Inspect generated artifacts".to_owned(),
                resolved: false,
            },
        );
        projection.artifacts.insert(
            diff_artifact_id.clone(),
            ArtifactProjection {
                id: diff_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::Diff,
                label: "Feature branch delta".to_owned(),
                uri: "artifact://diff/wi-inspector?files=3&insertions=42&deletions=9".to_owned(),
            },
        );
        projection.artifacts.insert(
            test_artifact_id.clone(),
            ArtifactProjection {
                id: test_artifact_id.clone(),
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::TestRun,
                label: "cargo test -p orchestrator-ui".to_owned(),
                uri: "artifact://tests/wi-inspector?tail=thread_main_panicked%3A+line+42"
                    .to_owned(),
            },
        );
        projection.artifacts.insert(
            pr_artifact_id.clone(),
            ArtifactProjection {
                id: pr_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::PR,
                label: "Draft PR #47".to_owned(),
                uri: "https://github.com/acme/orchestrator/pull/47?draft=true".to_owned(),
            },
        );
        projection.artifacts.insert(
            chat_artifact_id.clone(),
            ArtifactProjection {
                id: chat_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::Export,
                label: "Supervisor output".to_owned(),
                uri: "artifact://chat/wi-inspector".to_owned(),
            },
        );
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-checkpoint".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-16T09:00:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionCheckpoint,
            payload: OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                session_id: session_id.clone(),
                artifact_id: test_artifact_id,
                summary: "Ran 112 tests and captured the failing tail".to_owned(),
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-blocked".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-16T09:01:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionBlocked,
            payload: OrchestrationEventPayload::SessionBlocked(SessionBlockedPayload {
                session_id: session_id.clone(),
                reason: "cargo test fails in inspector pane tests".to_owned(),
                hint: None,
                log_ref: None,
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-response".to_owned(),
            sequence: 3,
            occurred_at: "2026-02-16T09:02:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::UserResponded,
            payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(session_id),
                work_item_id: Some(work_item_id),
                message: "Please summarize the supervisor output.".to_owned(),
            }),
            schema_version: 1,
        });

        projection
    }

    fn focus_card_projection_with_evidence() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-focus");
        let session_id = WorkerSessionId::new("sess-focus");
        let inbox_item_id = InboxItemId::new("inbox-focus");
        let pr_artifact_id = ArtifactId::new("artifact-pr");
        let log_artifact_id = ArtifactId::new("artifact-log");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![pr_artifact_id.clone(), log_artifact_id.clone()],
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: Some(log_artifact_id.clone()),
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id: work_item_id.clone(),
                kind: InboxItemKind::NeedsDecision,
                title: "Choose API shape".to_owned(),
                resolved: false,
            },
        );
        projection.artifacts.insert(
            pr_artifact_id.clone(),
            ArtifactProjection {
                id: pr_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::PR,
                label: "Draft PR".to_owned(),
                uri: "https://github.com/example/repo/pull/7".to_owned(),
            },
        );
        projection.artifacts.insert(
            log_artifact_id.clone(),
            ArtifactProjection {
                id: log_artifact_id.clone(),
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::LogSnippet,
                label: "Failing test tail".to_owned(),
                uri: "artifact://logs/wi-focus".to_owned(),
            },
        );
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-checkpoint".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-16T09:00:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionCheckpoint,
            payload: OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                session_id: session_id.clone(),
                artifact_id: log_artifact_id,
                summary: "Refactored parser and ran targeted tests".to_owned(),
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-needs-input".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-16T09:01:00Z".to_owned(),
            work_item_id: Some(work_item_id),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionNeedsInput,
            payload: OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id,
                prompt: "Choose API shape: A or B".to_owned(),
                prompt_id: Some("q1".to_owned()),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("A".to_owned()),
            }),
            schema_version: 1,
        });

        projection
    }

    fn focus_card_projection_with_multiple_sessions() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-focus-multi");
        let active_session_id = WorkerSessionId::new("sess-active");
        let prior_session_id = WorkerSessionId::new("sess-prior");
        let inbox_item_id = InboxItemId::new("inbox-focus-multi");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: Some(active_session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![],
            },
        );
        projection.sessions.insert(
            active_session_id.clone(),
            SessionProjection {
                id: active_session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: None,
            },
        );
        projection.sessions.insert(
            prior_session_id.clone(),
            SessionProjection {
                id: prior_session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::Done),
                latest_checkpoint: None,
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id: work_item_id.clone(),
                kind: InboxItemKind::NeedsDecision,
                title: "Pick an implementation".to_owned(),
                resolved: false,
            },
        );
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-active-needs-input".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-16T09:00:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(active_session_id.clone()),
            event_type: OrchestrationEventType::SessionNeedsInput,
            payload: OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id: active_session_id,
                prompt: "Active session question".to_owned(),
                prompt_id: Some("q-active".to_owned()),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("A".to_owned()),
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-prior-needs-input".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-16T09:01:00Z".to_owned(),
            work_item_id: Some(work_item_id),
            session_id: Some(prior_session_id.clone()),
            event_type: OrchestrationEventType::SessionNeedsInput,
            payload: OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id: prior_session_id,
                prompt: "Prior session question".to_owned(),
                prompt_id: Some("q-prior".to_owned()),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("B".to_owned()),
            }),
            schema_version: 1,
        });

        projection
    }

    fn focus_card_projection_with_many_artifacts() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-many-artifacts");
        let inbox_item_id = InboxItemId::new("inbox-many-artifacts");

        let mut projection = ProjectionState::default();
        let mut artifact_ids = Vec::new();
        for index in 0..8 {
            let artifact_id = ArtifactId::new(format!("artifact-{index}"));
            artifact_ids.push(artifact_id.clone());
            projection.artifacts.insert(
                artifact_id.clone(),
                ArtifactProjection {
                    id: artifact_id,
                    work_item_id: work_item_id.clone(),
                    kind: ArtifactKind::LogSnippet,
                    label: format!("Log {index}"),
                    uri: format!("artifact://logs/{index}"),
                },
            );
        }

        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: artifact_ids,
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Review artifact set".to_owned(),
                resolved: false,
            },
        );

        projection
    }

    fn attach_supervisor_stream(
        shell_state: &mut UiShellState,
        work_item_id: &str,
    ) -> mpsc::Sender<SupervisorStreamEvent> {
        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        shell_state.supervisor_chat_stream = Some(ActiveSupervisorChatStream::new(
            SupervisorStreamTarget::Inspector {
                work_item_id: WorkItemId::new(work_item_id),
            },
            receiver,
        ));
        sender
    }

    fn attach_global_supervisor_stream(
        shell_state: &mut UiShellState,
    ) -> mpsc::Sender<SupervisorStreamEvent> {
        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        shell_state.supervisor_chat_stream = Some(ActiveSupervisorChatStream::new(
            SupervisorStreamTarget::GlobalChatPanel,
            receiver,
        ));
        sender
    }

    #[test]
    fn session_panel_groups_by_project_and_names_sessions_from_ticket_metadata() {
        let mut projection = ProjectionState::default();
        let work_item_core = WorkItemId::new("wi-core");
        let work_item_orchestrator = WorkItemId::new("wi-orchestrator");
        let session_core = WorkerSessionId::new("sess-core");
        let session_orchestrator = WorkerSessionId::new("sess-orchestrator");
        let ticket_core = TicketId::from_provider_uuid(TicketProvider::Linear, "ticket-core");
        let ticket_orchestrator =
            TicketId::from_provider_uuid(TicketProvider::Linear, "ticket-orchestrator");

        projection.work_items.insert(
            work_item_core.clone(),
            WorkItemProjection {
                id: work_item_core.clone(),
                ticket_id: Some(ticket_core.clone()),
                project_id: Some(ProjectId::new("Core Platform")),
                workflow_state: None,
                session_id: Some(session_core.clone()),
                worktree_id: None,
                inbox_items: vec![],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            work_item_orchestrator.clone(),
            WorkItemProjection {
                id: work_item_orchestrator.clone(),
                ticket_id: Some(ticket_orchestrator.clone()),
                project_id: Some(ProjectId::new("Orchestrator")),
                workflow_state: None,
                session_id: Some(session_orchestrator.clone()),
                worktree_id: None,
                inbox_items: vec![],
                artifacts: vec![],
            },
        );

        projection.sessions.insert(
            session_core.clone(),
            SessionProjection {
                id: session_core.clone(),
                work_item_id: Some(work_item_core.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: None,
            },
        );
        projection.sessions.insert(
            session_orchestrator.clone(),
            SessionProjection {
                id: session_orchestrator.clone(),
                work_item_id: Some(work_item_orchestrator.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: None,
            },
        );

        projection.events.push(StoredEventEnvelope {
            event_id: "evt-ticket-core".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-19T00:00:00Z".to_owned(),
            work_item_id: Some(work_item_core),
            session_id: None,
            event_type: OrchestrationEventType::TicketSynced,
            payload: OrchestrationEventPayload::TicketSynced(
                orchestrator_core::TicketSyncedPayload {
                    ticket_id: ticket_core,
                    identifier: "AP-101".to_owned(),
                    title: "Harden session lifecycle".to_owned(),
                    state: "In Progress".to_owned(),
                    assignee: None,
                    priority: None,
                },
            ),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-ticket-orchestrator".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-19T00:00:01Z".to_owned(),
            work_item_id: Some(work_item_orchestrator),
            session_id: None,
            event_type: OrchestrationEventType::TicketSynced,
            payload: OrchestrationEventPayload::TicketSynced(
                orchestrator_core::TicketSyncedPayload {
                    ticket_id: ticket_orchestrator,
                    identifier: "AP-202".to_owned(),
                    title: "Session list redesign".to_owned(),
                    state: "In Progress".to_owned(),
                    assignee: None,
                    priority: None,
                },
            ),
            schema_version: 1,
        });

        let rows = session_panel_rows(&projection, &HashMap::new());
        assert_eq!(
            rows,
            vec![
                (
                    session_core,
                    "Core Platform".to_owned(),
                    "  [waiting] AP-101: Harden session lifecycle".to_owned()
                ),
                (
                    session_orchestrator,
                    "Orchestrator".to_owned(),
                    "  [waiting] AP-202: Session list redesign".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn session_panel_uses_repository_name_when_project_name_is_missing() {
        let mut projection = ProjectionState::default();
        let work_item_id = WorkItemId::new("wi-repo-fallback");
        let session_id = WorkerSessionId::new("sess-repo-fallback");
        let ticket_id = TicketId::from_provider_uuid(TicketProvider::Linear, "ticket-repo-fallback");

        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: Some(ticket_id.clone()),
                project_id: None,
                workflow_state: None,
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![],
                artifacts: vec![],
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: None,
            },
        );

        projection.events.push(StoredEventEnvelope {
            event_id: "evt-worktree-created".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-19T00:01:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: None,
            event_type: OrchestrationEventType::WorktreeCreated,
            payload: OrchestrationEventPayload::WorktreeCreated(
                orchestrator_core::WorktreeCreatedPayload {
                    worktree_id: orchestrator_core::WorktreeId::new("wt-repo-fallback"),
                    work_item_id: work_item_id.clone(),
                    path: "/tmp/github/orchestrator".to_owned(),
                    branch: "feature/repo-fallback".to_owned(),
                    base_branch: "main".to_owned(),
                },
            ),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-ticket-synced".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-19T00:01:01Z".to_owned(),
            work_item_id: Some(work_item_id),
            session_id: None,
            event_type: OrchestrationEventType::TicketSynced,
            payload: OrchestrationEventPayload::TicketSynced(
                orchestrator_core::TicketSyncedPayload {
                    ticket_id,
                    identifier: "AP-303".to_owned(),
                    title: "Repository label fallback".to_owned(),
                    state: "In Progress".to_owned(),
                    assignee: None,
                    priority: None,
                },
            ),
            schema_version: 1,
        });

        let rows = session_panel_rows(&projection, &HashMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, session_id);
        assert_eq!(rows[0].1, "orchestrator");
        assert_eq!(rows[0].2, "  [waiting] AP-303: Repository label fallback");
    }

    #[test]
    fn session_panel_falls_back_to_session_id_when_ticket_is_missing() {
        let mut projection = ProjectionState::default();
        let work_item_id = WorkItemId::new("wi-no-ticket");
        let session_id = WorkerSessionId::new("sess-no-ticket");

        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: Some(ProjectId::new("Orchestrator")),
                workflow_state: None,
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![],
                artifacts: vec![],
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id.clone(),
                work_item_id: Some(work_item_id),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: None,
            },
        );

        let rows = session_panel_rows(&projection, &HashMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, session_id);
        assert_eq!(rows[0].1, "Orchestrator");
        assert_eq!(rows[0].2, "  [waiting] session sess-no-ticket");
    }

    #[test]
    fn center_stack_replace_push_and_pop_behavior() {
        let mut stack = ViewStack::default();
        assert_eq!(stack.active_center(), Some(&CenterView::InboxView));

        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-1"),
        });
        assert_eq!(stack.center_views().len(), 1);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::FocusCardView { .. })
        ));

        stack.push_center(CenterView::TerminalView {
            session_id: WorkerSessionId::new("sess-1"),
        });
        assert_eq!(stack.center_views().len(), 2);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));

        assert!(stack.pop_center());
        assert_eq!(stack.center_views().len(), 1);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::FocusCardView { .. })
        ));
        assert!(!stack.pop_center());
    }

    #[test]
    fn ui_state_projects_from_domain_and_center_stack() {
        let projection = sample_projection(true);
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-1"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
        assert_eq!(ui_state.selected_inbox_index, Some(0));
        assert_eq!(
            ui_state
                .selected_inbox_item_id
                .as_ref()
                .map(|id| id.as_str())
                .expect("selected inbox"),
            "inbox-1"
        );
        assert_eq!(
            ui_state.center_stack_label(),
            "FocusCard(inbox-1)".to_owned()
        );
        assert!(ui_state
            .center_pane
            .lines
            .iter()
            .any(|line| line.contains("Review PR readiness")));
    }

    #[test]
    fn focus_card_projects_action_ready_context_and_evidence() {
        let projection = focus_card_projection_with_evidence();
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-focus"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");

        assert!(rendered.contains("Why attention is required:"));
        assert!(rendered.contains("Recommended response:"));
        assert!(rendered.contains("Evidence:"));
        assert!(rendered.contains("WaitingForUser"));
        assert!(rendered.contains("Answer the worker prompt"));
        assert!(rendered.contains("Choose API shape: A or B"));
        assert!(rendered.contains("https://github.com/example/repo/pull/7"));
        assert!(rendered.contains("artifact://logs/wi-focus"));
        assert!(rendered.contains("Latest checkpoint summary"));
    }

    #[test]
    fn focus_card_prefers_active_session_context_over_prior_session_events() {
        let projection = focus_card_projection_with_multiple_sessions();
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-focus-multi"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");

        assert!(rendered.contains("Active session question"));
        assert!(!rendered.contains("Prior session question"));
    }

    #[test]
    fn focus_card_limits_artifact_evidence_to_recent_entries() {
        let projection = focus_card_projection_with_many_artifacts();
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-many-artifacts"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");
        let artifact_evidence_count = ui_state
            .center_pane
            .lines
            .iter()
            .filter(|line| line.contains("artifact '"))
            .count();

        assert_eq!(artifact_evidence_count, FOCUS_CARD_ARTIFACT_LIMIT);
        assert!(rendered.contains("artifact://logs/7"));
        assert!(!rendered.contains("artifact://logs/0"));
        assert!(rendered.contains("older artifacts not shown"));
    }

    #[test]
    fn open_terminal_with_active_session_focuses_terminal() {
        let mut with_session = UiShellState::new("ready".to_owned(), sample_projection(true));
        with_session.open_terminal_for_selected();
        assert_eq!(with_session.view_stack.center_views().len(), 2);
        assert!(matches!(
            with_session.view_stack.center_views().first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
        assert!(matches!(
            with_session.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));

        with_session.open_terminal_for_selected();
        assert_eq!(with_session.view_stack.center_views().len(), 2);
    }

    #[test]
    fn open_terminal_without_session_opens_manual_terminal_when_backend_available() {
        let backend = Arc::new(ManualTerminalBackend::default());
        let mut without_session = UiShellState::new_with_integrations(
            "ready".to_owned(),
            sample_projection(false),
            None,
            None,
            None,
            Some(backend.clone()),
        );

        without_session.open_terminal_for_selected();
        assert_eq!(without_session.view_stack.center_views().len(), 2);
        assert!(matches!(
            without_session.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));
        assert_eq!(backend.spawned_session_ids().len(), 1);
        assert!(matches!(
            without_session
                .view_stack
                .center_views()
                .first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
    }

    #[test]
    fn active_terminal_session_auto_opens_needs_input_modal_on_event() {
        let backend = Arc::new(ManualTerminalBackend::default());
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            sample_projection(true),
            None,
            None,
            None,
            Some(backend),
        );
        shell_state.open_terminal_and_enter_mode();
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::TerminalView { session_id }) if session_id.as_str() == "sess-1"
        ));
        assert!(shell_state.needs_input_modal.is_none());

        let sender = shell_state
            .terminal_session_sender
            .clone()
            .expect("terminal sender");
        sender
            .try_send(TerminalSessionEvent::NeedsInput {
                session_id: WorkerSessionId::new("sess-1"),
                needs_input: BackendNeedsInputEvent {
                    prompt_id: "prompt-plan-gate".to_owned(),
                    question: "Confirm plan".to_owned(),
                    options: vec!["A".to_owned(), "B".to_owned()],
                    default_option: Some("A".to_owned()),
                    questions: Vec::new(),
                },
            })
            .expect("queue needs-input event");

        shell_state.poll_terminal_session_events();

        let modal = shell_state
            .needs_input_modal
            .as_ref()
            .expect("needs-input modal should open for active terminal session");
        assert_eq!(modal.session_id.as_str(), "sess-1");
        assert_eq!(modal.prompt_id.as_str(), "prompt-plan-gate");
        assert!(shell_state.mode == UiMode::Terminal);
    }

    #[test]
    fn needs_input_modal_uses_jk_navigation_and_enter_selection() {
        let backend = Arc::new(ManualTerminalBackend::default());
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            sample_projection(true),
            None,
            None,
            None,
            Some(backend),
        );
        shell_state.open_terminal_and_enter_mode();

        let sender = shell_state
            .terminal_session_sender
            .clone()
            .expect("terminal sender");
        sender
            .try_send(TerminalSessionEvent::NeedsInput {
                session_id: WorkerSessionId::new("sess-1"),
                needs_input: BackendNeedsInputEvent {
                    prompt_id: "prompt-list-navigation".to_owned(),
                    question: "choose options".to_owned(),
                    options: vec![],
                    default_option: None,
                    questions: vec![
                        BackendNeedsInputQuestion {
                            id: "q1".to_owned(),
                            header: "Runtime".to_owned(),
                            question: "Pick runtime".to_owned(),
                            is_other: false,
                            is_secret: false,
                            options: Some(vec![
                                BackendNeedsInputOption {
                                    label: "Codex".to_owned(),
                                    description: String::new(),
                                },
                                BackendNeedsInputOption {
                                    label: "OpenCode".to_owned(),
                                    description: String::new(),
                                },
                            ]),
                        },
                        BackendNeedsInputQuestion {
                            id: "q2".to_owned(),
                            header: "Mode".to_owned(),
                            question: "Pick mode".to_owned(),
                            is_other: false,
                            is_secret: false,
                            options: Some(vec![
                                BackendNeedsInputOption {
                                    label: "Planning".to_owned(),
                                    description: String::new(),
                                },
                                BackendNeedsInputOption {
                                    label: "Implementation".to_owned(),
                                    description: String::new(),
                                },
                            ]),
                        },
                    ],
                },
            })
            .expect("queue needs-input event");
        shell_state.poll_terminal_session_events();

        let modal = shell_state
            .needs_input_modal
            .as_ref()
            .expect("needs-input modal should be open");
        assert_eq!(modal.current_question_index, 0);
        assert_eq!(modal.select_state.highlighted_index, 0);

        let _ = route_needs_input_modal_key(&mut shell_state, key(KeyCode::Char('j')));
        let modal = shell_state
            .needs_input_modal
            .as_ref()
            .expect("needs-input modal should remain open");
        assert_eq!(modal.select_state.highlighted_index, 1);

        let _ = route_needs_input_modal_key(&mut shell_state, key(KeyCode::Enter));
        let modal = shell_state
            .needs_input_modal
            .as_ref()
            .expect("needs-input modal should advance to next question");
        assert_eq!(modal.current_question_index, 1);
        assert_eq!(modal.answer_drafts[0].selected_option_index, Some(1));
    }

    #[test]
    fn minimize_after_open_terminal_returns_to_focus_card() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        shell_state.open_terminal_and_enter_mode();
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::TerminalView { session_id }) if session_id.as_str() == "sess-1"
        ));

        shell_state.minimize_center_view();
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert_eq!(shell_state.view_stack.center_views().len(), 1);
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
    }

    #[test]
    fn open_terminal_normalizes_stack_to_focus_and_terminal() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        shell_state
            .view_stack
            .replace_center(CenterView::FocusCardView {
                inbox_item_id: InboxItemId::new("stale-inbox"),
            });
        shell_state
            .view_stack
            .push_center(CenterView::FocusCardView {
                inbox_item_id: InboxItemId::new("inbox-1"),
            });
        assert_eq!(shell_state.view_stack.center_views().len(), 2);

        shell_state.open_terminal_for_selected();
        assert_eq!(shell_state.view_stack.center_views().len(), 2);
        assert!(matches!(
            shell_state.view_stack.center_views().first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::TerminalView { session_id }) if session_id.as_str() == "sess-1"
        ));
    }

    #[test]
    fn open_inspector_pushes_focus_and_inspector_for_selected_item() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Diff);
        assert_eq!(shell_state.view_stack.center_views().len(), 2);
        assert!(matches!(
            shell_state.view_stack.center_views().first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-inspector"
        ));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InspectorView {
                work_item_id,
                inspector: ArtifactInspectorKind::Diff
            }) if work_item_id.as_str() == "wi-inspector"
        ));

        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Diff);
        assert_eq!(shell_state.view_stack.center_views().len(), 2);
    }

    #[test]
    fn artifact_inspector_projects_diff_test_pr_and_chat_context() {
        let projection = inspector_projection();
        let work_item_id = WorkItemId::new("wi-inspector");
        let stack = |inspector| {
            let mut stack = ViewStack::default();
            stack.replace_center(CenterView::InspectorView {
                work_item_id: work_item_id.clone(),
                inspector,
            });
            stack
        };

        let diff_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::Diff),
            None,
            None,
            None,
        );
        let diff_rendered = diff_state.center_pane.lines.join("\n");
        assert!(diff_rendered.contains("Diff artifacts:"));
        assert!(diff_rendered.contains("Diffstat: 3 files changed, +42/-9"));

        let test_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::Test),
            None,
            None,
            None,
        );
        let test_rendered = test_state.center_pane.lines.join("\n");
        assert!(test_rendered.contains("Test artifacts:"));
        assert!(test_rendered.contains("Latest test tail: thread_main_panicked: line 42"));
        assert!(test_rendered.contains("Latest blocker reason:"));

        let pr_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::PullRequest),
            None,
            None,
            None,
        );
        let pr_rendered = pr_state.center_pane.lines.join("\n");
        assert!(pr_rendered.contains("PR artifacts:"));
        assert!(pr_rendered.contains("PR metadata: #47 (draft)"));
        assert!(pr_rendered.contains("github.com/acme/orchestrator/pull/47"));

        let chat_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::Chat),
            None,
            None,
            None,
        );
        let chat_rendered = chat_state.center_pane.lines.join("\n");
        assert!(chat_rendered.contains("Supervisor output:"));
        assert!(chat_rendered.contains("you: Please summarize the supervisor output."));
        assert!(chat_rendered.contains("artifact://chat/wi-inspector"));
    }

    #[test]
    fn chat_inspector_surfaces_latest_supervisor_query_metrics() {
        let mut projection = inspector_projection();
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-supervisor-finished".to_owned(),
            sequence: 4,
            occurred_at: "2026-02-16T09:03:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-inspector")),
            session_id: Some(WorkerSessionId::new("sess-inspector")),
            event_type: OrchestrationEventType::SupervisorQueryFinished,
            payload: OrchestrationEventPayload::SupervisorQueryFinished(
                SupervisorQueryFinishedPayload {
                    query_id: "supq-1".to_owned(),
                    stream_id: "stream-1".to_owned(),
                    started_at: "2026-02-16T09:02:58Z".to_owned(),
                    finished_at: "2026-02-16T09:03:00Z".to_owned(),
                    duration_ms: 2010,
                    finish_reason: LlmFinishReason::Stop,
                    chunk_count: 6,
                    output_chars: 412,
                    usage: Some(LlmTokenUsage {
                        input_tokens: 144,
                        output_tokens: 41,
                        total_tokens: 185,
                    }),
                    error: None,
                    cancellation_source: None,
                },
            ),
            schema_version: 1,
        });

        let work_item_id = WorkItemId::new("wi-inspector");
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::InspectorView {
            work_item_id,
            inspector: ArtifactInspectorKind::Chat,
        });
        let chat_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = chat_state.center_pane.lines.join("\n");

        assert!(rendered.contains("Latest query metrics: id=supq-1"));
        assert!(rendered.contains("duration=2010ms"));
        assert!(rendered.contains("usage(input=144 output=41 total=185)"));
    }

    #[test]
    fn chat_stream_coalesces_chunks_and_renders_incrementally() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Started {
                stream_id: "stream-1".to_owned(),
            })
            .expect("send stream started");
        sender
            .try_send(SupervisorStreamEvent::Delta {
                text: "Recommended response:\n".to_owned(),
            })
            .expect("send chunk one");
        sender
            .try_send(SupervisorStreamEvent::Delta {
                text: "Please rerun tests with --nocapture and post the failing assertion."
                    .to_owned(),
            })
            .expect("send chunk two");

        shell_state.poll_supervisor_stream_events();
        let buffered_view = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(buffered_view.contains("Buffering:"));
        assert!(buffered_view.contains("State: streaming"));

        shell_state.tick_supervisor_stream();
        let flushed_view = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(flushed_view.contains("Live supervisor stream:"));
        assert!(flushed_view.contains("Backpressure: coalesced 2 chunks"));
        assert!(flushed_view.contains("Recommended response:"));
        assert!(flushed_view.contains("Please rerun tests with --nocapture"));
    }

    #[test]
    fn esc_cancels_active_chat_stream() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
        }

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.lifecycle, SupervisorStreamLifecycle::Cancelling);
        assert!(stream.pending_cancel);
    }

    #[test]
    fn ctrl_c_cancels_active_chat_stream_without_mode_change() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        shell_state.enter_insert_mode();
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
        }

        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('c')));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Insert);

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.lifecycle, SupervisorStreamLifecycle::Cancelling);
    }

    #[test]
    fn esc_in_global_chat_returns_to_normal_without_cancelling_stream() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        let _sender = attach_global_supervisor_stream(&mut shell_state);
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
        }

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert!(shell_state.is_global_supervisor_chat_active());

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.lifecycle, SupervisorStreamLifecycle::Streaming);
        assert!(!stream.pending_cancel);
    }

    #[test]
    fn chat_stream_rate_limit_errors_surface_in_status_warning() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed: HTTP 429 rate limit exceeded".to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let ui_state = shell_state.ui_state();
        assert!(ui_state.status.contains("rate-limit"));
        assert!(ui_state.status.contains("warning"));
        let rendered = ui_state.center_pane.lines.join("\n");
        assert!(rendered.contains("Error: OpenRouter request failed"));
        assert!(rendered.contains("Supervisor state: rate-limited"));
        assert!(rendered.contains("Retry guidance:"));
    }

    #[test]
    fn status_text_sanitizes_control_characters_in_warnings() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.status_warning = Some("workflow merge\0 fa\u{2400}iled\r\n".to_owned());
        let status = shell_state.status_text();
        assert!(status.contains("warning: workflow merge failed"));
        assert!(!status.contains('\0'));
        assert!(!status.contains('\u{2400}'));
    }

    #[test]
    fn merge_queue_event_replaces_queued_status_with_pending_message() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        let session_id = WorkerSessionId::new("sess-1");
        let (sender, receiver) = mpsc::channel(4);
        shell_state.merge_event_receiver = Some(receiver);
        shell_state.status_warning = Some("merge queued for review session sess-1".to_owned());

        sender
            .try_send(MergeQueueEvent::Completed {
                session_id,
                kind: MergeQueueCommandKind::Merge,
                completed: false,
                merge_conflict: false,
                base_branch: None,
                head_branch: None,
                error: None,
            })
            .expect("send merge event");
        shell_state.poll_merge_queue_events();

        assert_eq!(
            shell_state.status_warning.as_deref(),
            Some("merge pending for review session sess-1 (waiting for checks or merge queue)")
        );
    }

    #[test]
    fn merge_reconcile_poll_runs_for_review_session_without_pr_artifact() {
        let projection = review_projection_without_pr_artifact();
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(Vec::new()));
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            projection,
            None,
            Some(dispatcher),
            None,
            None,
        );

        shell_state.enqueue_merge_reconcile_polls();

        assert_eq!(shell_state.merge_queue.len(), 1);
        let request = shell_state
            .merge_queue
            .front()
            .expect("merge reconcile request queued");
        assert_eq!(request.kind, MergeQueueCommandKind::Reconcile);
        assert_eq!(request.session_id.as_str(), "sess-review");
    }

    #[test]
    fn global_chat_empty_query_sets_no_context_state_with_fallback_prompts() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("empty global query should set terminal state");
        assert_eq!(stream.response_state, SupervisorResponseState::NoContext);
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: no-context"));
        assert!(rendered.contains("Safe fallback prompts:"));
    }

    #[test]
    fn rate_limit_error_with_retry_after_surfaces_cooldown_hint() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message:
                    "OpenRouter request failed with status 429: quota exhausted. Retry after 17s."
                        .to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: rate-limited"));
        assert!(rendered.contains("Cooldown: retry after 17s"));
    }

    #[test]
    fn rate_limit_error_with_reset_at_surfaces_reset_cooldown_hint() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed with status 429: quota exhausted. Retry after rate limit reset at 2026-02-17T12:00:00Z. This is recoverable; retry with a smaller context or wait for cooldown.".to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Cooldown: rate limit reset at 2026-02-17T12:00:00Z"));
        assert!(!rendered.contains("Cooldown: retry after rate"));
    }

    #[test]
    fn auth_failure_clears_prior_rate_limit_cooldown_hint() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message:
                    "OpenRouter request failed with status 429: quota exhausted. Retry after 17s."
                        .to_owned(),
            })
            .expect("send rate limit failure");
        shell_state.poll_supervisor_stream_events();

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed with status 401: unauthorized".to_owned(),
            })
            .expect("send auth failure");
        shell_state.poll_supervisor_stream_events();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: auth-unavailable"));
        assert!(!rendered.contains("Cooldown:"));
    }

    #[test]
    fn auth_failures_surface_auth_unavailable_state() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed with status 401: unauthorized".to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(
            stream.response_state,
            SupervisorResponseState::AuthUnavailable
        );
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: auth-unavailable"));
    }

    #[test]
    fn high_cost_usage_sets_high_cost_state_with_guidance() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Usage {
                usage: LlmTokenUsage {
                    input_tokens: 700,
                    output_tokens: 260,
                    total_tokens: 960,
                },
            })
            .expect("send usage");
        sender
            .try_send(SupervisorStreamEvent::Finished {
                reason: LlmFinishReason::Stop,
                usage: None,
            })
            .expect("send finished");
        shell_state.tick_supervisor_stream();

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.response_state, SupervisorResponseState::HighCost);
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: high-cost"));
        assert!(rendered.contains("Safe fallback prompts:"));
    }

    #[test]
    fn chat_stream_usage_updates_surface_in_chat_inspector() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Usage {
                usage: LlmTokenUsage {
                    input_tokens: 120,
                    output_tokens: 45,
                    total_tokens: 165,
                },
            })
            .expect("send usage");
        sender
            .try_send(SupervisorStreamEvent::Finished {
                reason: LlmFinishReason::Stop,
                usage: None,
            })
            .expect("send finished");
        shell_state.tick_supervisor_stream();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Token usage: input=120 output=45 total=165"));
    }

    #[test]
    fn global_supervisor_chat_toggle_opens_from_normal_navigation_mode() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InboxView)
        ));
        assert_eq!(shell_state.mode, UiMode::Normal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::SupervisorChatView)
        ));
        assert_eq!(shell_state.mode, UiMode::Insert);
    }

    #[tokio::test]
    async fn global_supervisor_chat_accepts_freeform_query_and_streams_response() {
        let provider = Arc::new(TestLlmProvider::new(vec![
            Ok(LlmStreamChunk {
                delta: "System summary:\n".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "No blockers right now.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: Some(LlmTokenUsage {
                    input_tokens: 21,
                    output_tokens: 8,
                    total_tokens: 29,
                }),
                rate_limit: None,
            }),
        ]));
        let provider_dyn: Arc<dyn LlmProvider> = provider;
        let mut shell_state = UiShellState::new_with_supervisor(
            "ready".to_owned(),
            triage_projection(),
            Some(provider_dyn),
        );

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        for ch in "what needs me next?".chars() {
            handle_key_press(&mut shell_state, key(KeyCode::Char(ch)));
        }
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                shell_state.tick_supervisor_stream();
                let rendered = shell_state.ui_state().center_pane.lines.join("\n");
                if rendered.contains("No blockers right now.") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("global chat stream should render response");

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Last query: what needs me next?"));
        assert!(rendered.contains("Live supervisor stream:"));
        assert!(rendered.contains("Token usage: input=21 output=8 total=29"));
    }

    #[tokio::test]
    async fn selected_chat_entry_uses_dispatcher_freeform_invocation_and_renders_stream() {
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(vec![
            Ok(LlmStreamChunk {
                delta: "Current activity: running focused tests.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "No blockers detected.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let dispatcher_dyn: Arc<dyn SupervisorCommandDispatcher> = dispatcher.clone();
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            inspector_projection(),
            None,
            Some(dispatcher_dyn),
            None,
            None,
        );

        shell_state.open_chat_inspector_for_selected();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                shell_state.tick_supervisor_stream();
                let rendered = shell_state.ui_state().center_pane.lines.join("\n");
                if rendered.contains("No blockers detected.") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("selected chat stream should render dispatcher response");

        let requests = dispatcher.requests();
        assert_eq!(requests.len(), 1);
        let (invocation, context) = &requests[0];
        let command = CommandRegistry::default()
            .parse_invocation(invocation)
            .expect("dispatcher invocation should parse");
        assert!(matches!(
            command,
            Command::SupervisorQuery(SupervisorQueryArgs::Freeform { query, .. })
                if query == "What is the current status of this ticket?"
        ));
        assert_eq!(
            context.selected_work_item_id.as_deref(),
            Some("wi-inspector")
        );
        assert_eq!(
            context.selected_session_id.as_deref(),
            Some("sess-inspector")
        );
        assert_eq!(context.scope.as_deref(), Some("session:sess-inspector"));

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Live supervisor stream:"));
        assert!(rendered.contains("Current activity: running focused tests."));
        assert!(rendered.contains("No blockers detected."));
    }

    #[tokio::test]
    async fn global_chat_panel_dispatcher_open_close_and_message_rendering() {
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(vec![
            Ok(LlmStreamChunk {
                delta: "Global status: ".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "two approvals need review.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let dispatcher_dyn: Arc<dyn SupervisorCommandDispatcher> = dispatcher.clone();
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            triage_projection(),
            None,
            Some(dispatcher_dyn),
            None,
            None,
        );

        assert!(!shell_state.is_global_supervisor_chat_active());
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(shell_state.is_global_supervisor_chat_active());
        assert_eq!(shell_state.mode, UiMode::Insert);

        for ch in "what needs me next globally?".chars() {
            handle_key_press(&mut shell_state, key(KeyCode::Char(ch)));
        }
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                shell_state.tick_supervisor_stream();
                let rendered = shell_state.ui_state().center_pane.lines.join("\n");
                if rendered.contains("two approvals need review.") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("global chat stream should render dispatcher response");

        let requests = dispatcher.requests();
        assert_eq!(requests.len(), 1);
        let (invocation, context) = &requests[0];
        let command = CommandRegistry::default()
            .parse_invocation(invocation)
            .expect("dispatcher invocation should parse");
        assert!(matches!(
            command,
            Command::SupervisorQuery(SupervisorQueryArgs::Freeform { query, .. })
                if query == "what needs me next globally?"
        ));
        assert_eq!(context.scope.as_deref(), Some("global"));
        assert!(context.selected_work_item_id.is_none());
        assert!(context.selected_session_id.is_none());

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Last query: what needs me next globally?"));
        assert!(rendered.contains("Global status: two approvals need review."));

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(!shell_state.is_global_supervisor_chat_active());
    }

    #[test]
    fn closing_global_supervisor_chat_restores_prior_context_state() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.move_selection(2);
        shell_state.open_focus_card_for_selected();
        let before_selection = shell_state.selected_inbox_item_id.clone();
        let before_stack = shell_state.view_stack.center_views().to_vec();

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::SupervisorChatView)
        ));

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));

        assert_eq!(shell_state.selected_inbox_item_id, before_selection);
        assert_eq!(
            shell_state.view_stack.center_views(),
            before_stack.as_slice()
        );
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn failed_global_supervisor_query_preserves_draft_for_retry() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        for ch in "what changed?".chars() {
            handle_key_press(&mut shell_state, key(KeyCode::Char(ch)));
        }
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        assert_eq!(shell_state.global_supervisor_chat_input.text(), "what changed?");
        assert_eq!(shell_state.global_supervisor_chat_last_query, None);
        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("failed query should surface terminal state");
        assert_eq!(
            stream.response_state,
            SupervisorResponseState::BackendUnavailable
        );
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: backend-unavailable"));
        assert!(rendered.contains("Retry guidance:"));
        assert!(shell_state
            .status_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("supervisor stream unavailable")));
    }

    #[tokio::test]
    async fn opening_new_chat_stream_cancels_active_stream_with_known_id() {
        let provider = Arc::new(TestLlmProvider::new(Vec::new()));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let mut shell_state = UiShellState::new_with_supervisor(
            "ready".to_owned(),
            inspector_projection(),
            Some(provider_dyn),
        );
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
            stream.stream_id = Some("stream-old".to_owned());
        }

        shell_state.start_supervisor_stream_for_selected();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if provider
                    .cancelled_streams()
                    .iter()
                    .any(|id| id == "stream-old")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stream cancellation should be forwarded");
    }

    #[tokio::test]
    async fn opening_new_dispatcher_chat_stream_cancels_active_stream_with_known_id() {
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(vec![Ok(LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        })]));
        let dispatcher_dyn: Arc<dyn SupervisorCommandDispatcher> = dispatcher.clone();
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            inspector_projection(),
            None,
            Some(dispatcher_dyn),
            None,
            None,
        );
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
            stream.stream_id = Some("dispatcher-stream-old".to_owned());
        }

        shell_state.start_supervisor_stream_for_selected();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if dispatcher
                    .cancelled_streams()
                    .iter()
                    .any(|id| id == "dispatcher-stream-old")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatcher stream cancellation should be forwarded");
    }

    #[tokio::test]
    async fn run_supervisor_stream_task_emits_usage_events_for_usage_only_chunks() {
        let provider = Arc::new(TestLlmProvider::new(vec![
            Ok(LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: Some(LlmTokenUsage {
                    input_tokens: 20,
                    output_tokens: 5,
                    total_tokens: 25,
                }),
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "done".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let provider_dyn: Arc<dyn LlmProvider> = provider;
        let request = LlmChatRequest {
            model: "test-model".to_owned(),
            tools: Vec::new(),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "status".to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
            temperature: None,
            tool_choice: None,
            max_output_tokens: None,
        };

        let (sender, mut receiver) = mpsc::channel(8);
        run_supervisor_stream_task(provider_dyn, request, sender).await;

        let mut saw_usage = false;
        let mut saw_finished = false;
        while let Some(event) = receiver.recv().await {
            match event {
                SupervisorStreamEvent::Usage { usage } => {
                    saw_usage |= usage.total_tokens == 25;
                }
                SupervisorStreamEvent::Finished { reason, usage } => {
                    saw_finished |= reason == LlmFinishReason::Stop && usage.is_none();
                }
                _ => {}
            }
        }

        assert!(saw_usage);
        assert!(saw_finished);
    }

    #[test]
    fn inspector_ignores_mismatched_artifact_work_item_links() {
        let mut projection = inspector_projection();
        let selected_work_item = WorkItemId::new("wi-inspector");
        let foreign_work_item = WorkItemId::new("wi-foreign");
        let foreign_artifact_id = ArtifactId::new("artifact-foreign-diff");

        projection
            .work_items
            .get_mut(&selected_work_item)
            .expect("selected work item")
            .artifacts
            .push(foreign_artifact_id.clone());
        projection.artifacts.insert(
            foreign_artifact_id.clone(),
            ArtifactProjection {
                id: foreign_artifact_id,
                work_item_id: foreign_work_item,
                kind: ArtifactKind::Diff,
                label: "Foreign diff artifact".to_owned(),
                uri: "artifact://diff/wi-foreign?files=99&insertions=1&deletions=1".to_owned(),
            },
        );

        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::InspectorView {
            work_item_id: selected_work_item,
            inspector: ArtifactInspectorKind::Diff,
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");
        assert!(!rendered.contains("Foreign diff artifact"));
        assert!(!rendered.contains("artifact://diff/wi-foreign"));
    }

    #[test]
    fn keymap_prefix_binding_opens_artifact_inspectors() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('v')));
        let overlay = shell_state
            .which_key_overlay
            .as_ref()
            .expect("overlay is shown for inspector prefix");
        let rendered = render_which_key_overlay_text(overlay);
        assert!(rendered.contains("v  (Artifact inspectors)"));
        assert!(rendered.contains("d  Open diff inspector for selected item"));
        assert!(rendered.contains("t  Open test inspector for selected item"));
        assert!(rendered.contains("p  Open PR inspector for selected item"));
        assert!(rendered.contains("c  Open chat inspector for selected item"));

        handle_key_press(&mut shell_state, key(KeyCode::Char('d')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InspectorView {
                inspector: ArtifactInspectorKind::Diff,
                ..
            })
        ));

        shell_state.minimize_center_view();
        handle_key_press(&mut shell_state, key(KeyCode::Char('v')));
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InspectorView {
                inspector: ArtifactInspectorKind::Chat,
                ..
            })
        ));
    }

    #[test]
    fn unsupported_keymap_command_id_surfaces_warning_instead_of_no_op() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        let custom_keymap = KeymapTrie::compile(
            &KeymapConfig {
                modes: vec![ModeKeymapConfig {
                    mode: UiMode::Normal,
                    bindings: vec![KeyBindingConfig {
                        keys: vec!["x".to_owned()],
                        command_id: command_ids::SUPERVISOR_QUERY.to_owned(),
                    }],
                    prefixes: Vec::new(),
                }],
            },
            |_| true,
        )
        .expect("custom keymap should compile");
        shell_state.keymap = Box::leak(Box::new(custom_keymap));

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(!should_quit);
        let status = shell_state.ui_state().status;
        assert!(status.contains("unsupported command mapping"));
        assert!(status.contains(command_ids::SUPERVISOR_QUERY));
    }

    #[test]
    fn projection_prefers_selected_inbox_item_id_over_stale_index() {
        let work_item_a = WorkItemId::new("wi-a");
        let work_item_b = WorkItemId::new("wi-b");
        let inbox_item_a = InboxItemId::new("inbox-a");
        let inbox_item_b = InboxItemId::new("inbox-b");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_a.clone(),
            WorkItemProjection {
                id: work_item_a.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_a.clone()],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            work_item_b.clone(),
            WorkItemProjection {
                id: work_item_b.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_b.clone()],
                artifacts: vec![],
            },
        );
        projection.inbox_items.insert(
            inbox_item_a.clone(),
            InboxItemProjection {
                id: inbox_item_a.clone(),
                work_item_id: work_item_a,
                kind: InboxItemKind::NeedsApproval,
                title: "First".to_owned(),
                resolved: false,
            },
        );
        projection.inbox_items.insert(
            inbox_item_b.clone(),
            InboxItemProjection {
                id: inbox_item_b.clone(),
                work_item_id: work_item_b,
                kind: InboxItemKind::NeedsApproval,
                title: "Second".to_owned(),
                resolved: false,
            },
        );

        let ui_state = project_ui_state(
            "ready",
            &projection,
            &ViewStack::default(),
            Some(0),
            Some(&inbox_item_b),
            None,
        );
        assert_eq!(ui_state.selected_inbox_index, Some(1));
        assert_eq!(
            ui_state
                .selected_inbox_item_id
                .as_ref()
                .map(|item| item.as_str()),
            Some("inbox-b")
        );
    }

    #[test]
    fn inbox_view_projects_priority_bands_and_batch_surfaces() {
        let ui_state = project_ui_state(
            "ready",
            &triage_projection(),
            &ViewStack::default(),
            None,
            None,
            None,
        );
        let ordered_kinds = ui_state
            .inbox_rows
            .iter()
            .map(|row| row.kind.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_kinds,
            vec![
                InboxItemKind::NeedsDecision,
                InboxItemKind::NeedsApproval,
                InboxItemKind::ReadyForReview,
                InboxItemKind::FYI,
            ]
        );

        assert_eq!(
            ui_state
                .inbox_rows
                .iter()
                .map(|row| row.priority_band)
                .collect::<Vec<_>>(),
            vec![
                InboxPriorityBand::Urgent,
                InboxPriorityBand::Attention,
                InboxPriorityBand::Attention,
                InboxPriorityBand::Background,
            ]
        );

        assert_eq!(ui_state.inbox_batch_surfaces.len(), 4);
        assert_eq!(ui_state.inbox_batch_surfaces[0].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[1].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[2].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[3].unresolved_count, 1);

        let rendered = render_inbox_panel(&ui_state);
        assert!(rendered.contains("Batch lanes:"));
        assert!(rendered.contains("Urgent:"));
        assert!(rendered.contains("Attention:"));
        assert!(rendered.contains("Background:"));
    }

    #[test]
    fn batch_navigation_can_jump_and_cycle_surfaces() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());

        shell_state.jump_to_batch(InboxBatchKind::ReviewReady);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);

        shell_state.cycle_batch(1);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        shell_state.cycle_batch(-1);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);
    }

    #[test]
    fn keyboard_shortcuts_support_batch_and_range_navigation() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        handle_key_press(&mut shell_state, key(KeyCode::Char('g')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, key(KeyCode::Char('G')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        handle_key_press(&mut shell_state, key(KeyCode::BackTab));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);
    }

    #[test]
    fn keymap_prefix_binding_aliases_batch_jumps() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);
    }

    #[test]
    fn which_key_overlay_shows_next_keys_with_descriptions() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        assert!(shell_state.which_key_overlay.is_none());
        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));

        let overlay = shell_state
            .which_key_overlay
            .as_ref()
            .expect("overlay is shown for valid prefix");
        let rendered = render_which_key_overlay_text(overlay);
        assert!(rendered.contains("z  (Batch jumps)"));
        assert!(rendered.contains("1  Jump to Decide/Unblock lane"));
        assert!(rendered.contains("2  Jump to Approvals lane"));
        assert!(rendered.contains("3  Jump to PR Reviews lane"));
        assert!(rendered.contains("4  Jump to FYI Digest lane"));
    }

    #[test]
    fn which_key_overlay_clears_on_completion_invalid_and_cancel() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        assert!(shell_state.which_key_overlay.is_some());

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        assert!(shell_state.which_key_overlay.is_none());

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        assert!(shell_state.which_key_overlay.is_some());
        handle_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(shell_state.which_key_overlay.is_none());
        assert!(shell_state.mode_key_buffer.is_empty());

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        assert!(shell_state.which_key_overlay.is_some());
        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(shell_state.which_key_overlay.is_none());
    }

    #[test]
    fn which_key_overlay_popup_is_anchored_and_clamped() {
        let anchor = Rect {
            x: 10,
            y: 5,
            width: 24,
            height: 8,
        };
        let content = "z  (Batch jumps)\n1  Jump to Decide/Unblock lane\n2  Jump to Approvals lane";
        let popup = which_key_overlay_popup(anchor, content).expect("overlay popup");

        assert!(popup.width <= anchor.width);
        assert!(popup.height <= anchor.height);
        assert_eq!(popup.x + popup.width, anchor.x + anchor.width);
        assert_eq!(popup.y + popup.height, anchor.y + anchor.height);

        let too_narrow = Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 8,
        };
        assert!(which_key_overlay_popup(too_narrow, content).is_none());

        let too_short = Rect {
            x: 0,
            y: 0,
            width: 24,
            height: 2,
        };
        assert!(which_key_overlay_popup(too_short, content).is_none());
    }

    #[test]
    fn which_key_overlay_popup_handles_large_content_without_overflow() {
        let anchor = Rect {
            x: 0,
            y: 0,
            width: 32,
            height: 12,
        };

        let very_wide = "x".repeat(70_000);
        let popup = which_key_overlay_popup(anchor, very_wide.as_str()).expect("wide popup");
        assert_eq!(popup.width, anchor.width);
        assert_eq!(popup.height, 3);

        let very_tall = "xx\n".repeat(70_000);
        let popup = which_key_overlay_popup(anchor, very_tall.as_str()).expect("tall popup");
        assert_eq!(popup.width, 4);
        assert_eq!(popup.height, anchor.height);
    }

    #[test]
    fn keyboard_shortcuts_ignore_control_modified_chars() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('q')));
        assert!(!should_quit);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('j')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);
    }

    #[test]
    fn ticket_picker_overlay_help_includes_new_ticket_shortcut() {
        let overlay = TicketPickerOverlayState::default();
        let rendered = render_ticket_picker_overlay_text(&overlay);
        assert!(rendered.contains("n: new ticket"));
        assert!(rendered.contains("x: archive"));
    }

    #[test]
    fn ticket_picker_n_enters_new_ticket_mode() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();

        let routed = route_ticket_picker_key(&mut shell_state, key(KeyCode::Char('n')));
        assert!(matches!(routed, RoutedInput::Ignore));
        assert!(shell_state.ticket_picker_overlay.new_ticket_mode);
    }

    #[test]
    fn ticket_picker_new_ticket_mode_overlay_text_does_not_duplicate_brief_label() {
        let mut overlay = TicketPickerOverlayState::default();
        overlay.begin_new_ticket_mode();
        overlay.new_ticket_brief_input.set_text("draft");

        let rendered = render_ticket_picker_overlay_text(&overlay);
        assert!(!rendered.contains("Brief:"));
        assert!(rendered.contains("Enter: create"));
    }

    #[test]
    fn ticket_picker_new_ticket_mode_captures_input_and_submit_validation() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();
        shell_state.ticket_picker_overlay.begin_new_ticket_mode();

        route_ticket_picker_key(&mut shell_state, key(KeyCode::Char('b')));
        route_ticket_picker_key(&mut shell_state, key(KeyCode::Char('r')));
        route_ticket_picker_key(&mut shell_state, key(KeyCode::Backspace));
        assert_eq!(shell_state.ticket_picker_overlay.new_ticket_brief_input.text(), "b");

        shell_state.ticket_picker_overlay.new_ticket_brief_input.clear();
        route_ticket_picker_key(&mut shell_state, key(KeyCode::Enter));
        assert!(shell_state
            .ticket_picker_overlay
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("enter a brief description"));
    }

    #[test]
    fn ticket_picker_esc_cancels_new_ticket_mode_without_closing_overlay() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();
        shell_state.ticket_picker_overlay.begin_new_ticket_mode();
        shell_state
            .ticket_picker_overlay
            .new_ticket_brief_input
            .set_text("draft");

        route_ticket_picker_key(&mut shell_state, key(KeyCode::Esc));
        assert!(shell_state.ticket_picker_overlay.visible);
        assert!(!shell_state.ticket_picker_overlay.new_ticket_mode);
        assert!(shell_state.ticket_picker_overlay.new_ticket_brief_input.is_empty());
    }

    #[test]
    fn ticket_picker_x_enters_archive_confirm_mode() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();
        shell_state.ticket_picker_overlay.loading = false;
        shell_state.ticket_picker_overlay.apply_tickets(
            vec![sample_ticket_summary("issue-300", "AP-300", "Todo")],
            Vec::new(),
            &["Todo".to_owned()],
        );
        shell_state.ticket_picker_overlay.move_selection(1);

        let routed = route_ticket_picker_key(&mut shell_state, key(KeyCode::Char('x')));
        assert!(matches!(routed, RoutedInput::Ignore));
        assert!(shell_state.ticket_picker_overlay.archive_confirm_ticket.is_some());
    }

    #[test]
    fn ticket_picker_archive_confirm_esc_cancels_without_closing_overlay() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();
        shell_state.ticket_picker_overlay.archive_confirm_ticket =
            Some(sample_ticket_summary("issue-301", "AP-301", "Todo"));

        route_ticket_picker_key(&mut shell_state, key(KeyCode::Esc));
        assert!(shell_state.ticket_picker_overlay.visible);
        assert!(shell_state.ticket_picker_overlay.archive_confirm_ticket.is_none());
    }

    #[tokio::test]
    async fn run_ticket_picker_create_task_emits_created_event() {
        let created = sample_ticket_summary("issue-200", "AP-200", "Todo");
        let refreshed = vec![sample_ticket_summary("issue-201", "AP-201", "Todo")];
        let provider = Arc::new(TestTicketPickerProvider {
            tickets: refreshed.clone(),
            created: Some(created.clone()),
        });
        let (sender, mut receiver) = mpsc::channel(1);

        run_ticket_picker_create_task(provider, "brief".to_owned(), sender).await;

        let event = receiver.recv().await.expect("ticket picker event");
        match event {
            TicketPickerEvent::TicketCreated {
                created_ticket,
                projection,
                tickets,
                warning,
            } => {
                assert_eq!(created_ticket.identifier, created.identifier);
                assert!(projection.is_some());
                assert_eq!(tickets.unwrap_or_default(), refreshed);
                assert!(warning.is_none());
            }
            _ => panic!("expected ticket created event"),
        }
    }

    #[test]
    fn ticket_created_event_focuses_created_ticket_and_updates_status() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();
        shell_state.ticket_picker_overlay.loading = false;
        shell_state.ticket_picker_overlay.apply_tickets(
            vec![
                sample_ticket_summary("issue-401", "AP-401", "Todo"),
                sample_ticket_summary("issue-402", "AP-402", "Todo"),
            ],
            Vec::new(),
            &["Todo".to_owned()],
        );
        shell_state.ticket_picker_overlay.move_selection(1);

        let created = sample_ticket_summary("issue-499", "AP-499", "Todo");
        shell_state.apply_ticket_picker_event(TicketPickerEvent::TicketCreated {
            created_ticket: created.clone(),
            projection: None,
            tickets: Some(vec![
                sample_ticket_summary("issue-401", "AP-401", "Todo"),
                created,
            ]),
            warning: None,
        });

        let selected = shell_state
            .ticket_picker_overlay
            .selected_ticket()
            .expect("selected ticket after create");
        assert_eq!(selected.identifier, "AP-499");
        assert_eq!(
            shell_state.status_warning.as_deref(),
            Some("created AP-499")
        );
    }

    #[test]
    fn ticket_created_event_inserts_created_ticket_when_refresh_misses_it() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.ticket_picker_overlay.open();
        shell_state.ticket_picker_overlay.loading = false;
        shell_state.ticket_picker_overlay.apply_tickets(
            vec![sample_ticket_summary("issue-501", "AP-501", "Todo")],
            Vec::new(),
            &["Todo".to_owned()],
        );

        let created = sample_ticket_summary("issue-599", "AP-599", "Todo");
        shell_state.apply_ticket_picker_event(TicketPickerEvent::TicketCreated {
            created_ticket: created,
            projection: None,
            tickets: Some(vec![sample_ticket_summary("issue-501", "AP-501", "Todo")]),
            warning: None,
        });

        assert!(shell_state
            .ticket_picker_overlay
            .tickets_snapshot()
            .iter()
            .any(|ticket| ticket.identifier == "AP-599"));
    }

    #[tokio::test]
    async fn run_ticket_picker_archive_task_emits_archived_event() {
        let archived = sample_ticket_summary("issue-202", "AP-202", "Todo");
        let refreshed = vec![sample_ticket_summary("issue-203", "AP-203", "Todo")];
        let provider = Arc::new(TestTicketPickerProvider {
            tickets: refreshed.clone(),
            created: None,
        });
        let (sender, mut receiver) = mpsc::channel(1);

        run_ticket_picker_archive_task(provider, archived.clone(), sender).await;

        let event = receiver.recv().await.expect("ticket picker event");
        match event {
            TicketPickerEvent::TicketArchived {
                archived_ticket,
                tickets,
                warning,
            } => {
                assert_eq!(archived_ticket.identifier, archived.identifier);
                assert_eq!(tickets.unwrap_or_default(), refreshed);
                assert!(warning.is_none());
            }
            _ => panic!("expected ticket archived event"),
        }
    }

    #[tokio::test]
    async fn run_session_merge_finalize_task_emits_finalized_event() {
        let provider = Arc::new(TestTicketPickerProvider {
            tickets: Vec::new(),
            created: None,
        });
        let session_id = WorkerSessionId::new("sess-merge-finalized");
        let (sender, mut receiver) = mpsc::channel(1);

        run_session_merge_finalize_task(provider, session_id.clone(), sender).await;

        let event = receiver.recv().await.expect("merge queue event");
        match event {
            MergeQueueEvent::SessionFinalized {
                session_id: event_session_id,
            } => assert_eq!(event_session_id, session_id),
            _ => panic!("expected merge finalize success event"),
        }
    }

    #[tokio::test]
    async fn run_session_merge_finalize_task_emits_failure_event() {
        struct FailingMergeFinalizeProvider;

        #[async_trait]
        impl TicketPickerProvider for FailingMergeFinalizeProvider {
            async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
                Ok(Vec::new())
            }

            async fn start_or_resume_ticket(
                &self,
                _ticket: TicketSummary,
                _repository_override: Option<PathBuf>,
            ) -> Result<SelectedTicketFlowResult, CoreError> {
                Err(CoreError::DependencyUnavailable("not used".to_owned()))
            }

            async fn create_ticket_from_brief(
                &self,
                _brief: String,
            ) -> Result<TicketSummary, CoreError> {
                Err(CoreError::DependencyUnavailable("not used".to_owned()))
            }

            async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
                Ok(ProjectionState::default())
            }

            async fn complete_session_after_merge(
                &self,
                _session_id: WorkerSessionId,
            ) -> Result<(), CoreError> {
                Err(CoreError::DependencyUnavailable(
                    "merge finalize failed in test".to_owned(),
                ))
            }
        }

        let provider = Arc::new(FailingMergeFinalizeProvider);
        let session_id = WorkerSessionId::new("sess-merge-fail");
        let (sender, mut receiver) = mpsc::channel(1);

        run_session_merge_finalize_task(provider, session_id.clone(), sender).await;

        let event = receiver.recv().await.expect("merge queue event");
        match event {
            MergeQueueEvent::SessionFinalizeFailed {
                session_id: event_session_id,
                message,
            } => {
                assert_eq!(event_session_id, session_id);
                assert!(message.contains("merge finalize failed in test"));
            }
            _ => panic!("expected merge finalize failure event"),
        }
    }

    #[tokio::test]
    async fn run_session_archive_task_emits_archived_event() {
        let provider = Arc::new(TestTicketPickerProvider {
            tickets: Vec::new(),
            created: None,
        });
        let session_id = WorkerSessionId::new("sess-archive-ok");
        let (sender, mut receiver) = mpsc::channel(1);

        run_session_archive_task(provider, session_id.clone(), sender).await;

        let event = receiver.recv().await.expect("ticket picker event");
        match event {
            TicketPickerEvent::SessionArchived {
                session_id: event_session_id,
                warning,
            } => {
                assert_eq!(event_session_id, session_id);
                assert!(warning.is_none());
            }
            _ => panic!("expected session archived event"),
        }
    }

    #[tokio::test]
    async fn run_session_archive_task_emits_failed_event() {
        struct FailingArchiveProvider;

        #[async_trait]
        impl TicketPickerProvider for FailingArchiveProvider {
            async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
                Ok(Vec::new())
            }

            async fn start_or_resume_ticket(
                &self,
                _ticket: TicketSummary,
                _repository_override: Option<PathBuf>,
            ) -> Result<SelectedTicketFlowResult, CoreError> {
                Err(CoreError::DependencyUnavailable("not used".to_owned()))
            }

            async fn create_ticket_from_brief(
                &self,
                _brief: String,
            ) -> Result<TicketSummary, CoreError> {
                Err(CoreError::DependencyUnavailable("not used".to_owned()))
            }

            async fn archive_session(
                &self,
                _session_id: WorkerSessionId,
            ) -> Result<Option<String>, CoreError> {
                Err(CoreError::DependencyUnavailable(
                    "session archive failed in test".to_owned(),
                ))
            }

            async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
                Ok(ProjectionState::default())
            }
        }

        let provider = Arc::new(FailingArchiveProvider);
        let session_id = WorkerSessionId::new("sess-archive-fail");
        let (sender, mut receiver) = mpsc::channel(1);

        run_session_archive_task(provider, session_id.clone(), sender).await;

        let event = receiver.recv().await.expect("ticket picker event");
        match event {
            TicketPickerEvent::SessionArchiveFailed {
                session_id: event_session_id,
                message,
            } => {
                assert_eq!(event_session_id, session_id);
                assert!(message.contains("session archive failed in test"));
            }
            _ => panic!("expected session archive failed event"),
        }
    }

    #[test]
    fn batch_jump_prefers_unresolved_then_falls_back_to_first_any() {
        let mut projection = ProjectionState::default();
        let resolved_approval = InboxItemId::new("inbox-approval-resolved");
        let unresolved_approval = InboxItemId::new("inbox-approval-unresolved");
        let resolved_work_item_id = WorkItemId::new("wi-approval-resolved");
        let unresolved_work_item_id = WorkItemId::new("wi-approval-unresolved");

        projection.work_items.insert(
            resolved_work_item_id.clone(),
            WorkItemProjection {
                id: resolved_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![resolved_approval.clone()],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            unresolved_work_item_id.clone(),
            WorkItemProjection {
                id: unresolved_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![unresolved_approval.clone()],
                artifacts: vec![],
            },
        );
        projection.inbox_items.insert(
            resolved_approval.clone(),
            InboxItemProjection {
                id: resolved_approval,
                work_item_id: resolved_work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Resolved approval".to_owned(),
                resolved: true,
            },
        );
        projection.inbox_items.insert(
            unresolved_approval.clone(),
            InboxItemProjection {
                id: unresolved_approval.clone(),
                work_item_id: unresolved_work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Unresolved approval".to_owned(),
                resolved: false,
            },
        );

        let mut shell_state = UiShellState::new("ready".to_owned(), projection.clone());
        shell_state.jump_to_batch(InboxBatchKind::Approvals);
        let selected = shell_state.ui_state();
        assert_eq!(
            selected
                .selected_inbox_item_id
                .as_ref()
                .map(|id| id.as_str())
                .expect("selected approval"),
            unresolved_approval.as_str()
        );

        projection
            .inbox_items
            .get_mut(&unresolved_approval)
            .expect("unresolved approval exists")
            .resolved = true;
        let mut shell_state = UiShellState::new("ready".to_owned(), projection);
        shell_state.jump_to_batch(InboxBatchKind::Approvals);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsApproval);
        let selected_title = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .title
            .clone();
        assert!(selected_title.contains("approval"));
    }

    #[test]
    fn set_selection_ignores_out_of_bounds_index() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let rows = shell_state.ui_state().inbox_rows;
        shell_state.set_selection(Some(usize::MAX), &rows);
        assert_eq!(shell_state.selected_inbox_index, None);
        assert_eq!(shell_state.selected_inbox_item_id, None);
    }

    #[test]
    fn mode_commands_have_stable_ids() {
        assert_eq!(command_id(UiCommand::EnterNormalMode), "ui.mode.normal");
        assert_eq!(command_id(UiCommand::EnterInsertMode), "ui.mode.insert");
        assert_eq!(
            command_id(UiCommand::ToggleGlobalSupervisorChat),
            "ui.supervisor_chat.toggle"
        );
        assert_eq!(
            command_id(UiCommand::OpenTerminalForSelected),
            command_ids::UI_OPEN_TERMINAL_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenDiffInspectorForSelected),
            command_ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenTestInspectorForSelected),
            command_ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenPrInspectorForSelected),
            command_ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenChatInspectorForSelected),
            command_ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::FocusNextInbox),
            command_ids::UI_FOCUS_NEXT_INBOX
        );
        assert_eq!(
            command_id(UiCommand::AdvanceTerminalWorkflowStage),
            "ui.terminal.workflow.advance"
        );
        assert_eq!(
            command_id(UiCommand::ArchiveSelectedSession),
            "ui.terminal.archive_selected_session"
        );
    }

    #[test]
    fn command_registry_round_trips_ids() {
        let all_commands = [
            UiCommand::EnterNormalMode,
            UiCommand::EnterInsertMode,
            UiCommand::ToggleGlobalSupervisorChat,
            UiCommand::OpenTerminalForSelected,
            UiCommand::OpenDiffInspectorForSelected,
            UiCommand::OpenTestInspectorForSelected,
            UiCommand::OpenPrInspectorForSelected,
            UiCommand::OpenChatInspectorForSelected,
            UiCommand::StartTerminalEscapeChord,
            UiCommand::QuitShell,
            UiCommand::FocusNextInbox,
            UiCommand::FocusPreviousInbox,
            UiCommand::CycleBatchNext,
            UiCommand::CycleBatchPrevious,
            UiCommand::JumpFirstInbox,
            UiCommand::JumpLastInbox,
            UiCommand::JumpBatchDecideOrUnblock,
            UiCommand::JumpBatchApprovals,
            UiCommand::JumpBatchReviewReady,
            UiCommand::JumpBatchFyiDigest,
            UiCommand::OpenFocusCard,
            UiCommand::AdvanceTerminalWorkflowStage,
            UiCommand::ArchiveSelectedSession,
            UiCommand::MinimizeCenterView,
        ];

        for command in all_commands {
            let id = command.id();
            assert_eq!(UiCommand::from_id(id), Some(command));
        }
    }

    #[test]
    fn esc_returns_to_normal_without_quitting() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.enter_insert_mode();
        assert_eq!(shell_state.mode, UiMode::Insert);

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn insert_mode_routes_navigation_keys_to_ignore() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.move_selection(2);
        let before_index = shell_state.ui_state().selected_inbox_index;

        shell_state.enter_insert_mode();
        assert_eq!(shell_state.mode, UiMode::Insert);

        handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        handle_key_press(&mut shell_state, key(KeyCode::Down));
        let after_index = shell_state.ui_state().selected_inbox_index;
        assert_eq!(before_index, after_index);
    }

    #[test]
    fn insert_mode_is_not_entered_while_terminal_view_is_active() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(shell_state.is_terminal_view_active());

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert!(shell_state.is_terminal_view_active());

        shell_state.enter_insert_mode();
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn ctrl_left_bracket_returns_to_normal_mode() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.enter_insert_mode();
        assert_eq!(shell_state.mode, UiMode::Insert);

        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('[')));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn terminal_mode_supports_escape_chord_with_compose_buffer() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(shell_state.is_terminal_view_active());

        let routed = route_key_press(&mut shell_state, key(KeyCode::Char('j')));
        assert!(matches!(routed, RoutedInput::Ignore));
        assert_eq!(shell_state.terminal_compose_input.text(), "j");

        let start_chord = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('\\')));
        assert!(!start_chord);
        assert!(shell_state.terminal_escape_pending);
        assert_eq!(shell_state.mode, UiMode::Terminal);

        let finish_chord = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('n')));
        assert!(!finish_chord);
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert!(!shell_state.terminal_escape_pending);
    }

    #[test]
    fn terminal_mode_without_terminal_view_recovers_to_normal() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.mode = UiMode::Terminal;
        assert!(!shell_state.is_terminal_view_active());

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsApproval);
    }

    #[test]
    fn terminal_escape_prefix_replays_when_chord_not_completed() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);

        handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('\\')));
        let routed = route_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(matches!(routed, RoutedInput::Ignore));
        assert!(!shell_state.terminal_escape_pending);
        assert!(shell_state.terminal_compose_input.is_empty());
    }

    #[test]
    fn x_opens_session_archive_confirmation() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(!should_quit);
        assert!(shell_state.archive_session_confirm_session.is_some());
    }

    #[test]
    fn session_archive_confirm_esc_cancels() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        let selected = shell_state
            .selected_session_id_for_terminal_action()
            .expect("selected session");
        shell_state.archive_session_confirm_session = Some(selected);

        let routed = route_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(matches!(routed, RoutedInput::Ignore));
        assert!(shell_state.archive_session_confirm_session.is_none());
    }

    #[test]
    fn terminal_compose_supports_multiline_input() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('h')));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        handle_key_press(&mut shell_state, shift_key(KeyCode::Enter));
        handle_key_press(&mut shell_state, key(KeyCode::Char('!')));

        assert_eq!(shell_state.terminal_compose_input.text(), "hi\n!");
    }

    #[test]
    fn entering_terminal_mode_snaps_stream_view_to_bottom() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        let session_id = shell_state
            .active_terminal_session_id()
            .expect("active terminal session")
            .clone();
        shell_state
            .terminal_session_states
            .entry(session_id.clone())
            .or_default();

        {
            let view = shell_state
                .terminal_session_states
                .get_mut(&session_id)
                .expect("terminal view state");
            view.entries = vec![
                TerminalTranscriptEntry::Message("line 1".to_owned()),
                TerminalTranscriptEntry::Message("line 2".to_owned()),
                TerminalTranscriptEntry::Message("line 3".to_owned()),
                TerminalTranscriptEntry::Message("line 4".to_owned()),
            ];
            view.output_viewport_rows = 2;
            view.output_scroll_line = 0;
            view.output_follow_tail = false;
        }

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);

        let view = shell_state
            .terminal_session_states
            .get_mut(&session_id)
            .expect("terminal view state");
        let rendered_line_count = render_terminal_transcript_entries(view).len();
        assert_eq!(
            view.output_scroll_line,
            rendered_line_count.saturating_sub(view.output_viewport_rows)
        );
        assert!(view.output_follow_tail);
    }

    #[test]
    fn terminal_stream_normal_mode_scrolls_with_shift_jk_and_g() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        let session_id = shell_state
            .active_terminal_session_id()
            .expect("active terminal session")
            .clone();
        shell_state
            .terminal_session_states
            .entry(session_id.clone())
            .or_default();
        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);

        {
            let view = shell_state
                .terminal_session_states
                .get_mut(&session_id)
                .expect("terminal view state");
            view.entries = vec![
                TerminalTranscriptEntry::Message("line 1".to_owned()),
                TerminalTranscriptEntry::Message("line 2".to_owned()),
                TerminalTranscriptEntry::Message("line 3".to_owned()),
                TerminalTranscriptEntry::Message("line 4".to_owned()),
                TerminalTranscriptEntry::Message("line 5".to_owned()),
            ];
            view.output_viewport_rows = 2;
            view.output_scroll_line = 0;
            view.output_follow_tail = false;
        }

        handle_key_press(&mut shell_state, shift_key(KeyCode::Char('J')));
        let view = shell_state
            .terminal_session_states
            .get_mut(&session_id)
            .expect("terminal view state");
        assert_eq!(view.output_scroll_line, 1);

        handle_key_press(&mut shell_state, shift_key(KeyCode::Char('K')));
        let view = shell_state
            .terminal_session_states
            .get_mut(&session_id)
            .expect("terminal view state");
        assert_eq!(view.output_scroll_line, 0);

        handle_key_press(&mut shell_state, key(KeyCode::Char('G')));
        let view = shell_state
            .terminal_session_states
            .get_mut(&session_id)
            .expect("terminal view state");
        let rendered_line_count = render_terminal_transcript_entries(view).len();
        assert_eq!(
            view.output_scroll_line,
            rendered_line_count.saturating_sub(view.output_viewport_rows)
        );
        assert!(view.output_follow_tail);
    }

    #[test]
    fn terminal_stream_scroll_uses_rendered_line_count_without_initial_jump() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        let session_id = shell_state
            .active_terminal_session_id()
            .expect("active terminal session")
            .clone();
        shell_state
            .terminal_session_states
            .entry(session_id.clone())
            .or_default();
        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);

        {
            let view = shell_state
                .terminal_session_states
                .get_mut(&session_id)
                .expect("terminal view state");
            view.entries = vec![
                TerminalTranscriptEntry::Message("line 1".to_owned()),
                TerminalTranscriptEntry::Message("line 2".to_owned()),
                TerminalTranscriptEntry::Message("line 3".to_owned()),
                TerminalTranscriptEntry::Message("line 4".to_owned()),
            ];
            view.output_follow_tail = true;
        }

        shell_state.sync_terminal_output_viewport(120, 20);
        let view = shell_state
            .terminal_session_states
            .get(&session_id)
            .expect("terminal view state");
        assert_eq!(view.output_scroll_line, 100);

        handle_key_press(&mut shell_state, shift_key(KeyCode::Char('K')));
        let view = shell_state
            .terminal_session_states
            .get(&session_id)
            .expect("terminal view state");
        assert_eq!(view.output_scroll_line, 99);
        assert!(!view.output_follow_tail);
    }

    #[test]
    fn terminal_transcript_adds_padding_around_user_messages() {
        let state = TerminalViewState {
            entries: vec![
                TerminalTranscriptEntry::Message("worker: planning update".to_owned()),
                TerminalTranscriptEntry::Message("> ship it".to_owned()),
                TerminalTranscriptEntry::Message("worker: applied patch".to_owned()),
                TerminalTranscriptEntry::Message("> system: workflow transition".to_owned()),
            ],
            ..TerminalViewState::default()
        };

        let rendered = render_terminal_transcript_entries(&state)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec![
                "worker: planning update".to_owned(),
                String::new(),
                "> ship it".to_owned(),
                String::new(),
                "worker: applied patch".to_owned(),
                "> system: workflow transition".to_owned(),
            ]
        );
    }

    #[test]
    fn normal_mode_router_maps_expected_commands() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('j')))),
            Some(UiCommand::FocusNextInbox)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('i')))),
            Some(UiCommand::OpenTerminalForSelected)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('q')))),
            Some(UiCommand::QuitShell)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('c')))),
            Some(UiCommand::ToggleGlobalSupervisorChat)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('v')))),
            None
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('d')))),
            Some(UiCommand::OpenDiffInspectorForSelected)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Esc))),
            Some(UiCommand::EnterNormalMode)
        );
    }
}
