#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        ArtifactCreatedPayload, NewEventEnvelope, OrchestrationEventPayload, StoredEventEnvelope,
    };
    use crate::normalization::DOMAIN_EVENT_SCHEMA_VERSION;
    use orchestrator_core::test_support::{with_env_var, with_env_vars, TestDbPath};
    use orchestrator_core::{
        command_ids, AddTicketCommentRequest, ArtifactId, ArtifactKind, ArtifactRecord,
        BackendCapabilities, BackendKind, CodeHostKind, Command, CommandRegistry,
        CreateTicketRequest, GetTicketRequest, LlmChatRequest, LlmFinishReason, LlmProviderKind,
        LlmResponseStream, LlmResponseSubscription, LlmRole, LlmStreamChunk, LlmToolCall,
        RuntimeMappingRecord, RuntimeResult, SessionHandle, SessionRecord, SpawnSpec,
        SupervisorQueryArgs, SupervisorQueryCancellationSource, SupervisorQueryContextArgs,
        TicketDetails, TicketId, TicketProvider, TicketQuery, TicketRecord, TicketSummary,
        UntypedCommandInvocation, UpdateTicketDescriptionRequest, UpdateTicketStateRequest,
        WorkItemId, WorkerSessionId, WorkerSessionStatus, WorktreeId, WorktreeRecord,
        WorktreeStatus,
    };
    use orchestrator_ticketing::{self as integration_linear, TicketingProvider};
    use serde_json::json;
    use std::collections::{BTreeMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[derive(Debug)]
    struct StubLinearTransport {
        responses: std::sync::Mutex<VecDeque<serde_json::Value>>,
        call_count: AtomicUsize,
    }

    impl StubLinearTransport {
        fn new(responses: Vec<serde_json::Value>) -> Self {
            Self {
                responses: std::sync::Mutex::new(VecDeque::from(responses)),
                call_count: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        fn linear_empty_issue_payload() -> serde_json::Value {
            json!({
                "viewer": {
                    "id": "viewer-1",
                },
                "issues": {
                    "nodes": []
                },
            })
        }
    }

    #[async_trait::async_trait]
    impl integration_linear::GraphqlTransport for StubLinearTransport {
        async fn execute(
            &self,
            _request: integration_linear::GraphqlRequest,
        ) -> Result<serde_json::Value, CoreError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut responses = self
                .responses
                .lock()
                .expect("linear polling transport response lock");
            responses.pop_front().ok_or_else(|| {
                CoreError::DependencyUnavailable("No stub Linear response available".to_owned())
            })
        }
    }

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "orchestrator-app-config-{prefix}-{now}-{}",
            std::process::id()
        ))
    }

    fn write_config_file(path: &Path, raw: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture config parent");
        }
        std::fs::write(path, raw.as_bytes()).expect("write fixture config");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = unique_temp_path(prefix);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn remove_temp_path(path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    fn expected_default_data_dir(home: &Path) -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            return home.join("AppData").join("Local").join("orchestrator");
        }

        #[cfg(target_os = "macos")]
        {
            return home
                .join("Library")
                .join("Application Support")
                .join("orchestrator");
        }

        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            home.join(".local").join("share").join("orchestrator")
        }
    }

    fn expected_default_workspace(home: &Path) -> PathBuf {
        expected_default_data_dir(home).join("workspace")
    }

    fn expected_default_event_store(home: &Path) -> PathBuf {
        expected_default_data_dir(home).join("orchestrator-events.db")
    }

    struct Healthy;

    #[async_trait::async_trait]
    impl Supervisor for Healthy {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl GithubClient for Healthy {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl CodeHostProvider for Healthy {
        fn kind(&self) -> CodeHostKind {
            CodeHostKind::Github
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn create_draft_pull_request(
            &self,
            _request: orchestrator_core::CreatePullRequestRequest,
        ) -> Result<orchestrator_core::PullRequestSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "healthy mock does not create pull requests in unit tests".to_owned(),
            ))
        }

        async fn mark_ready_for_review(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn request_reviewers(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
            _reviewers: orchestrator_core::ReviewerRequest,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn list_waiting_for_my_review(
            &self,
        ) -> Result<Vec<orchestrator_core::PullRequestSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn get_pull_request_merge_state(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
        ) -> Result<orchestrator_core::PullRequestMergeState, CoreError> {
            Ok(orchestrator_core::PullRequestMergeState {
                merged: false,
                is_draft: true,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: None,
                head_branch: None,
            })
        }

        async fn merge_pull_request(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
        ) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct QueryingSupervisor {
        requests: Arc<Mutex<Vec<LlmChatRequest>>>,
        cancelled_streams: Arc<Mutex<Vec<String>>>,
        stream_chunks: Arc<Mutex<VecDeque<Vec<LlmStreamChunk>>>>,
    }

    impl QueryingSupervisor {
        fn with_chunks(stream_chunks: Vec<LlmStreamChunk>) -> Self {
            Self::with_chunk_sequences(vec![stream_chunks])
        }

        fn with_chunk_sequences(stream_chunk_sequences: Vec<Vec<LlmStreamChunk>>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                cancelled_streams: Arc::new(Mutex::new(Vec::new())),
                stream_chunks: Arc::new(Mutex::new(VecDeque::from(stream_chunk_sequences))),
            }
        }

        fn requests(&self) -> Vec<LlmChatRequest> {
            self.requests.lock().expect("request lock").clone()
        }

        fn cancelled_streams(&self) -> Vec<String> {
            self.cancelled_streams.lock().expect("cancel lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl Supervisor for QueryingSupervisor {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct TestLlmStream {
        chunks: VecDeque<LlmStreamChunk>,
    }

    #[async_trait::async_trait]
    impl LlmResponseSubscription for TestLlmStream {
        async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
            Ok(self.chunks.pop_front())
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for QueryingSupervisor {
        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::OpenRouter
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn stream_chat(
            &self,
            request: LlmChatRequest,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            self.requests.lock().expect("request lock").push(request);
            let chunks = {
                let mut sequences = self.stream_chunks.lock().expect("stream chunk lock");
                sequences.pop_front().unwrap_or_else(Vec::new)
            };
            Ok((
                "test-stream-1".to_owned(),
                Box::new(TestLlmStream {
                    chunks: chunks.into(),
                }),
            ))
        }

        async fn cancel_stream(&self, stream_id: &str) -> Result<(), CoreError> {
            self.cancelled_streams
                .lock()
                .expect("cancel lock")
                .push(stream_id.to_owned());
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct MockCodeHost {
        ready_calls: Arc<Mutex<Vec<orchestrator_core::PullRequestRef>>>,
        ready_error: Arc<Mutex<Option<String>>>,
    }

    impl MockCodeHost {
        fn with_error(message: &str) -> Self {
            Self {
                ready_calls: Arc::new(Mutex::new(Vec::new())),
                ready_error: Arc::new(Mutex::new(Some(message.to_owned()))),
            }
        }

        fn ready_calls(&self) -> Vec<orchestrator_core::PullRequestRef> {
            self.ready_calls.lock().expect("ready calls lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl CodeHostProvider for MockCodeHost {
        fn kind(&self) -> CodeHostKind {
            CodeHostKind::Github
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn create_draft_pull_request(
            &self,
            request: orchestrator_core::CreatePullRequestRequest,
        ) -> Result<orchestrator_core::PullRequestSummary, CoreError> {
            Ok(orchestrator_core::PullRequestSummary {
                reference: orchestrator_core::PullRequestRef {
                    repository: request.repository,
                    number: 1,
                    url: "https://github.com/example/placeholder/pull/1".to_owned(),
                },
                title: "placeholder".to_owned(),
                is_draft: true,
            })
        }

        async fn mark_ready_for_review(
            &self,
            pr: &orchestrator_core::PullRequestRef,
        ) -> Result<(), CoreError> {
            self.ready_calls
                .lock()
                .expect("ready calls lock")
                .push(pr.clone());

            let maybe_error = self.ready_error.lock().expect("ready error lock").clone();
            match maybe_error {
                Some(message) => Err(CoreError::DependencyUnavailable(message)),
                None => Ok(()),
            }
        }

        async fn request_reviewers(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
            _reviewers: orchestrator_core::ReviewerRequest,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn list_waiting_for_my_review(
            &self,
        ) -> Result<Vec<orchestrator_core::PullRequestSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn get_pull_request_merge_state(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
        ) -> Result<orchestrator_core::PullRequestMergeState, CoreError> {
            Ok(orchestrator_core::PullRequestMergeState {
                merged: false,
                is_draft: true,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: None,
                head_branch: None,
            })
        }

        async fn merge_pull_request(
            &self,
            _pr: &orchestrator_core::PullRequestRef,
        ) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl GithubClient for MockCodeHost {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    fn template_query_invocation(template: &str) -> UntypedCommandInvocation {
        CommandRegistry::default()
            .to_untyped_invocation(&Command::SupervisorQuery(SupervisorQueryArgs::Template {
                template: template.to_owned(),
                variables: BTreeMap::new(),
                context: None,
            }))
            .expect("serialize supervisor.query")
    }

    fn template_query_invocation_with_context(
        template: &str,
        context: SupervisorQueryContextArgs,
    ) -> UntypedCommandInvocation {
        CommandRegistry::default()
            .to_untyped_invocation(&Command::SupervisorQuery(SupervisorQueryArgs::Template {
                template: template.to_owned(),
                variables: BTreeMap::new(),
                context: Some(context),
            }))
            .expect("serialize supervisor.query with context")
    }

    fn freeform_query_invocation_with_context(
        query: &str,
        context: Option<SupervisorQueryContextArgs>,
    ) -> UntypedCommandInvocation {
        CommandRegistry::default()
            .to_untyped_invocation(&Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query: query.to_owned(),
                context,
            }))
            .expect("serialize freeform supervisor.query")
    }

    fn read_events(path: &std::path::Path) -> Vec<StoredEventEnvelope> {
        let store = SqliteEventStore::open(path).expect("open event store");
        store.read_ordered().expect("read ordered events")
    }

    fn seed_runtime_mapping_with_pr_artifact(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
        artifact_id: &str,
        pr_url: &str,
    ) -> Result<(), CoreError> {
        let ticket = orchestrator_core::TicketRecord {
            ticket_id: TicketId::from_provider_uuid(
                TicketProvider::Linear,
                format!("provider-{}", work_item_id.as_str()),
            ),
            provider: TicketProvider::Linear,
            provider_ticket_id: format!("provider-{}", work_item_id.as_str()),
            identifier: format!("ORCH-{}", work_item_id.as_str()),
            title: "Workflow ready test".to_owned(),
            state: "In Progress".to_owned(),
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        };
        let worktree_path = PathBuf::from(format!("/workspace/{}", work_item_id.as_str()));
        let runtime = RuntimeMappingRecord {
            ticket,
            work_item_id: work_item_id.clone(),
            worktree: WorktreeRecord {
                worktree_id: WorktreeId::new(format!("wt-{}", work_item_id.as_str())),
                work_item_id: work_item_id.clone(),
                path: worktree_path.to_string_lossy().to_string(),
                branch: "feature/approve-test".to_owned(),
                base_branch: "main".to_owned(),
                created_at: "2026-02-16T11:00:00Z".to_owned(),
            },
            session: SessionRecord {
                session_id: WorkerSessionId::new(session_id),
                work_item_id: work_item_id.clone(),
                backend_kind: BackendKind::OpenCode,
                workdir: worktree_path.to_string_lossy().to_string(),
                model: Some("gpt-5".to_owned()),
                status: WorkerSessionStatus::Running,
                created_at: "2026-02-16T11:00:00Z".to_owned(),
                updated_at: "2026-02-16T11:01:00Z".to_owned(),
            },
        };

        store.upsert_runtime_mapping(&runtime)?;

        let artifact = ArtifactRecord {
            artifact_id: ArtifactId::new(artifact_id),
            work_item_id: work_item_id.clone(),
            kind: ArtifactKind::PR,
            metadata: json!({"type": "pull_request"}),
            storage_ref: pr_url.to_owned(),
            created_at: "2026-02-16T11:01:00Z".to_owned(),
        };
        store.create_artifact(&artifact)?;
        store.append_event(
            NewEventEnvelope {
                event_id: format!("evt-pr-artifact-{}", artifact_id),
                occurred_at: "2026-02-16T11:02:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(WorkerSessionId::new(session_id)),
                payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                    artifact_id: artifact.artifact_id.clone(),
                    work_item_id: work_item_id.clone(),
                    kind: ArtifactKind::PR,
                    label: "Pull request".to_owned(),
                    uri: pr_url.to_owned(),
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            },
            &[artifact.artifact_id.clone()],
        )?;

        Ok(())
    }

    #[test]
    fn config_defaults_when_env_missing() {
        let home = unique_temp_dir("home");
        let expected = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");
        let expected_workspace = expected_default_workspace(&home);
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", None),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("default config");
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );
                assert_eq!(config.ticketing_provider, "ticketing.linear");
                assert_eq!(config.harness_provider, "harness.codex");
                assert_eq!(config.vcs_provider, "vcs.git_cli");
                assert_eq!(config.vcs_repo_provider, "vcs_repos.github_gh_cli");
                assert_eq!(config.ui.transcript_line_limit, 100);
                assert_eq!(config.ui.background_session_refresh_secs, 15);
                assert_eq!(config.ui.session_info_background_refresh_secs, 15);
                assert_eq!(config.ui.merge_poll_base_interval_secs, 15);
                assert_eq!(config.ui.merge_poll_max_backoff_secs, 120);
                assert_eq!(config.ui.merge_poll_backoff_multiplier, 2);
                assert!(Path::new(&config.workspace).is_absolute());
                assert!(Path::new(&config.event_store_path).is_absolute());
                assert_eq!(
                    expected,
                    default_config_path().expect("default config path")
                );
                assert!(expected.exists());
                let raw = std::fs::read_to_string(expected.clone()).unwrap();
                let parsed: AppConfig = toml::from_str(&raw).unwrap();
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    parsed.event_store_path,
                    expected_event_store.to_string_lossy()
                );
                assert_eq!(parsed.ticketing_provider, "ticketing.linear");
                assert_eq!(parsed.harness_provider, "harness.codex");
                assert_eq!(parsed.vcs_provider, "vcs.git_cli");
                assert_eq!(parsed.vcs_repo_provider, "vcs_repos.github_gh_cli");
                assert_eq!(parsed.ui.transcript_line_limit, 100);
                assert_eq!(parsed.ui.background_session_refresh_secs, 15);
                assert_eq!(parsed.ui.session_info_background_refresh_secs, 15);
                assert_eq!(parsed.ui.merge_poll_base_interval_secs, 15);
                assert_eq!(parsed.ui.merge_poll_max_backoff_secs, 120);
                assert_eq!(parsed.ui.merge_poll_backoff_multiplier, 2);
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_creates_default_when_missing() {
        let home = unique_temp_dir("create");
        let expected = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");
        let expected_workspace = expected_default_workspace(&home);
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(expected.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("bootstrap config");
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );
                assert_eq!(config.ticketing_provider, "ticketing.linear");
                assert_eq!(config.harness_provider, "harness.codex");
                assert_eq!(config.vcs_provider, "vcs.git_cli");
                assert_eq!(config.vcs_repo_provider, "vcs_repos.github_gh_cli");
                assert_eq!(config.ui.transcript_line_limit, 100);
                assert_eq!(config.ui.background_session_refresh_secs, 15);
                assert_eq!(config.ui.session_info_background_refresh_secs, 15);
                assert_eq!(config.ui.merge_poll_base_interval_secs, 15);
                assert_eq!(config.ui.merge_poll_max_backoff_secs, 120);
                assert_eq!(config.ui.merge_poll_backoff_multiplier, 2);
                assert!(expected.exists());
                let contents = std::fs::read_to_string(expected.clone()).unwrap();
                let parsed: AppConfig = toml::from_str(&contents).unwrap();
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    parsed.event_store_path,
                    expected_event_store.to_string_lossy()
                );
                assert_eq!(parsed.ticketing_provider, "ticketing.linear");
                assert_eq!(parsed.harness_provider, "harness.codex");
                assert_eq!(parsed.vcs_provider, "vcs.git_cli");
                assert_eq!(parsed.vcs_repo_provider, "vcs_repos.github_gh_cli");
                assert_eq!(parsed.ui.transcript_line_limit, 100);
                assert_eq!(parsed.ui.background_session_refresh_secs, 15);
                assert_eq!(parsed.ui.session_info_background_refresh_secs, 15);
                assert_eq!(parsed.ui.merge_poll_base_interval_secs, 15);
                assert_eq!(parsed.ui.merge_poll_max_backoff_secs, 120);
                assert_eq!(parsed.ui.merge_poll_backoff_multiplier, 2);
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn new_to_planning_instruction_requires_plan_file_and_summary_only() {
        let (_, _, _, instruction) = App::<Healthy, Healthy>::workflow_advance_target(
            &WorkflowState::New,
        )
        .expect("resolve workflow advance target");
        let instruction = instruction.expect("new->planning instruction");
        assert!(instruction.contains("implementation plan"));
        assert!(instruction.contains("planning mode"));
    }

    #[test]
    fn config_parses_from_toml_file() {
        let home = unique_temp_dir("parse");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse config");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(config.event_store_path, "/tmp/events.db");
                assert_eq!(config.ui.transcript_line_limit, 100);
                assert_eq!(config.ui.background_session_refresh_secs, 15);
                assert_eq!(config.ui.session_info_background_refresh_secs, 15);
                assert_eq!(config.ui.merge_poll_base_interval_secs, 15);
                assert_eq!(config.ui.merge_poll_max_backoff_secs, 120);
                assert_eq!(config.ui.merge_poll_backoff_multiplier, 2);
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_defaults_event_store_path_when_missing_in_toml_file() {
        let home = unique_temp_dir("partial");
        let config_path = home.join("config.toml");
        write_config_file(&config_path, "workspace = '/tmp/work'\n");
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("parse config");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_upgrades_legacy_relative_defaults() {
        let home = unique_temp_dir("legacy-upgrade");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = './'\nevent_store_path = './orchestrator-events.db'\n",
        );
        let expected_workspace = expected_default_workspace(&home);
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("config should load");
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert!(Path::new(&config.workspace).is_absolute());
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );

                let rewritten = std::fs::read_to_string(&config_path).expect("read rewritten");
                let parsed: AppConfig = toml::from_str(&rewritten).expect("parse rewritten");
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert!(!rewritten.contains("worktrees_root"));
                assert_eq!(
                    parsed.event_store_path,
                    expected_event_store.to_string_lossy()
                );
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_preserves_non_legacy_relative_paths() {
        let home = unique_temp_dir("relative-preserved");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = './custom-workspace'\nevent_store_path = './custom-events.db'\n",
        );

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("config should load");
                assert_eq!(config.workspace, "./custom-workspace");
                assert_eq!(config.event_store_path, "./custom-events.db");
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_preserves_legacy_worktree_directory_layout() {
        let home = unique_temp_dir("legacy-worktree-layout");
        let workspace = expected_default_workspace(&home);
        let legacy_root = workspace.join(".orchestrator").join("worktrees");
        let legacy_worktree = legacy_root.join("ap-999-sample-ticket");
        std::fs::create_dir_all(&legacy_worktree).expect("create legacy worktree");
        write_config_file(&legacy_worktree.join("marker.txt"), "legacy");

        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            format!(
                "workspace = '{}'\nworktrees_root = '{}'\nevent_store_path = '{}'\n",
                workspace.display(),
                workspace.display(),
                expected_default_event_store(&home).display()
            )
            .as_str(),
        );

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                AppConfig::from_env().expect("config should load");
                let migrated = PathBuf::from(workspace.clone())
                    .join("ap-999-sample-ticket")
                    .join("marker.txt");
                assert!(!migrated.exists());
                assert!(legacy_worktree.join("marker.txt").exists());
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_preserves_runtime_mapping_paths_from_legacy_worktree_root() {
        let home = unique_temp_dir("legacy-runtime-mapping-layout");
        let workspace = expected_default_workspace(&home);
        let event_store_path = expected_default_event_store(&home);
        if let Some(parent) = event_store_path.parent() {
            std::fs::create_dir_all(parent).expect("create event store parent");
        }

        let mut store = SqliteEventStore::open(&event_store_path).expect("create event store");
        let ticket = TicketRecord {
            ticket_id: TicketId::from("linear:issue-299"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "issue-299".to_owned(),
            identifier: "AP-299".to_owned(),
            title: "Legacy mapping".to_owned(),
            state: "In Progress".to_owned(),
            updated_at: "2026-02-21T12:00:00Z".to_owned(),
        };
        let legacy_workdir = workspace
            .join(".orchestrator")
            .join("worktrees")
            .join("ap-299-legacy-mapping")
            .to_string_lossy()
            .to_string();
        store
            .upsert_runtime_mapping(&RuntimeMappingRecord {
                ticket,
                work_item_id: WorkItemId::new("wi-linear-issue-299"),
                worktree: WorktreeRecord {
                    worktree_id: WorktreeId::new("wt-linear-issue-299"),
                    work_item_id: WorkItemId::new("wi-linear-issue-299"),
                    path: legacy_workdir.clone(),
                    branch: "ap/AP-299-legacy-mapping".to_owned(),
                    base_branch: "main".to_owned(),
                    created_at: "2026-02-21T12:00:00Z".to_owned(),
                },
                session: SessionRecord {
                    session_id: WorkerSessionId::new("sess-linear-issue-299"),
                    work_item_id: WorkItemId::new("wi-linear-issue-299"),
                    backend_kind: BackendKind::Codex,
                    workdir: legacy_workdir,
                    model: Some("gpt-5-codex".to_owned()),
                    status: WorkerSessionStatus::Running,
                    created_at: "2026-02-21T12:00:00Z".to_owned(),
                    updated_at: "2026-02-21T12:00:00Z".to_owned(),
                },
            })
            .expect("seed runtime mapping");

        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            format!(
                "workspace = '{}'\nworktrees_root = '{}'\nevent_store_path = '{}'\n",
                workspace.display(),
                workspace.display(),
                event_store_path.display()
            )
            .as_str(),
        );

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                AppConfig::from_env().expect("config should load and migrate mappings");
            },
        );

        let store = SqliteEventStore::open(&event_store_path).expect("open migrated store");
        let mapping = store
            .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "issue-299")
            .expect("mapping lookup")
            .expect("mapping exists");
        let expected = workspace
            .join(".orchestrator")
            .join("worktrees")
            .join("ap-299-legacy-mapping")
            .to_string_lossy()
            .to_string();
        assert_eq!(mapping.worktree.path, expected);
        assert_eq!(mapping.session.workdir, expected);

        remove_temp_path(&home);
    }

    #[test]
    fn config_rejects_invalid_toml_file() {
        let home = unique_temp_dir("invalid");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = [\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let err = AppConfig::from_env().expect_err("invalid toml should fail");
                let message = err.to_string();
                assert!(message.contains("Failed to parse ORCHESTRATOR_CONFIG"));
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_clamps_background_refresh_settings_to_supported_bounds() {
        let home = unique_temp_dir("ui-refresh-clamp");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n[ui]\nbackground_session_refresh_secs = 99\nsession_info_background_refresh_secs = 3\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse and normalize config");
                assert_eq!(config.ui.background_session_refresh_secs, 15);
                assert_eq!(config.ui.session_info_background_refresh_secs, 15);
            },
        );

        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n[ui]\nbackground_session_refresh_secs = 1\nsession_info_background_refresh_secs = 30\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse and normalize low config");
                assert_eq!(config.ui.background_session_refresh_secs, 2);
                assert_eq!(config.ui.session_info_background_refresh_secs, 30);
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_clamps_transcript_line_limit_to_supported_bounds() {
        let home = unique_temp_dir("ui-transcript-limit-clamp");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n[ui]\ntranscript_line_limit = 0\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse and normalize config");
                assert_eq!(config.ui.transcript_line_limit, 1);
            },
        );

        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n[ui]\ntranscript_line_limit = 250\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse and preserve config");
                assert_eq!(config.ui.transcript_line_limit, 250);
            },
        );

        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n[ui]\nmerge_poll_base_interval_secs = 1\nmerge_poll_max_backoff_secs = 9999\nmerge_poll_backoff_multiplier = 0\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse and normalize merge poll config");
                assert_eq!(config.ui.merge_poll_base_interval_secs, 5);
                assert_eq!(config.ui.merge_poll_max_backoff_secs, 900);
                assert_eq!(config.ui.merge_poll_backoff_multiplier, 1);
            },
        );

        remove_temp_path(&home);
    }

    #[derive(Debug)]
    struct MockTicketingProvider;

    impl MockTicketingProvider {
        fn service() -> Arc<dyn TicketingProvider + Send + Sync> {
            Arc::new(Self)
        }
    }

    #[async_trait::async_trait]
    impl TicketingProvider for MockTicketingProvider {
        fn provider(&self) -> TicketProvider {
            TicketProvider::Linear
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn list_tickets(&self, _query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn create_ticket(
            &self,
            _request: CreateTicketRequest,
        ) -> Result<TicketSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "mock ticketing provider does not implement create_ticket in tests".to_owned(),
            ))
        }

        async fn update_ticket_state(
            &self,
            _request: UpdateTicketStateRequest,
        ) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "mock ticketing provider does not implement update_ticket_state in tests"
                    .to_owned(),
            ))
        }

        async fn get_ticket(&self, _request: GetTicketRequest) -> Result<TicketDetails, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "mock ticketing provider does not implement get_ticket in tests".to_owned(),
            ))
        }

        async fn update_ticket_description(
            &self,
            _request: UpdateTicketDescriptionRequest,
        ) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "mock ticketing provider does not implement update_ticket_description in tests"
                    .to_owned(),
            ))
        }

        async fn add_comment(&self, _request: AddTicketCommentRequest) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ScriptedTicketingProvider {
        provider: TicketProvider,
        list_tickets_calls: Arc<Mutex<Vec<TicketQuery>>>,
        get_ticket_calls: Arc<Mutex<Vec<TicketId>>>,
        update_ticket_state_calls: Arc<Mutex<Vec<(TicketId, String)>>>,
        update_ticket_description_calls: Arc<Mutex<Vec<(TicketId, String)>>>,
        add_comment_calls: Arc<Mutex<Vec<(TicketId, String)>>>,
        list_tickets_result: Mutex<Option<Result<Vec<TicketSummary>, CoreError>>>,
        get_ticket_result: Mutex<Option<Result<TicketDetails, CoreError>>>,
        update_ticket_state_result: Mutex<Option<Result<(), CoreError>>>,
        update_ticket_description_result: Mutex<Option<Result<(), CoreError>>>,
        add_comment_result: Mutex<Option<Result<(), CoreError>>>,
    }

    impl Default for ScriptedTicketingProvider {
        fn default() -> Self {
            Self::new(TicketProvider::Linear)
        }
    }

    impl ScriptedTicketingProvider {
        fn new(provider: TicketProvider) -> Self {
            Self {
                provider,
                list_tickets_calls: Arc::new(Mutex::new(Vec::new())),
                get_ticket_calls: Arc::new(Mutex::new(Vec::new())),
                update_ticket_state_calls: Arc::new(Mutex::new(Vec::new())),
                update_ticket_description_calls: Arc::new(Mutex::new(Vec::new())),
                add_comment_calls: Arc::new(Mutex::new(Vec::new())),
                list_tickets_result: Mutex::new(Some(Ok(Vec::new()))),
                get_ticket_result: Mutex::new(Some(Ok(TicketDetails {
                    summary: TicketSummary {
                        ticket_id: TicketId::from("linear:missing"),
                        identifier: "MISSING".to_owned(),
                        title: "Missing".to_owned(),
                        project: Some("Missing".to_owned()),
                        state: "Unknown".to_owned(),
                        url: "https://linear.app/missing".to_owned(),
                        assignee: None,
                        priority: None,
                        labels: Vec::new(),
                        updated_at: "1970-01-01T00:00:00Z".to_owned(),
                    },
                    description: None,
                }))),
                update_ticket_state_result: Mutex::new(Some(Ok(()))),
                update_ticket_description_result: Mutex::new(Some(Ok(()))),
                add_comment_result: Mutex::new(Some(Ok(()))),
            }
        }

        fn with_list_tickets_result(&self, tickets: Vec<TicketSummary>) -> &Self {
            *self.list_tickets_result.lock().expect("list result lock") = Some(Ok(tickets));
            self
        }

        fn with_update_state_result(&self, result: Result<(), CoreError>) -> &Self {
            *self
                .update_ticket_state_result
                .lock()
                .expect("update state lock") = Some(result);
            self
        }

        fn list_tickets_calls(&self) -> Vec<TicketQuery> {
            self.list_tickets_calls
                .lock()
                .expect("list calls lock")
                .clone()
        }

        fn update_ticket_state_calls(&self) -> Vec<(TicketId, String)> {
            self.update_ticket_state_calls
                .lock()
                .expect("state calls lock")
                .clone()
        }
    }

    #[async_trait::async_trait]
    impl TicketingProvider for ScriptedTicketingProvider {
        fn provider(&self) -> TicketProvider {
            self.provider.clone()
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn list_tickets(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
            self.list_tickets_calls
                .lock()
                .expect("record list call lock")
                .push(query);
            self.list_tickets_result
                .lock()
                .expect("list result lock")
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        async fn create_ticket(
            &self,
            _request: CreateTicketRequest,
        ) -> Result<TicketSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "scripted mock ticketing provider does not implement create_ticket in tests"
                    .to_owned(),
            ))
        }

        async fn update_ticket_state(
            &self,
            request: UpdateTicketStateRequest,
        ) -> Result<(), CoreError> {
            self.update_ticket_state_calls
                .lock()
                .expect("record state call lock")
                .push((request.ticket_id, request.state));
            self.update_ticket_state_result
                .lock()
                .expect("update state lock")
                .take()
                .unwrap_or_else(|| Ok(()))
        }

        async fn get_ticket(&self, request: GetTicketRequest) -> Result<TicketDetails, CoreError> {
            self.get_ticket_calls
                .lock()
                .expect("record get call lock")
                .push(request.ticket_id);
            self.get_ticket_result
                .lock()
                .expect("get result lock")
                .take()
                .unwrap_or_else(|| {
                    Ok(TicketDetails {
                        summary: TicketSummary {
                            ticket_id: TicketId::from("linear:missing"),
                            identifier: "MISSING".to_owned(),
                            title: "Missing".to_owned(),
                            project: Some("Missing".to_owned()),
                            state: "Unknown".to_owned(),
                            url: "https://linear.app/missing".to_owned(),
                            assignee: None,
                            priority: None,
                            labels: Vec::new(),
                            updated_at: "1970-01-01T00:00:00Z".to_owned(),
                        },
                        description: None,
                    })
                })
        }

        async fn update_ticket_description(
            &self,
            request: UpdateTicketDescriptionRequest,
        ) -> Result<(), CoreError> {
            self.update_ticket_description_calls
                .lock()
                .expect("record description call lock")
                .push((request.ticket_id, request.description));
            self.update_ticket_description_result
                .lock()
                .expect("description result lock")
                .take()
                .unwrap_or_else(|| Ok(()))
        }

        async fn add_comment(&self, request: AddTicketCommentRequest) -> Result<(), CoreError> {
            self.add_comment_calls
                .lock()
                .expect("record comment call lock")
                .push((request.ticket_id, request.comment));
            self.add_comment_result
                .lock()
                .expect("comment result lock")
                .take()
                .unwrap_or_else(|| Ok(()))
        }
    }

    #[tokio::test]
    async fn startup_composition_succeeds_with_mock_adapters() {
        let temp_db = TestDbPath::new("app-startup-test");

        let app = App {
            config: AppConfig {
                workspace: "./".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: Healthy,
            github: Healthy,
        };

        let state = app.startup_state().await.expect("startup state");
        assert!(state.status.contains("ready"));
        assert!(state.projection.events.is_empty());
    }

    struct StubVcs {
        repository: orchestrator_core::RepositoryRef,
        create_calls: Mutex<Vec<orchestrator_core::CreateWorktreeRequest>>,
    }

    #[async_trait::async_trait]
    impl orchestrator_core::VcsProvider for StubVcs {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn discover_repositories(
            &self,
            _roots: &[PathBuf],
        ) -> Result<Vec<orchestrator_core::RepositoryRef>, CoreError> {
            Ok(vec![self.repository.clone()])
        }

        async fn create_worktree(
            &self,
            request: orchestrator_core::CreateWorktreeRequest,
        ) -> Result<orchestrator_core::WorktreeSummary, CoreError> {
            self.create_calls
                .lock()
                .expect("lock")
                .push(request.clone());
            Ok(orchestrator_core::WorktreeSummary {
                worktree_id: request.worktree_id,
                repository: request.repository,
                path: request.worktree_path,
                branch: request.branch,
                base_branch: request.base_branch,
            })
        }

        async fn delete_worktree(
            &self,
            _request: orchestrator_core::DeleteWorktreeRequest,
        ) -> Result<(), CoreError> {
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

    #[async_trait::async_trait]
    impl orchestrator_vcs::VcsProvider for StubVcs {
        fn kind(&self) -> orchestrator_vcs::VcsProviderKind {
            orchestrator_vcs::VcsProviderKind::GitCli
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            <Self as orchestrator_core::VcsProvider>::health_check(self).await
        }

        async fn discover_repositories(
            &self,
            roots: &[PathBuf],
        ) -> Result<Vec<orchestrator_vcs::RepositoryRef>, CoreError> {
            <Self as orchestrator_core::VcsProvider>::discover_repositories(self, roots).await
        }

        async fn create_worktree(
            &self,
            request: orchestrator_vcs::CreateWorktreeRequest,
        ) -> Result<orchestrator_vcs::WorktreeSummary, CoreError> {
            <Self as orchestrator_core::VcsProvider>::create_worktree(self, request).await
        }

        async fn delete_worktree(
            &self,
            request: orchestrator_vcs::DeleteWorktreeRequest,
        ) -> Result<(), CoreError> {
            <Self as orchestrator_core::VcsProvider>::delete_worktree(self, request).await
        }

        async fn worktree_status(
            &self,
            worktree_path: &Path,
        ) -> Result<orchestrator_vcs::WorktreeStatus, CoreError> {
            <Self as orchestrator_core::VcsProvider>::worktree_status(self, worktree_path).await
        }
    }

    struct EmptyStream;

    #[async_trait::async_trait]
    impl orchestrator_core::WorkerEventSubscription for EmptyStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<orchestrator_core::BackendEvent>> {
            Ok(None)
        }
    }

    struct StubBackend {
        spawn_calls: Mutex<Vec<SpawnSpec>>,
    }

    #[async_trait::async_trait]
    impl orchestrator_core::SessionLifecycle for StubBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.spawn_calls.lock().expect("lock").push(spec.clone());
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

    #[async_trait::async_trait]
    impl WorkerBackend for StubBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::OpenCode
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
        ) -> RuntimeResult<orchestrator_core::WorkerEventStream> {
            Ok(Box::new(EmptyStream))
        }
    }

    #[tokio::test]
    async fn app_starts_selected_ticket_and_persists_runtime_mapping() {
        let temp_db = TestDbPath::new("app-ticket-selected-start");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: Healthy,
            github: Healthy,
        };
        let vcs = StubVcs {
            repository: orchestrator_core::RepositoryRef {
                id: "/workspace/repo".to_owned(),
                name: "repo".to_owned(),
                root: PathBuf::from("/workspace/repo"),
            },
            create_calls: Mutex::new(Vec::new()),
        };
        let backend = StubBackend {
            spawn_calls: Mutex::new(Vec::new()),
        };
        let ticket = TicketSummary {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "issue-126"),
            identifier: "AP-126".to_owned(),
            title: "Implement ticket selected start resume orchestration flow".to_owned(),
            project: None,
            state: "In Progress".to_owned(),
            url: "https://linear.app/acme/issue/AP-126".to_owned(),
            assignee: None,
            priority: Some(2),
            labels: vec!["orchestrator".to_owned()],
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        };

        let result = app
            .start_or_resume_selected_ticket(
                &ticket,
                Some(vcs.repository.root.clone()),
                &vcs,
                &backend,
            )
            .await
            .expect("start selected ticket");

        assert_eq!(
            result.action,
            orchestrator_core::SelectedTicketFlowAction::Started
        );
        assert_eq!(vcs.create_calls.lock().expect("lock").len(), 1);
        assert_eq!(backend.spawn_calls.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn app_archive_session_marks_mapping_done_and_emits_completed_event() {
        let temp_db = TestDbPath::new("app-archive-session");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: Healthy,
            github: Healthy,
        };
        let session_id = WorkerSessionId::new("sess-archive-1");
        {
            let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
            store
                .upsert_runtime_mapping(&RuntimeMappingRecord {
                    ticket: orchestrator_core::TicketRecord {
                        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "issue-1"),
                        provider: TicketProvider::Linear,
                        provider_ticket_id: "issue-1".to_owned(),
                        identifier: "AP-1".to_owned(),
                        title: "Archive session".to_owned(),
                        state: "In Progress".to_owned(),
                        updated_at: "2026-02-20T00:00:00Z".to_owned(),
                    },
                    work_item_id: WorkItemId::new("wi-archive-1"),
                    worktree: WorktreeRecord {
                        worktree_id: WorktreeId::new("wt-archive-1"),
                        work_item_id: WorkItemId::new("wi-archive-1"),
                        path: "/tmp/nonexistent-worktree-archive-1".to_owned(),
                        branch: "ap/AP-1-archive-session".to_owned(),
                        base_branch: "main".to_owned(),
                        created_at: "2026-02-20T00:00:00Z".to_owned(),
                    },
                    session: SessionRecord {
                        session_id: session_id.clone(),
                        work_item_id: WorkItemId::new("wi-archive-1"),
                        backend_kind: BackendKind::OpenCode,
                        workdir: "/tmp/nonexistent-worktree-archive-1".to_owned(),
                        model: Some("gpt-5".to_owned()),
                        status: WorkerSessionStatus::Running,
                        created_at: "2026-02-20T00:00:00Z".to_owned(),
                        updated_at: "2026-02-20T00:00:00Z".to_owned(),
                    },
                })
                .expect("seed runtime mapping");
        }

        let backend = StubBackend {
            spawn_calls: Mutex::new(Vec::new()),
        };

        let outcome = app
            .archive_session(&session_id, &backend)
            .await
            .expect("archive session");
        assert!(outcome.warning.is_none());
        assert!(matches!(
            outcome.event.payload,
            OrchestrationEventPayload::SessionCompleted(_)
        ));

        let store = SqliteEventStore::open(temp_db.path()).expect("open store");
        let mapping = store
            .find_runtime_mapping_by_session_id(&session_id)
            .expect("find mapping")
            .expect("mapping exists");
        assert_eq!(mapping.session.status, WorkerSessionStatus::Done);

        let events = read_events(temp_db.path());
        assert!(events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::SessionCompleted(payload)
                    if payload.session_id == session_id
            )
        }));
    }

    #[tokio::test]
    async fn app_start_and_stop_linear_polling_lifecycle() {
        let temp_db = TestDbPath::new("app-linear-polling-lifecycle");
        let transport = Arc::new(StubLinearTransport::new(vec![
            StubLinearTransport::linear_empty_issue_payload(),
        ]));
        let transport_trait: Arc<dyn integration_linear::GraphqlTransport> = transport.clone();
        let mut linear_config = integration_linear::LinearConfig::default();
        linear_config.poll_interval = Duration::from_secs(3600);
        let linear_ticketing = Arc::new(LinearTicketingProvider::with_transport(
            linear_config,
            transport_trait,
        ));

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: Healthy,
            github: Healthy,
        };

        app.start_linear_polling(Some(&linear_ticketing))
            .await
            .expect("start polling");
        app.stop_linear_polling(Some(&linear_ticketing))
            .await
            .expect("stop polling");
        assert_eq!(transport.call_count(), 1);
    }

    #[tokio::test]
    async fn app_linear_polling_lifecycle_is_noop_without_linear_provider() {
        let temp_db = TestDbPath::new("app-linear-polling-noop");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.shortcut".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: Healthy,
            github: Healthy,
        };

        app.start_linear_polling(None)
            .await
            .expect("start polling noop");
        app.stop_linear_polling(None)
            .await
            .expect("stop polling noop");
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_streams_from_runtime_handler() {
        let temp_db = TestDbPath::new("app-supervisor-query-dispatch");
        let supervisor = QueryingSupervisor::with_chunks(vec![
            LlmStreamChunk {
                delta: "Current activity: worker is implementing AP-180".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            },
            LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            },
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status_current_session"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        assert!(stream_id.starts_with("runtime-"));

        let first = stream
            .next_chunk()
            .await
            .expect("poll first chunk")
            .expect("first chunk");
        assert!(first.delta.contains("Current activity"));
        assert_eq!(first.finish_reason, None);

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Template: Current activity"));
        assert!(requests[0].messages[1]
            .content
            .contains("Scope: session:sess-query"));
        assert!(requests[0].messages[1].content.contains("Context pack:"));
        assert!(requests[0].messages[1]
            .content
            .contains("Ticket status context:"));
        assert!(requests[0].messages[1]
            .content
            .contains("Ticket status fallback:"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_executes_list_tickets_tool() {
        let temp_db = TestDbPath::new("app-supervisor-query-list-tickets-tool");
        let ticketing = {
            let ticketing = ScriptedTicketingProvider::new(TicketProvider::Linear);
            ticketing.with_list_tickets_result(vec![TicketSummary {
                ticket_id: TicketId::from("linear:issue-101"),
                identifier: "ISSUE-101".to_owned(),
                title: "Validate tool calls".to_owned(),
                project: Some("Orchestrator".to_owned()),
                state: "In Review".to_owned(),
                url: "https://linear.app/example/issue/ISSUE-101".to_owned(),
                assignee: Some("owner".to_owned()),
                priority: Some(1),
                labels: vec!["orchestrator".to_owned()],
                updated_at: "2026-02-16T11:00:00Z".to_owned(),
            }]);
            Arc::new(ticketing)
        };

        let supervisor = QueryingSupervisor::with_chunk_sequences(vec![
            vec![LlmStreamChunk {
                delta: String::new(),
                tool_calls: vec![LlmToolCall {
                    id: "call_list_tickets_1".to_owned(),
                    name: "list_tickets".to_owned(),
                    arguments: serde_json::json!({
                        "assigned_to_me": true,
                        "states": ["In Review", "Blocked"],
                        "search": "tool",
                        "limit": 1,
                    })
                    .to_string(),
                }],
                finish_reason: Some(LlmFinishReason::ToolCall),
                usage: None,
                rate_limit: None,
            }],
            vec![LlmStreamChunk {
                delta: "There are matching tickets.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }],
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: ticketing.clone(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id, mut stream) = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "list open blocking tickets",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-query".to_owned()),
                        selected_session_id: None,
                        scope: Some("work_item:wi-query".to_owned()),
                    }),
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: None,
                    scope: Some("work_item:wi-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let mut chunk_payloads = Vec::new();
        while let Some(chunk) = stream.next_chunk().await.expect("tool stream poll") {
            chunk_payloads.push(chunk.delta);
        }

        assert!(chunk_payloads
            .iter()
            .any(|chunk| chunk.contains("[tool-call] list_tickets")));
        assert!(chunk_payloads
            .iter()
            .any(|chunk| chunk.contains("[tool-result] list_tickets")));
        assert!(chunk_payloads
            .iter()
            .any(|chunk| chunk.contains("There are matching tickets.")));

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].tools.len(), 5);
        assert!(requests[1]
            .tools
            .iter()
            .any(|tool| tool.function.name == "list_tickets"));
        assert!(requests[1]
            .messages
            .iter()
            .any(|message| message.role == LlmRole::Tool
                && message.name.as_deref() == Some("list_tickets")));

        let list_calls = ticketing.list_tickets_calls();
        assert_eq!(list_calls.len(), 1);
        assert_eq!(list_calls[0].assigned_to_me, true);
        assert_eq!(
            list_calls[0].states,
            vec!["In Review".to_owned(), "Blocked".to_owned()]
        );
        assert_eq!(list_calls[0].search, Some("tool".to_owned()));
        assert_eq!(list_calls[0].limit, Some(1));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_executes_update_ticket_state_tool() {
        let temp_db = TestDbPath::new("app-supervisor-query-update-state-tool");
        let ticketing = Arc::new(ScriptedTicketingProvider::new(TicketProvider::Linear));
        ticketing.with_update_state_result(Ok(()));

        let supervisor = QueryingSupervisor::with_chunk_sequences(vec![
            vec![LlmStreamChunk {
                delta: String::new(),
                tool_calls: vec![LlmToolCall {
                    id: "call_update_state_1".to_owned(),
                    name: "update_ticket_state".to_owned(),
                    arguments: serde_json::json!({
                        "ticket_id": "linear:issue-202",
                        "state": "In Review",
                    })
                    .to_string(),
                }],
                finish_reason: Some(LlmFinishReason::ToolCall),
                usage: None,
                rate_limit: None,
            }],
            vec![LlmStreamChunk {
                delta: "Status updated successfully.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }],
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: ticketing.clone(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id, mut stream) = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "mark ticket as in review",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-query".to_owned()),
                        selected_session_id: None,
                        scope: Some("work_item:wi-query".to_owned()),
                    }),
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: None,
                    scope: Some("work_item:wi-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let mut chunk_payloads = Vec::new();
        while let Some(chunk) = stream.next_chunk().await.expect("tool stream poll") {
            chunk_payloads.push(chunk.delta);
        }

        assert!(chunk_payloads
            .iter()
            .any(|chunk| chunk.contains("[tool-result] update_ticket_state")));
        assert!(chunk_payloads
            .iter()
            .any(|chunk| chunk.contains("Status updated successfully.")));

        let calls = ticketing.update_ticket_state_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_str(), "linear:issue-202");
        assert_eq!(calls[0].1, "In Review");
        let requests = supervisor.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1]
            .messages
            .iter()
            .any(|message| message.role == LlmRole::Tool
                && message.name.as_deref() == Some("update_ticket_state")));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_global_scope_streams_and_persists_global_events() {
        let temp_db = TestDbPath::new("app-supervisor-query-global");
        let supervisor = QueryingSupervisor::with_chunks(vec![
            LlmStreamChunk {
                delta: "Global summary: inbox has two approvals pending.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            },
            LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: Some(orchestrator_core::LlmTokenUsage {
                    input_tokens: 18,
                    output_tokens: 11,
                    total_tokens: 29,
                }),
                rate_limit: None,
            },
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id, mut stream) = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What needs my attention globally?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: None,
                        selected_session_id: None,
                        scope: Some("global".to_owned()),
                    }),
                ),
                SupervisorCommandContext::default(),
            )
            .await
            .expect("dispatch should stream");

        while stream
            .next_chunk()
            .await
            .expect("poll global stream")
            .is_some()
        {}

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1].content.contains("Scope: global"));

        let events = read_events(temp_db.path());
        let started = events.iter().find_map(|event| match &event.payload {
            OrchestrationEventPayload::SupervisorQueryStarted(payload) => Some(payload),
            _ => None,
        });
        let finished = events.iter().find_map(|event| match &event.payload {
            OrchestrationEventPayload::SupervisorQueryFinished(payload) => Some(payload),
            _ => None,
        });

        let started = started.expect("expected started event");
        assert_eq!(started.scope, "global");
        assert!(events.iter().all(|event| event.work_item_id.is_none()));
        assert!(events.iter().all(|event| event.session_id.is_none()));

        let finished = finished.expect("expected finished event");
        assert_eq!(finished.finish_reason, LlmFinishReason::Stop);
        assert_eq!(
            finished.usage.as_ref().map(|usage| usage.total_tokens),
            Some(29)
        );
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_persists_lifecycle_events() {
        let temp_db = TestDbPath::new("app-supervisor-query-lifecycle");
        let supervisor = QueryingSupervisor::with_chunks(vec![
            LlmStreamChunk {
                delta: "Current activity: reviewing build output.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            },
            LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: Some(orchestrator_core::LlmTokenUsage {
                    input_tokens: 64,
                    output_tokens: 19,
                    total_tokens: 83,
                }),
                rate_limit: None,
            },
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status_current_session"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        while stream
            .next_chunk()
            .await
            .expect("poll lifecycle stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let started_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    OrchestrationEventPayload::SupervisorQueryStarted(_)
                )
            })
            .count();
        let chunk_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    OrchestrationEventPayload::SupervisorQueryChunk(_)
                )
            })
            .count();
        let finished = events.iter().find_map(|event| match &event.payload {
            OrchestrationEventPayload::SupervisorQueryFinished(payload) => Some(payload.clone()),
            _ => None,
        });

        assert_eq!(started_count, 1);
        assert!(chunk_count >= 1);
        let finished = finished.expect("finished lifecycle event");
        assert_eq!(finished.finish_reason, LlmFinishReason::Stop);
        assert_eq!(finished.chunk_count, 2);
        assert_eq!(finished.usage.map(|usage| usage.total_tokens), Some(83));
        assert!(
            events.iter().any(|event| {
                event
                    .work_item_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "wi-query")
                    && matches!(
                        event.payload,
                        OrchestrationEventPayload::SupervisorQueryStarted(_)
                    )
            }),
            "lifecycle events should carry work item identity for projection views"
        );
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_uses_invocation_context_when_present() {
        let temp_db = TestDbPath::new("app-supervisor-query-command-context");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                template_query_invocation_with_context(
                    "status",
                    SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: Some("sess-from-command".to_owned()),
                        scope: Some("session:sess-from-command".to_owned()),
                    },
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-fallback".to_owned()),
                    selected_session_id: None,
                    scope: Some("work_item:wi-fallback".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0]
            .messages
            .iter()
            .any(|message| message.content.contains("Scope: session:sess-from-command")));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_partial_invocation_context_overlays_fallback() {
        let temp_db = TestDbPath::new("app-supervisor-query-partial-context-overlay");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What should I do next?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: None,
                        selected_session_id: Some("sess-from-command".to_owned()),
                        scope: None,
                    }),
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-fallback".to_owned()),
                    selected_session_id: Some("sess-fallback".to_owned()),
                    scope: Some("session:sess-fallback".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Scope: session:sess-from-command"));
        assert!(requests[0].messages[1]
            .content
            .contains("- selected_work_item_id: wi-fallback"));
        assert!(requests[0].messages[1]
            .content
            .contains("No local ticket metadata is mapped for work item 'wi-fallback'"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_auto_resolves_blocking_intent_as_freeform() {
        let temp_db = TestDbPath::new("app-supervisor-query-freeform-blocking-template");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What is blocking this ticket?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: None,
                        scope: None,
                    }),
                ),
                SupervisorCommandContext::default(),
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Operator question:"));
        assert!(requests[0].messages[1]
            .content
            .contains("What is blocking this ticket?"));
        assert!(requests[0].messages[1].content.contains("Context pack:"));
        assert!(!requests[0].messages[1].content.contains("Template:"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_auto_resolves_status_and_planning_intents_as_freeform() {
        let temp_db = TestDbPath::new("app-supervisor-query-freeform-intent-templates");

        let cases = [
            "What's the status right now?",
            "What should we do next to unblock this?",
        ];

        for query in cases {
            let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }]);

            let app = App {
                config: AppConfig {
                    workspace: "/workspace".to_owned(),
                    event_store_path: temp_db.path().to_string_lossy().to_string(),
                    ticketing_provider: "ticketing.linear".to_owned(),
                    harness_provider: "harness.codex".to_owned(),
                    ..AppConfig::default()
                },
                ticketing: MockTicketingProvider::service(),
                supervisor: supervisor.clone(),
                github: Healthy,
            };

            let _ = app
                .dispatch_supervisor_command(
                    freeform_query_invocation_with_context(
                        query,
                        Some(SupervisorQueryContextArgs {
                            selected_work_item_id: Some("wi-from-command".to_owned()),
                            selected_session_id: None,
                            scope: None,
                        }),
                    ),
                    SupervisorCommandContext::default(),
                )
                .await
                .expect("dispatch should stream");

            let requests = supervisor.requests();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].messages[1].content.contains(query));
            assert!(!requests[0].messages[1].content.contains("Template:"));
        }
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_preserves_freeform_for_unmatched_questions() {
        let temp_db = TestDbPath::new("app-supervisor-query-freeform-fallback");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "Compare AP-101 and AP-102 delivery risk by owner.",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: None,
                        scope: None,
                    }),
                ),
                SupervisorCommandContext::default(),
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Operator question:\nCompare AP-101 and AP-102 delivery risk by owner."));
        assert!(!requests[0].messages[1].content.contains("Template:"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_rejects_unknown_template_with_guidance() {
        let temp_db = TestDbPath::new("app-supervisor-query-template-error");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };

        let err = match app
            .dispatch_supervisor_command(
                template_query_invocation("not-a-template"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: None,
                    scope: Some("work_item:wi-query".to_owned()),
                },
            )
            .await
        {
            Ok(_) => panic!("unknown template should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains("unknown supervisor template"));
        assert!(message.contains("current_activity"));
        assert!(message.contains("recommended_response"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_rejects_malformed_context_scope() {
        let temp_db = TestDbPath::new("app-supervisor-query-context-error");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };

        let err = match app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: None,
                    scope: Some("session:  ".to_owned()),
                },
            )
            .await
        {
            Ok(_) => panic!("malformed scope should fail"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("malformed context"));
        assert!(err.to_string().contains("scope identifier"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_context_from_invocation_replaces_fallback_context() {
        let temp_db = TestDbPath::new("app-supervisor-query-command-context-precedence");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What changed for AP-180?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: None,
                        scope: None,
                    }),
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-fallback".to_owned()),
                    selected_session_id: Some("sess-fallback".to_owned()),
                    scope: Some("session:sess-fallback".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Operator question:\nWhat changed for AP-180?"));
        assert!(requests[0].messages[1]
            .content
            .contains("Ticket status context:"));
        assert!(requests[0].messages[1]
            .content
            .contains("Scope: work_item:wi-from-command"));
        assert!(
            !requests[0].messages[1]
                .content
                .contains("Scope: session:sess-fallback"),
            "fallback scope should not leak when invocation context is present"
        );
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_rejects_non_supervisor_commands() {
        let temp_db = TestDbPath::new("app-supervisor-query-unsupported-command");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::UiFocusNextInbox)
            .expect("serialize zero-arg command");

        let err = match app
            .dispatch_supervisor_command(invocation, SupervisorCommandContext::default())
            .await
        {
            Ok(_) => panic!("unsupported command should fail"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(message.contains("unsupported command"));
        assert!(message.contains("ui.focus_next_inbox"));
        assert!(message.contains("supervisor.query"));
    }

    #[tokio::test]
    async fn supervisor_runtime_dispatch_supports_workflow_approve_pr_ready() {
        let temp_db = TestDbPath::new("app-runtime-command-approve-pr-ready");
        let work_item_id = WorkItemId::new("wi-approve-success");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        let pr_url = "https://github.com/acme/repo/pull/321";
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-approve-success",
            "artifact-pr-approve-success",
            pr_url,
        )
        .expect("seed runtime mapping");
        let host = MockCodeHost::default();
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: host.clone(),
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::WorkflowApprovePrReady)
            .expect("serialize workflow.approve_pr_ready");

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                invocation,
                SupervisorCommandContext {
                    selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                    selected_session_id: None,
                    scope: None,
                },
            )
            .await
            .expect("approve_pr_ready should be supported at runtime");
        assert!(stream_id.starts_with("runtime-"));

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected one chunk");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(
            first_chunk
                .delta
                .contains("workflow.approve_pr_ready command completed for PR #321"),
            "runtime command should return a completion message"
        );
        assert!(
            stream.next_chunk().await.expect("stream close").is_none(),
            "runtime command stream should terminate after acknowledgement"
        );

        let calls = host.ready_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].number, 321);
        assert_eq!(calls[0].url, pr_url);
        assert_eq!(calls[0].repository.name, "acme/repo");
        assert_eq!(calls[0].repository.id, "acme/repo");
    }

    #[tokio::test]
    async fn supervisor_runtime_dispatch_workflow_approve_pr_ready_requires_context() {
        let temp_db = TestDbPath::new("app-runtime-command-approve-pr-ready-missing-context");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::WorkflowApprovePrReady)
            .expect("serialize workflow.approve_pr_ready");

        let err = match app
            .dispatch_supervisor_command(invocation, SupervisorCommandContext::default())
            .await
        {
            Ok(_) => panic!("approve_pr_ready should fail without context"),
            Err(err) => err,
        };

        assert!(err
            .to_string()
            .contains("requires an active runtime session or selected work item context"));
    }

    #[tokio::test]
    async fn supervisor_runtime_dispatch_workflow_approve_pr_ready_propagates_host_errors() {
        let temp_db = TestDbPath::new("app-runtime-command-approve-pr-ready-host-error");
        let work_item_id = WorkItemId::new("wi-approve-error");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        let pr_url = "https://github.com/acme/repo/pull/512";
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-approve-error",
            "artifact-pr-approve-error",
            pr_url,
        )
        .expect("seed runtime mapping");

        let host = MockCodeHost::with_error("github API unavailable");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: host.clone(),
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::WorkflowApprovePrReady)
            .expect("serialize workflow.approve_pr_ready");

        let err = match app
            .dispatch_supervisor_command(
                invocation,
                SupervisorCommandContext {
                    selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                    selected_session_id: None,
                    scope: None,
                },
            )
            .await
        {
            Ok(_) => panic!("approve_pr_ready should fail when host returns an error"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains("workflow.approve_pr_ready failed for PR #512"));
        assert!(message.contains("github API unavailable"));

        let calls = host.ready_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].number, 512);
    }

    #[tokio::test]
    async fn supervisor_runtime_dispatch_workflow_approve_pr_ready_skips_invalid_pr_artifact_urls()
    {
        let temp_db = TestDbPath::new("app-runtime-command-approve-pr-ready-invalid-url");
        let work_item_id = WorkItemId::new("wi-approve-invalid-url");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-approve-invalid-url",
            "artifact-pr-approve-valid-1",
            "https://github.com/acme/repo/pull/777",
        )
        .expect("seed valid runtime mapping");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-approve-invalid-url",
            "artifact-pr-approve-invalid-1",
            "https://github.com/acme/repo/compare/777",
        )
        .expect("seed invalid runtime mapping");

        let host = MockCodeHost::default();
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: host.clone(),
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::WorkflowApprovePrReady)
            .expect("serialize workflow.approve_pr_ready");

        let (_stream_id, mut stream) = app
            .dispatch_supervisor_command(
                invocation,
                SupervisorCommandContext {
                    selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                    selected_session_id: None,
                    scope: None,
                },
            )
            .await
            .expect("approve_pr_ready should skip bad PR artifact");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected one chunk");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(
            first_chunk
                .delta
                .contains("workflow.approve_pr_ready command completed for PR #777"),
            "runtime command should use valid PR fallback artifact"
        );

        let calls = host.ready_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].number, 777);
        assert_eq!(calls[0].url, "https://github.com/acme/repo/pull/777");
    }

    #[tokio::test]
    async fn supervisor_runtime_dispatch_open_review_tabs_requires_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::GithubOpenReviewTabs)
            .expect("serialize github.open_review_tabs");

        let err = match app
            .dispatch_supervisor_command(invocation, SupervisorCommandContext::default())
            .await
        {
            Ok(_) => panic!("open_review_tabs should require context"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(
            message.contains("requires an active runtime session or selected work item context")
        );
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
    }

    #[tokio::test]
    async fn supervisor_runtime_dispatch_rejects_non_runtime_command_variants() {
        let temp_db = TestDbPath::new("app-runtime-command-unsupported-variants");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };

        let non_runtime = [
            Command::UiFocusNextInbox,
            Command::UiOpenTerminalForSelected,
            Command::UiOpenDiffInspectorForSelected,
            Command::UiOpenTestInspectorForSelected,
            Command::UiOpenPrInspectorForSelected,
            Command::UiOpenChatInspectorForSelected,
        ];

        for command in non_runtime {
            let invocation = CommandRegistry::default()
                .to_untyped_invocation(&command)
                .expect("serialize ui command");
            let err = match app
                .dispatch_supervisor_command(invocation, SupervisorCommandContext::default())
                .await
            {
                Ok(_) => panic!("ui command should remain unsupported"),
                Err(err) => err,
            };
            let message = err.to_string();
            assert!(message.contains("unsupported"));
            assert!(message.contains("expected"));
            assert!(message.contains(command.id()));
            assert!(message.contains("supervisor.query"));
            assert!(message.contains("workflow.approve_pr_ready"));
            assert!(message.contains("workflow.reconcile_pr_merge"));
            assert!(message.contains("workflow.merge_pr"));
            assert!(message.contains("github.open_review_tabs"));
        }
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_forwards_cancel_to_provider() {
        let temp_db = TestDbPath::new("app-supervisor-query-cancel");
        let supervisor = QueryingSupervisor::default();
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        app.cancel_supervisor_command("stream-cancel-7")
            .await
            .expect("cancel should succeed");

        assert_eq!(
            supervisor.cancelled_streams(),
            vec!["stream-cancel-7".to_owned()]
        );
    }

    #[tokio::test]
    async fn supervisor_query_cancel_records_user_initiated_cancellation() {
        let temp_db = TestDbPath::new("app-supervisor-query-user-cancel-record");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Cancelled),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        app.cancel_supervisor_command(stream_id.as_str())
            .await
            .expect("cancel should succeed");

        while stream
            .next_chunk()
            .await
            .expect("poll cancelled stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let saw_user_cancelled = events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::SupervisorQueryCancelled(payload)
                    if payload.source == SupervisorQueryCancellationSource::UserInitiated
            )
        });
        let saw_finished_user_cancelled = events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::SupervisorQueryFinished(payload)
                    if payload.finish_reason == LlmFinishReason::Cancelled
                        && payload.cancellation_source
                            == Some(SupervisorQueryCancellationSource::UserInitiated)
            )
        });

        assert!(saw_user_cancelled);
        assert!(saw_finished_user_cancelled);
    }

    #[tokio::test]
    async fn supervisor_query_cancel_records_single_user_cancel_event_for_retries() {
        let temp_db = TestDbPath::new("app-supervisor-query-user-cancel-deduped");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Cancelled),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        app.cancel_supervisor_command(stream_id.as_str())
            .await
            .expect("first cancel should succeed");
        app.cancel_supervisor_command(stream_id.as_str())
            .await
            .expect("second cancel should succeed");

        while stream
            .next_chunk()
            .await
            .expect("poll cancelled stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let user_cancelled_count = events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    OrchestrationEventPayload::SupervisorQueryCancelled(payload)
                        if payload.source == SupervisorQueryCancellationSource::UserInitiated
                )
            })
            .count();

        assert_eq!(user_cancelled_count, 1);
    }

    #[tokio::test]
    async fn supervisor_query_cancel_targets_latest_active_query_when_stream_ids_repeat() {
        let temp_db = TestDbPath::new("app-supervisor-query-stream-id-reuse");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Cancelled),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
                ticketing_provider: "ticketing.linear".to_owned(),
                harness_provider: "harness.codex".to_owned(),
                ..AppConfig::default()
            },
            ticketing: MockTicketingProvider::service(),
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id_first, mut stream_first) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-first".to_owned()),
                    selected_session_id: Some("sess-first".to_owned()),
                    scope: Some("session:sess-first".to_owned()),
                },
            )
            .await
            .expect("first dispatch should stream");

        let (stream_id_second, mut stream_second) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-second".to_owned()),
                    selected_session_id: Some("sess-second".to_owned()),
                    scope: Some("session:sess-second".to_owned()),
                },
            )
            .await
            .expect("second dispatch should stream");

        while stream_first
            .next_chunk()
            .await
            .expect("poll first stream")
            .is_some()
        {}

        app.cancel_supervisor_command(stream_id_second.as_str())
            .await
            .expect("cancel should target second active stream");

        while stream_second
            .next_chunk()
            .await
            .expect("poll second stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let saw_second_user_cancel = events.iter().any(|event| {
            event
                .work_item_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "wi-second")
                && matches!(
                    &event.payload,
                    OrchestrationEventPayload::SupervisorQueryCancelled(payload)
                        if payload.source == SupervisorQueryCancellationSource::UserInitiated
                )
        });

        assert!(saw_second_user_cancel);
    }
}
