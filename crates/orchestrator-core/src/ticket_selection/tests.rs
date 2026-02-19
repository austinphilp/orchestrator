#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::{BackendCapabilities, BackendKind, RuntimeResult, WorktreeStatus};

    struct StubVcsProvider {
        discovered: Vec<RepositoryRef>,
        created: Mutex<Vec<CreateWorktreeRequest>>,
        created_worktree_path_override: Option<PathBuf>,
        deleted: Mutex<Vec<crate::DeleteWorktreeRequest>>,
    }

    impl StubVcsProvider {
        fn with_single_repository(root: &str) -> Self {
            Self {
                discovered: vec![RepositoryRef {
                    id: root.to_owned(),
                    name: "repo".to_owned(),
                    root: PathBuf::from(root),
                }],
                created: Mutex::new(Vec::new()),
                created_worktree_path_override: None,
                deleted: Mutex::new(Vec::new()),
            }
        }

        fn with_created_worktree_path(mut self, path: impl Into<PathBuf>) -> Self {
            self.created_worktree_path_override = Some(path.into());
            self
        }
    }

    #[async_trait]
    impl VcsProvider for StubVcsProvider {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn discover_repositories(
            &self,
            _roots: &[PathBuf],
        ) -> Result<Vec<RepositoryRef>, CoreError> {
            Ok(self.discovered.clone())
        }

        async fn create_worktree(
            &self,
            request: CreateWorktreeRequest,
        ) -> Result<crate::WorktreeSummary, CoreError> {
            self.created.lock().expect("lock").push(request.clone());
            let created_path = self
                .created_worktree_path_override
                .clone()
                .unwrap_or_else(|| request.worktree_path.clone());
            Ok(crate::WorktreeSummary {
                worktree_id: request.worktree_id,
                repository: request.repository,
                path: created_path,
                branch: request.branch,
                base_branch: request.base_branch,
            })
        }

        async fn delete_worktree(
            &self,
            request: crate::DeleteWorktreeRequest,
        ) -> Result<(), CoreError> {
            self.deleted.lock().expect("lock").push(request);
            Ok(())
        }

        async fn worktree_status(
            &self,
            _worktree_path: &Path,
        ) -> Result<WorktreeStatus, CoreError> {
            Ok(WorktreeStatus {
                is_dirty: false,
                commits_ahead: 0,
                commits_behind: 0,
            })
        }
    }

    struct EmptyStream;

    #[async_trait]
    impl crate::WorkerEventSubscription for EmptyStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<crate::BackendEvent>> {
            Ok(None)
        }
    }

    struct StubWorkerBackend {
        kind: BackendKind,
        spawn_specs: Mutex<Vec<SpawnSpec>>,
        spawn_results: Mutex<VecDeque<RuntimeResult<SessionHandle>>>,
        send_inputs: Mutex<Vec<(SessionHandle, Vec<u8>)>>,
        send_input_results: Mutex<VecDeque<Result<(), RuntimeError>>>,
        harness_session_ids: Mutex<VecDeque<RuntimeResult<Option<String>>>>,
    }

    impl StubWorkerBackend {
        fn new(kind: BackendKind) -> Self {
            Self {
                kind,
                spawn_specs: Mutex::new(Vec::new()),
                spawn_results: Mutex::new(VecDeque::new()),
                send_inputs: Mutex::new(Vec::new()),
                send_input_results: Mutex::new(VecDeque::new()),
                harness_session_ids: Mutex::new(VecDeque::new()),
            }
        }

        fn push_spawn_result(&self, result: RuntimeResult<SessionHandle>) {
            self.spawn_results.lock().expect("lock").push_back(result);
        }

        fn push_send_input_result(&self, result: Result<(), RuntimeError>) {
            self.send_input_results
                .lock()
                .expect("lock")
                .push_back(result);
        }

        fn push_harness_session_id_result(&self, result: RuntimeResult<Option<String>>) {
            self.harness_session_ids
                .lock()
                .expect("lock")
                .push_back(result);
        }
    }

    #[async_trait]
    impl crate::SessionLifecycle for StubWorkerBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.spawn_specs.lock().expect("lock").push(spec.clone());
            self.spawn_results
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or(Ok(SessionHandle {
                    session_id: spec.session_id,
                    backend: self.kind.clone(),
                }))
        }

        async fn kill(&self, _session: &SessionHandle) -> RuntimeResult<()> {
            Ok(())
        }

        async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
            self.send_inputs
                .lock()
                .expect("lock")
                .push((session.clone(), input.to_vec()));

            self.send_input_results
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or(Ok(()))
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
    impl WorkerBackend for StubWorkerBackend {
        fn kind(&self) -> BackendKind {
            self.kind.clone()
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            Ok(())
        }

        async fn subscribe(
            &self,
            _session: &SessionHandle,
        ) -> RuntimeResult<crate::WorkerEventStream> {
            Ok(Box::new(EmptyStream))
        }

        async fn harness_session_id(
            &self,
            _session: &SessionHandle,
        ) -> RuntimeResult<Option<String>> {
            self.harness_session_ids
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or(Ok(None))
        }
    }

    fn selected_ticket() -> TicketSummary {
        TicketSummary {
            ticket_id: TicketId::from("linear:issue-126"),
            identifier: "AP-126".to_owned(),
            title: "Implement ticket selected start resume orchestration flow".to_owned(),
            project: None,
            state: "In Progress".to_owned(),
            url: "https://linear.app/acme/issue/AP-126".to_owned(),
            assignee: None,
            priority: Some(2),
            labels: vec!["orchestrator".to_owned()],
            updated_at: "2026-02-16T10:30:00Z".to_owned(),
        }
    }

    fn config() -> SelectedTicketFlowConfig {
        SelectedTicketFlowConfig {
            repository_roots: vec![PathBuf::from("/workspace")],
            worktrees_root: PathBuf::from("/workspace/.orchestrator/worktrees"),
            project_id: ProjectId::new("proj-126"),
            base_branch: "main".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
        }
    }

    #[tokio::test]
    async fn start_flow_creates_worktree_spawns_session_and_persists_runtime_mapping() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            Some(PathBuf::from("/workspace/repo")),
            &vcs,
            &backend,
        )
        .await
        .expect("start flow succeeds");

        assert_eq!(result.action, SelectedTicketFlowAction::Started);
        assert_eq!(vcs.created.lock().expect("lock").len(), 1);
        assert_eq!(backend.spawn_specs.lock().expect("lock").len(), 1);
        assert_eq!(backend.send_inputs.lock().expect("lock").len(), 1);
        assert!(
            String::from_utf8_lossy(&backend.send_inputs.lock().expect("lock")[0].1)
                .contains("Begin work on AP-126")
        );
        assert!(backend
            .spawn_specs
            .lock()
            .expect("lock")
            .first()
            .expect("spawn spec")
            .instruction_prelude
            .as_deref()
            .is_some_and(|prelude| prelude.contains("Begin planning for AP-126")));

        let mapping = store
            .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "issue-126")
            .expect("mapping lookup succeeds")
            .expect("mapping exists");
        assert_eq!(mapping.session.status, WorkerSessionStatus::Running);
        assert!(mapping.worktree.branch.starts_with("ap/AP-126-"));

        let events = store
            .read_events_for_work_item(&mapping.work_item_id)
            .expect("work-item events");
        assert!(events.iter().any(|event| {
            matches!(event.payload, OrchestrationEventPayload::WorkItemCreated(_))
        }));
        assert!(events.iter().any(|event| {
            matches!(event.payload, OrchestrationEventPayload::WorktreeCreated(_))
        }));
        assert!(events.iter().any(|event| {
            matches!(event.payload, OrchestrationEventPayload::SessionSpawned(_))
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event.payload,
                OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    from: WorkflowState::New,
                    to: WorkflowState::Planning,
                    reason: Some(WorkflowTransitionReason::TicketAccepted),
                    ..
                })
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event.payload,
                OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    to: WorkflowState::Implementing,
                    ..
                })
            )
        }));
    }

    #[tokio::test]
    async fn start_flow_persists_harness_session_binding_when_backend_reports_one() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::Codex);
        backend.push_harness_session_id_result(Ok(Some("thread-start-126".to_owned())));

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            Some(PathBuf::from("/workspace/repo")),
            &vcs,
            &backend,
        )
        .await
        .expect("start flow succeeds");

        let binding = store
            .find_harness_session_binding(
                &result.mapping.session.session_id,
                &result.mapping.session.backend_kind,
            )
            .expect("binding lookup")
            .expect("binding exists");
        assert_eq!(binding, "thread-start-126");
    }

    #[tokio::test]
    async fn start_flow_uses_provider_reported_worktree_path_for_spawn_and_mapping() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let canonical_path = PathBuf::from("/workspace/.orchestrator/worktrees/ap-126-canonical");
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo")
            .with_created_worktree_path(canonical_path.clone());
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            Some(PathBuf::from("/workspace/repo")),
            &vcs,
            &backend,
        )
        .await
        .expect("start flow succeeds");

        assert_eq!(
            backend.spawn_specs.lock().expect("lock")[0].workdir,
            canonical_path
        );
        assert_eq!(result.mapping.worktree.path, result.mapping.session.workdir);
        assert_eq!(
            result.mapping.session.workdir,
            "/workspace/.orchestrator/worktrees/ap-126-canonical"
        );
    }

    #[tokio::test]
    async fn start_flow_cleans_up_worktree_when_spawn_fails() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);
        backend.push_spawn_result(Err(RuntimeError::Process(
            "simulated spawn failure".to_owned(),
        )));

        let err = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            Some(PathBuf::from("/workspace/repo")),
            &vcs,
            &backend,
        )
        .await
        .expect_err("spawn failure should be returned");

        match err {
            CoreError::Runtime(RuntimeError::Process(message)) => {
                assert!(message.contains("simulated spawn failure"));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let deleted = vcs.deleted.lock().expect("lock");
        assert_eq!(deleted.len(), 1);
        assert!(!deleted[0].delete_branch);
        assert!(!deleted[0].delete_directory);
        assert!(store
            .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "issue-126")
            .expect("mapping lookup")
            .is_none());
    }

    #[tokio::test]
    async fn start_flow_requires_mapping_when_no_repository_override() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        let err = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect_err("expected mapping prompt error");

        match err {
            CoreError::MissingProjectRepositoryMapping { provider, project } => {
                assert_eq!(provider, "linear");
                assert_eq!(project, "proj-126");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_flow_uses_mapped_repository_for_project() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let vcs = StubVcsProvider::with_single_repository("/mapped/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);
        store
            .upsert_project_repository_mapping(&crate::store::ProjectRepositoryMappingRecord {
                provider: TicketProvider::Linear,
                project_id: ProjectId::new("proj-126"),
                repository_path: "/mapped/repo".to_owned(),
            })
            .expect("seed repository mapping");

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("mapped start succeeds");

        assert_eq!(result.action, SelectedTicketFlowAction::Started);
        assert_eq!(result.mapping.session.status, WorkerSessionStatus::Running);
        assert_eq!(backend.spawn_specs.lock().expect("lock").len(), 1);
    }

    fn seeded_runtime_mapping(
        store: &mut SqliteEventStore,
        status: WorkerSessionStatus,
        model: Option<&str>,
    ) -> RuntimeMappingRecord {
        let mapping = RuntimeMappingRecord {
            ticket: TicketRecord {
                ticket_id: TicketId::from("linear:issue-126"),
                provider: TicketProvider::Linear,
                provider_ticket_id: "issue-126".to_owned(),
                identifier: "AP-126".to_owned(),
                title: "Implement ticket selected start resume orchestration flow".to_owned(),
                state: "In Progress".to_owned(),
                updated_at: "2026-02-16T09:00:00Z".to_owned(),
            },
            work_item_id: WorkItemId::new("wi-linear-issue-126"),
            worktree: WorktreeRecord {
                worktree_id: WorktreeId::new("wt-linear-issue-126"),
                work_item_id: WorkItemId::new("wi-linear-issue-126"),
                path: "/workspace/.orchestrator/worktrees/ap-126-ticket".to_owned(),
                branch: "ap/AP-126-ticket".to_owned(),
                base_branch: "main".to_owned(),
                created_at: "2026-02-16T09:00:10Z".to_owned(),
            },
            session: SessionRecord {
                session_id: WorkerSessionId::new("sess-linear-issue-126"),
                work_item_id: WorkItemId::new("wi-linear-issue-126"),
                backend_kind: BackendKind::OpenCode,
                workdir: "/workspace/.orchestrator/worktrees/ap-126-ticket".to_owned(),
                model: model.map(str::to_owned),
                status,
                created_at: "2026-02-16T09:00:20Z".to_owned(),
                updated_at: "2026-02-16T09:00:30Z".to_owned(),
            },
        };
        store
            .upsert_runtime_mapping(&mapping)
            .expect("seed runtime mapping");
        mapping
    }

    fn seed_workflow_state_event(
        store: &mut SqliteEventStore,
        mapping: &RuntimeMappingRecord,
        from: WorkflowState,
        to: WorkflowState,
        reason: WorkflowTransitionReason,
    ) {
        store
            .append(new_event(
                "workflow-transition-seed",
                Some(mapping.work_item_id.clone()),
                Some(mapping.session.session_id.clone()),
                OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: mapping.work_item_id.clone(),
                    from,
                    to,
                    reason: Some(reason),
                }),
            ))
            .expect("seed workflow transition");
    }

    #[tokio::test]
    async fn resume_flow_sends_resume_instruction_without_creating_new_worktree() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let mapping = seeded_runtime_mapping(
            &mut store,
            WorkerSessionStatus::WaitingForUser,
            Some("gpt-5-codex"),
        );
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("resume flow succeeds");

        assert_eq!(result.action, SelectedTicketFlowAction::Resumed);
        assert!(vcs.created.lock().expect("lock").is_empty());
        assert!(backend.spawn_specs.lock().expect("lock").is_empty());
        assert_eq!(backend.send_inputs.lock().expect("lock").len(), 1);
        assert!(
            String::from_utf8_lossy(&backend.send_inputs.lock().expect("lock")[0].1)
                .contains("Resume work on AP-126")
        );

        let updated = store
            .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "issue-126")
            .expect("lookup")
            .expect("mapping exists");
        assert_eq!(updated.work_item_id, mapping.work_item_id);
        assert_eq!(updated.session.status, WorkerSessionStatus::Running);
    }

    #[tokio::test]
    async fn resume_flow_respawns_when_runtime_session_is_missing() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let mapping = seeded_runtime_mapping(
            &mut store,
            WorkerSessionStatus::Running,
            Some("gpt-5-codex"),
        );
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);
        backend.push_send_input_result(Err(RuntimeError::SessionNotFound(
            "sess-linear-issue-126".to_owned(),
        )));

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("resume with respawn succeeds");

        assert_eq!(result.action, SelectedTicketFlowAction::Resumed);
        assert_eq!(backend.send_inputs.lock().expect("lock").len(), 2);
        assert_eq!(backend.spawn_specs.lock().expect("lock").len(), 1);
        assert_eq!(
            backend.spawn_specs.lock().expect("lock")[0]
                .session_id
                .as_str(),
            mapping.session.session_id.as_str()
        );
    }

    #[tokio::test]
    async fn resume_respawn_for_codex_uses_persisted_harness_session_id() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let mapping = RuntimeMappingRecord {
            ticket: TicketRecord {
                ticket_id: TicketId::from("linear:issue-126"),
                provider: TicketProvider::Linear,
                provider_ticket_id: "issue-126".to_owned(),
                identifier: "AP-126".to_owned(),
                title: "Implement ticket selected start resume orchestration flow".to_owned(),
                state: "In Progress".to_owned(),
                updated_at: "2026-02-16T09:00:00Z".to_owned(),
            },
            work_item_id: WorkItemId::new("wi-linear-issue-126"),
            worktree: WorktreeRecord {
                worktree_id: WorktreeId::new("wt-linear-issue-126"),
                work_item_id: WorkItemId::new("wi-linear-issue-126"),
                path: "/workspace/.orchestrator/worktrees/ap-126-ticket".to_owned(),
                branch: "ap/AP-126-ticket".to_owned(),
                base_branch: "main".to_owned(),
                created_at: "2026-02-16T09:00:10Z".to_owned(),
            },
            session: SessionRecord {
                session_id: WorkerSessionId::new("sess-linear-issue-126"),
                work_item_id: WorkItemId::new("wi-linear-issue-126"),
                backend_kind: BackendKind::Codex,
                workdir: "/workspace/.orchestrator/worktrees/ap-126-ticket".to_owned(),
                model: Some("gpt-5-codex".to_owned()),
                status: WorkerSessionStatus::Running,
                created_at: "2026-02-16T09:00:20Z".to_owned(),
                updated_at: "2026-02-16T09:00:30Z".to_owned(),
            },
        };
        store
            .upsert_runtime_mapping(&mapping)
            .expect("seed runtime mapping");
        store
            .upsert_harness_session_binding(
                &mapping.session.session_id,
                &mapping.session.backend_kind,
                "thread-resume-126",
            )
            .expect("seed harness session binding");

        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::Codex);
        backend.push_send_input_result(Err(RuntimeError::SessionNotFound(
            "sess-linear-issue-126".to_owned(),
        )));

        start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("resume with respawn succeeds");

        let environment = &backend.spawn_specs.lock().expect("lock")[0].environment;
        assert!(environment.iter().any(|(name, value)| {
            name == ENV_HARNESS_SESSION_ID && value == "thread-resume-126"
        }));
    }

    #[tokio::test]
    async fn resume_respawn_prefers_persisted_model_over_config_override() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        seeded_runtime_mapping(
            &mut store,
            WorkerSessionStatus::Crashed,
            Some("persisted-model"),
        );
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);
        let mut resume_config = config();
        resume_config.model = Some("config-override-model".to_owned());

        let result = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &resume_config,
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("resume flow succeeds");

        assert_eq!(
            backend.spawn_specs.lock().expect("lock")[0]
                .model
                .as_deref(),
            Some("persisted-model")
        );
        assert_eq!(
            result.mapping.session.model.as_deref(),
            Some("persisted-model")
        );
    }

    #[tokio::test]
    async fn resume_flow_rejects_terminal_done_mapping() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        seeded_runtime_mapping(&mut store, WorkerSessionStatus::Done, Some("gpt-5-codex"));
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        let err = start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect_err("done mapping should reject resume");

        match err {
            CoreError::Configuration(message) => {
                assert!(message.contains("already completed"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_flow_from_testing_does_not_force_implementation_transition() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let mapping = seeded_runtime_mapping(
            &mut store,
            WorkerSessionStatus::WaitingForUser,
            Some("gpt-5-codex"),
        );
        seed_workflow_state_event(
            &mut store,
            &mapping,
            WorkflowState::Implementing,
            WorkflowState::Testing,
            WorkflowTransitionReason::TestsStarted,
        );
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("resume flow succeeds");

        let events = store
            .read_events_for_work_item(&mapping.work_item_id)
            .expect("work-item events");
        assert!(!events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if payload.to == WorkflowState::Implementing
            )
        }));
    }

    #[tokio::test]
    async fn resume_flow_from_awaiting_review_does_not_force_implementation_transition() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");
        let mapping = seeded_runtime_mapping(
            &mut store,
            WorkerSessionStatus::WaitingForUser,
            Some("gpt-5-codex"),
        );
        seed_workflow_state_event(
            &mut store,
            &mapping,
            WorkflowState::PRDrafted,
            WorkflowState::AwaitingYourReview,
            WorkflowTransitionReason::AwaitingApproval,
        );
        let vcs = StubVcsProvider::with_single_repository("/workspace/repo");
        let backend = StubWorkerBackend::new(BackendKind::OpenCode);

        start_or_resume_selected_ticket(
            &mut store,
            &selected_ticket(),
            &config(),
            None,
            &vcs,
            &backend,
        )
        .await
        .expect("resume flow succeeds");

        let events = store
            .read_events_for_work_item(&mapping.work_item_id)
            .expect("work-item events");
        assert!(!events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if payload.to == WorkflowState::Implementing
            )
        }));
    }
}
