#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppConfig;
    use crate::events::{
        ArtifactCreatedPayload, NewEventEnvelope, OrchestrationEventPayload,
        WorkflowTransitionPayload,
    };
    use crate::normalization::DOMAIN_EVENT_SCHEMA_VERSION;
    use orchestrator_domain::test_support::TestDbPath;
    use orchestrator_domain::{
        ArtifactId, ArtifactKind, ArtifactRecord, BackendKind, CodeHostKind, PullRequestCiStatus,
        PullRequestMergeState, PullRequestRef, PullRequestSummary, RepositoryRef, ReviewerRequest,
        RuntimeMappingRecord, SessionRecord, SqliteEventStore, TicketId, TicketProvider,
        TicketRecord, WorkItemId, WorkerSessionId, WorkerSessionStatus, WorkflowState, WorktreeId,
        WorktreeRecord,
    };
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn parse_pull_request_number_supports_query_and_fragment_suffix() {
        let number =
            parse_pull_request_number("https://github.com/acme/repo/pull/123?draft=true#section")
                .expect("parse numeric suffix");
        assert_eq!(number, 123);
    }

    #[test]
    fn parse_pull_request_number_rejects_missing_pull_segment() {
        let err = parse_pull_request_number("https://github.com/acme/repo/issues/123")
            .expect_err("missing /pull");
        assert!(matches!(err, CoreError::Configuration { .. }));
    }

    #[test]
    fn parse_pull_request_repository_parses_without_scheme() {
        let (name, id) = parse_pull_request_repository("github.com/acme/repo/pull/777")
            .expect("parse repo without scheme");
        assert_eq!(name, "acme/repo");
        assert_eq!(id, "acme/repo");
    }

    #[test]
    fn parse_pull_request_repository_rejects_short_path() {
        let err = parse_pull_request_repository("https://github.com/pull/777")
            .expect_err("repository name missing");
        assert!(matches!(err, CoreError::Configuration { .. }));
    }

    #[test]
    fn parse_pull_request_repository_trims_surrounding_punctuation() {
        let (name, id) =
            parse_pull_request_repository("(\"https://github.com/acme/repo/pull/777\",)")
                .expect("parse url with punctuation");
        assert_eq!(name, "acme/repo");
        assert_eq!(id, "acme/repo");
    }

    #[test]
    fn sanitize_error_display_text_strips_control_characters() {
        let sanitized = sanitize_error_display_text("workflow merge\0 failed\u{2400}\r\n");
        assert_eq!(sanitized, "workflow merge failed\n\n");
    }

    #[test]
    fn supervisor_context_adapter_preserves_fields() {
        let context = SupervisorCommandContext {
            selected_work_item_id: Some("wi-77".to_owned()),
            selected_session_id: Some("sess-77".to_owned()),
            scope: Some("session:sess-77".to_owned()),
        };

        let domain_context = to_domain_supervisor_query_context(&context);

        assert_eq!(
            domain_context.selected_work_item_id,
            context.selected_work_item_id
        );
        assert_eq!(domain_context.selected_session_id, context.selected_session_id);
        assert_eq!(domain_context.scope, context.scope);
    }

    struct MockUrlOpener {
        calls: Mutex<Vec<String>>,
    }

    impl MockUrlOpener {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock calls").clone()
        }
    }

    impl Default for MockUrlOpener {
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl UrlOpener for MockUrlOpener {
        async fn open_url(&self, url: &str) -> Result<(), CoreError> {
            self.calls.lock().expect("lock calls").push(url.to_owned());
            Ok(())
        }
    }

    struct FailingUrlOpener;

    #[async_trait::async_trait]
    impl UrlOpener for FailingUrlOpener {
        async fn open_url(&self, _url: &str) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "open failed intentionally".to_owned(),
            ))
        }
    }

    struct MockCodeHost {
        fallback_pr: Option<PullRequestRef>,
        fallback_calls: Mutex<Vec<(String, String)>>,
        ready_calls: Mutex<Vec<PullRequestRef>>,
        merge_state: PullRequestMergeState,
        merge_state_error: Option<String>,
        ready_error: Option<String>,
        merge_error: Option<String>,
        ci_statuses: Vec<PullRequestCiStatus>,
        ci_status_error: Option<String>,
    }

    impl MockCodeHost {
        fn new(fallback_pr: Option<PullRequestRef>) -> Self {
            Self {
                fallback_pr,
                fallback_calls: Mutex::new(Vec::new()),
                ready_calls: Mutex::new(Vec::new()),
                merge_state: PullRequestMergeState {
                    merged: false,
                    is_draft: true,
                    state: None,
                    review_decision: None,
                    review_summary: None,
                    merge_conflict: false,
                    base_branch: None,
                    head_branch: None,
                },
                merge_state_error: None,
                ready_error: None,
                merge_error: None,
                ci_statuses: Vec::new(),
                ci_status_error: None,
            }
        }

        fn with_merge_state(
            fallback_pr: Option<PullRequestRef>,
            merge_state: PullRequestMergeState,
        ) -> Self {
            Self {
                fallback_pr,
                fallback_calls: Mutex::new(Vec::new()),
                ready_calls: Mutex::new(Vec::new()),
                merge_state,
                merge_state_error: None,
                ready_error: None,
                merge_error: None,
                ci_statuses: Vec::new(),
                ci_status_error: None,
            }
        }

        fn with_merge_error(
            fallback_pr: Option<PullRequestRef>,
            merge_state: PullRequestMergeState,
            merge_error: impl Into<String>,
        ) -> Self {
            Self {
                fallback_pr,
                fallback_calls: Mutex::new(Vec::new()),
                ready_calls: Mutex::new(Vec::new()),
                merge_state,
                merge_state_error: None,
                ready_error: None,
                merge_error: Some(merge_error.into()),
                ci_statuses: Vec::new(),
                ci_status_error: None,
            }
        }

        fn with_ready_error(
            fallback_pr: Option<PullRequestRef>,
            merge_state: PullRequestMergeState,
            ready_error: impl Into<String>,
        ) -> Self {
            Self {
                fallback_pr,
                fallback_calls: Mutex::new(Vec::new()),
                ready_calls: Mutex::new(Vec::new()),
                merge_state,
                merge_state_error: None,
                ready_error: Some(ready_error.into()),
                merge_error: None,
                ci_statuses: Vec::new(),
                ci_status_error: None,
            }
        }

        fn with_merge_state_and_merge_error(
            fallback_pr: Option<PullRequestRef>,
            merge_state_error: impl Into<String>,
            merge_error: impl Into<String>,
        ) -> Self {
            Self {
                fallback_pr,
                fallback_calls: Mutex::new(Vec::new()),
                ready_calls: Mutex::new(Vec::new()),
                merge_state: PullRequestMergeState {
                    merged: false,
                    is_draft: false,
                    state: None,
                    review_decision: None,
                    review_summary: None,
                    merge_conflict: false,
                    base_branch: None,
                    head_branch: None,
                },
                merge_state_error: Some(merge_state_error.into()),
                ready_error: None,
                merge_error: Some(merge_error.into()),
                ci_statuses: Vec::new(),
                ci_status_error: None,
            }
        }

        fn with_ci_statuses(mut self, ci_statuses: Vec<PullRequestCiStatus>) -> Self {
            self.ci_statuses = ci_statuses;
            self
        }

        fn fallback_calls(&self) -> Vec<(String, String)> {
            self.fallback_calls
                .lock()
                .expect("fallback calls lock")
                .clone()
        }

        fn ready_calls(&self) -> Vec<PullRequestRef> {
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
            _request: orchestrator_domain::CreatePullRequestRequest,
        ) -> Result<PullRequestSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in test mock".to_owned(),
            ))
        }

        async fn mark_ready_for_review(&self, pr: &PullRequestRef) -> Result<(), CoreError> {
            self.ready_calls
                .lock()
                .expect("ready calls lock")
                .push(pr.clone());
            if let Some(message) = &self.ready_error {
                return Err(CoreError::DependencyUnavailable(message.clone()));
            }
            Ok(())
        }

        async fn request_reviewers(
            &self,
            _pr: &PullRequestRef,
            _reviewers: ReviewerRequest,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn list_waiting_for_my_review(&self) -> Result<Vec<PullRequestSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn get_pull_request_merge_state(
            &self,
            _pr: &PullRequestRef,
        ) -> Result<PullRequestMergeState, CoreError> {
            if let Some(message) = &self.merge_state_error {
                return Err(CoreError::DependencyUnavailable(message.clone()));
            }
            Ok(self.merge_state.clone())
        }

        async fn merge_pull_request(&self, _pr: &PullRequestRef) -> Result<(), CoreError> {
            if let Some(message) = &self.merge_error {
                return Err(CoreError::DependencyUnavailable(message.clone()));
            }
            Ok(())
        }

        async fn list_pull_request_ci_statuses(
            &self,
            _pr: &PullRequestRef,
        ) -> Result<Vec<PullRequestCiStatus>, CoreError> {
            if let Some(message) = &self.ci_status_error {
                return Err(CoreError::DependencyUnavailable(message.clone()));
            }
            Ok(self.ci_statuses.clone())
        }

        async fn find_open_pull_request_for_branch(
            &self,
            repository: &RepositoryRef,
            head_branch: &str,
        ) -> Result<Option<PullRequestRef>, CoreError> {
            self.fallback_calls
                .lock()
                .expect("fallback calls lock")
                .push((
                    repository.root.to_string_lossy().to_string(),
                    head_branch.to_owned(),
                ));
            Ok(self.fallback_pr.clone())
        }
    }

    async fn execute_github_open_review_tabs_with_opener(
        event_store_path: &str,
        context: SupervisorCommandContext,
        url_opener: &impl UrlOpener,
    ) -> Result<(String, LlmResponseStream), CoreError> {
        let code_host = MockCodeHost::new(None);
        let database_runtime_config = AppConfig::default().database_runtime();
        super::execute_github_open_review_tabs_with_opener(
            &code_host,
            event_store_path,
            &database_runtime_config,
            context,
            url_opener,
        )
        .await
    }

    fn seed_runtime_mapping(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
    ) -> Result<(), CoreError> {
        let ticket = TicketRecord {
            ticket_id: TicketId::from_provider_uuid(
                TicketProvider::Linear,
                format!("provider-{}", work_item_id.as_str()),
            ),
            provider: TicketProvider::Linear,
            provider_ticket_id: format!("provider-{}", work_item_id.as_str()),
            identifier: format!("ORCH-{}", work_item_id.as_str()),
            title: "Open review test".to_owned(),
            state: "In Progress".to_owned(),
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        };

        let worktree_path =
            std::path::PathBuf::from(format!("/workspace/{}", work_item_id.as_str()));
        let runtime = RuntimeMappingRecord {
            ticket,
            work_item_id: work_item_id.clone(),
            worktree: WorktreeRecord {
                worktree_id: WorktreeId::new(format!("wt-{}", work_item_id.as_str())),
                work_item_id: work_item_id.clone(),
                path: worktree_path.to_string_lossy().to_string(),
                branch: "feature/open-review-tabs".to_owned(),
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
        Ok(())
    }

    fn seed_runtime_mapping_with_artifact(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
        artifact_id: &str,
        kind: ArtifactKind,
        pr_url: &str,
    ) -> Result<(), CoreError> {
        seed_runtime_mapping(store, work_item_id, session_id)?;
        let artifact = ArtifactRecord {
            artifact_id: ArtifactId::new(artifact_id),
            work_item_id: work_item_id.clone(),
            kind: kind.clone(),
            metadata: json!({"type": "pull_request"}),
            storage_ref: pr_url.to_owned(),
            created_at: "2026-02-16T11:01:00Z".to_owned(),
        };
        store.create_artifact(&artifact)?;
        let event = OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
            artifact_id: artifact.artifact_id.clone(),
            work_item_id: work_item_id.clone(),
            kind,
            label: "Pull request".to_owned(),
            uri: pr_url.to_owned(),
        });
        store.append_event(
            NewEventEnvelope {
                event_id: format!("evt-pr-artifact-{}", artifact_id),
                occurred_at: "2026-02-16T11:02:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(WorkerSessionId::new(session_id)),
                payload: event,
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            },
            &[artifact.artifact_id.clone()],
        )?;
        Ok(())
    }

    fn seed_runtime_mapping_with_pr_artifact(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
        artifact_id: &str,
        pr_url: &str,
    ) -> Result<(), CoreError> {
        seed_runtime_mapping_with_artifact(
            store,
            work_item_id,
            session_id,
            artifact_id,
            ArtifactKind::PR,
            pr_url,
        )
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_opens_pr_url_for_session_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-session");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-session");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-session",
            "artifact-pr-review-session",
            "https://github.com/acme/repo/pull/123",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: None,
                selected_session_id: Some("sess-open-review-tabs-session".to_owned()),
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk.delta.contains("github.open_review_tabs"));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/123"));
        assert!(stream
            .next_chunk()
            .await
            .expect("read runtime close")
            .is_none());
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/123".to_owned()]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_opens_pr_url_for_work_item_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-work-item");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-work-item");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-work-item",
            "artifact-pr-review-work-item",
            "https://github.com/acme/repo/pull/124",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/124"));
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/124".to_owned()]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_accepts_pr_link_artifacts() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-link-artifact");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-link-artifact");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-link-artifact",
            "artifact-pr-link-review",
            ArtifactKind::Link,
            "https://github.com/acme/repo/pull/321",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/321"));
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/321".to_owned()]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_reopens_url_per_invocation() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-reopen");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-reopen");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-reopen",
            "artifact-pr-review-reopen",
            "https://github.com/acme/repo/pull/125",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let context = SupervisorCommandContext {
            selected_work_item_id: Some(work_item_id.as_str().to_owned()),
            selected_session_id: None,
            scope: None,
        };
        let event_store_path = temp_db.path().to_str().expect("path").to_owned();

        let _ = execute_github_open_review_tabs_with_opener(
            &event_store_path,
            context.clone(),
            &opener,
        )
        .await
        .expect("first invocation");
        let _ = execute_github_open_review_tabs_with_opener(
            &event_store_path,
            context.clone(),
            &opener,
        )
        .await
        .expect("second invocation");

        assert_eq!(
            opener.calls(),
            vec![
                "https://github.com/acme/repo/pull/125".to_owned(),
                "https://github.com/acme/repo/pull/125".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_rejects_missing_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-missing-context");
        let opener = MockUrlOpener::default();

        let err = match execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext::default(),
            &opener,
        )
        .await
        {
            Ok(_) => panic!("missing context should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
        assert!(
            message.contains("requires an active runtime session or selected work item context")
        );
        assert!(opener.calls().is_empty());
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_rejects_missing_pr_artifact() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-missing-pr");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-missing-pr");
        seed_runtime_mapping(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-missing-pr",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let err = match execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        {
            Ok(_) => panic!("missing artifact should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
        assert!(message.contains("could not resolve a PR artifact"));
        assert!(opener.calls().is_empty());
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_propagates_opener_errors() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-opener-error");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-opener-error");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-opener-error",
            "artifact-pr-review-opener-error",
            "https://github.com/acme/repo/pull/126",
        )
        .expect("seed mapping");

        let opener = FailingUrlOpener;
        let err = match execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        {
            Ok(_) => panic!("opener failure should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains("open failed intentionally"));
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_lazy_resolves_pr_via_code_host_fallback() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-fallback");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-fallback");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, "sess-open-review-tabs-fallback")
            .expect("seed mapping");

        let code_host = MockCodeHost::new(Some(PullRequestRef {
            repository: RepositoryRef {
                id: "acme/repo".to_owned(),
                name: "acme/repo".to_owned(),
                root: std::path::PathBuf::from("/workspace/wi-open-review-tabs-fallback"),
            },
            number: 333,
            url: "https://github.com/acme/repo/pull/333".to_owned(),
        }));
        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = super::execute_github_open_review_tabs_with_opener(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/333"));
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/333".to_owned()]
        );
        assert_eq!(code_host.fallback_calls().len(), 1);

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let runtime = resolve_runtime_mapping_for_context_with_command(
            &persisted_store,
            &SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            command_ids::GITHUB_OPEN_REVIEW_TABS,
        )
        .expect("runtime mapping");
        let resolved = resolve_pull_request_for_mapping_with_command(
            &persisted_store,
            &runtime,
            command_ids::GITHUB_OPEN_REVIEW_TABS,
        )
        .expect("fallback should persist PR artifact");
        assert_eq!(resolved.number, 333);
    }

    #[tokio::test]
    async fn execute_workflow_reconcile_pr_merge_lazy_resolves_pr_via_code_host_fallback() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-pr-fallback");
        let work_item_id = WorkItemId::new("wi-reconcile-pr-fallback");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, "sess-reconcile-pr-fallback")
            .expect("seed mapping");

        let code_host = MockCodeHost::new(Some(PullRequestRef {
            repository: RepositoryRef {
                id: "acme/repo".to_owned(),
                name: "acme/repo".to_owned(),
                root: std::path::PathBuf::from("/workspace/wi-reconcile-pr-fallback"),
            },
            number: 444,
            url: "https://github.com/acme/repo/pull/444".to_owned(),
        }));

        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
        )
        .await
        .expect("reconcile should succeed with fallback");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains(command_ids::WORKFLOW_RECONCILE_PR_MERGE));
        assert_eq!(code_host.fallback_calls().len(), 1);

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let runtime = resolve_runtime_mapping_for_context_with_command(
            &persisted_store,
            &SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            command_ids::WORKFLOW_RECONCILE_PR_MERGE,
        )
        .expect("runtime mapping");
        let resolved = resolve_pull_request_for_mapping_with_command(
            &persisted_store,
            &runtime,
            command_ids::WORKFLOW_RECONCILE_PR_MERGE,
        )
        .expect("fallback should persist PR artifact");
        assert_eq!(resolved.number, 444);
    }

    #[tokio::test]
    async fn execute_workflow_reconcile_pr_merge_returns_not_resolved_response_when_pr_missing() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-pr-missing");
        let work_item_id = WorkItemId::new("wi-reconcile-pr-missing");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, "sess-reconcile-pr-missing")
            .expect("seed mapping");

        let code_host = MockCodeHost::new(None);
        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some("sess-reconcile-pr-missing".to_owned()),
                scope: Some("session:sess-reconcile-pr-missing".to_owned()),
            },
        )
        .await
        .expect("reconcile should succeed even when PR cannot be resolved");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse reconcile response");
        assert_eq!(parsed["pr_resolved"], false);
        assert_eq!(parsed["completed"], false);
        assert_eq!(parsed["merged"], false);
        assert_eq!(parsed["merge_conflict"], false);
        assert_eq!(parsed["pr_state"], serde_json::Value::Null);
        assert_eq!(parsed["pr_is_draft"], false);
        assert_eq!(parsed["review_decision"], serde_json::Value::Null);
        assert_eq!(parsed["review_summary"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn execute_workflow_reconcile_pr_merge_includes_ci_failure_details() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-pr-ci-failures");
        let work_item_id = WorkItemId::new("wi-reconcile-pr-ci-failures");
        let session_id = WorkerSessionId::new("sess-reconcile-pr-ci-failures");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");

        let code_host = MockCodeHost::with_merge_state(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-reconcile-pr-ci-failures"),
                },
                number: 888,
                url: "https://github.com/acme/repo/pull/888".to_owned(),
            }),
            PullRequestMergeState {
                merged: false,
                is_draft: false,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-888-fix".to_owned()),
            },
        )
        .with_ci_statuses(vec![
            PullRequestCiStatus {
                name: "build".to_owned(),
                workflow: Some("Build".to_owned()),
                bucket: "pass".to_owned(),
                state: "SUCCESS".to_owned(),
                link: None,
            },
            PullRequestCiStatus {
                name: "tests".to_owned(),
                workflow: Some("Tests".to_owned()),
                bucket: "fail".to_owned(),
                state: "FAILURE".to_owned(),
                link: Some("https://github.com/acme/repo/actions/runs/123".to_owned()),
            },
        ]);

        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("reconcile should include CI details");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse reconcile response");
        assert_eq!(parsed["ci_has_failures"], true);
        assert_eq!(parsed["ci_statuses"].as_array().map(Vec::len), Some(2));
        assert_eq!(parsed["ci_failures"][0], "Tests / tests");
        assert_eq!(parsed["pr_state"], serde_json::Value::Null);
        assert_eq!(parsed["pr_is_draft"], false);
    }

    #[tokio::test]
    async fn reconcile_pr_merge_progresses_awaiting_review_to_done_after_merge() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-awaiting-review-to-done");
        let work_item_id = WorkItemId::new("wi-reconcile-awaiting-review");
        let session_id = WorkerSessionId::new("sess-reconcile-awaiting-review");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-awaiting-review-seed".to_owned(),
                occurred_at: "2026-02-19T00:00:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: None,
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed awaiting-review workflow transition");

        let code_host = MockCodeHost::with_merge_state(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-reconcile-awaiting-review"),
                },
                number: 445,
                url: "https://github.com/acme/repo/pull/445".to_owned(),
            }),
            PullRequestMergeState {
                merged: true,
                is_draft: false,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-445-fix".to_owned()),
            },
        );

        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("reconcile should transition merged review flow to done");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse reconcile response");
        assert_eq!(parsed["completed"], true);

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let events = persisted_store.read_ordered().expect("read events");
        let latest_state = events
            .iter()
            .filter_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if event.work_item_id.as_ref() == Some(&work_item_id) =>
                {
                    Some(payload.to.clone())
                }
                _ => None,
            })
            .last()
            .expect("workflow transitions recorded");
        assert_eq!(latest_state, WorkflowState::Done);
    }

    #[tokio::test]
    async fn workflow_merge_pr_marks_draft_ready_before_merge_attempt() {
        let temp_db = TestDbPath::new("app-runtime-command-merge-pr-draft-auto-ready");
        let work_item_id = WorkItemId::new("wi-merge-pr-draft-auto-ready");
        let session_id = WorkerSessionId::new("sess-merge-pr-draft-auto-ready");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-awaiting-review-seed".to_owned(),
                occurred_at: "2026-02-19T00:00:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: None,
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed awaiting-review workflow transition");

        let code_host = MockCodeHost::with_merge_state(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-merge-pr-draft-auto-ready"),
                },
                number: 611,
                url: "https://github.com/acme/repo/pull/611".to_owned(),
            }),
            PullRequestMergeState {
                merged: false,
                is_draft: true,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-611-fix".to_owned()),
            },
        );

        let (_stream_id, mut stream) = super::execute_workflow_merge_pr(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("merge command should return runtime output payload");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse merge response");
        assert_eq!(parsed["completed"], false);
        assert_eq!(code_host.ready_calls().len(), 1);
        assert_eq!(code_host.ready_calls()[0].number, 611);
    }

    #[tokio::test]
    async fn workflow_merge_pr_skips_ready_conversion_when_pr_is_not_draft() {
        let temp_db = TestDbPath::new("app-runtime-command-merge-pr-ready-skip");
        let work_item_id = WorkItemId::new("wi-merge-pr-ready-skip");
        let session_id = WorkerSessionId::new("sess-merge-pr-ready-skip");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-awaiting-review-seed".to_owned(),
                occurred_at: "2026-02-19T00:00:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: None,
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed awaiting-review workflow transition");

        let code_host = MockCodeHost::with_merge_state(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-merge-pr-ready-skip"),
                },
                number: 612,
                url: "https://github.com/acme/repo/pull/612".to_owned(),
            }),
            PullRequestMergeState {
                merged: false,
                is_draft: false,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-612-fix".to_owned()),
            },
        );

        let (_stream_id, mut stream) = super::execute_workflow_merge_pr(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("merge command should return runtime output payload");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse merge response");
        assert_eq!(parsed["completed"], false);
        assert!(code_host.ready_calls().is_empty());
    }

    #[tokio::test]
    async fn workflow_merge_pr_rolls_back_to_in_review_when_mark_ready_fails() {
        let temp_db = TestDbPath::new("app-runtime-command-merge-pr-ready-failure");
        let work_item_id = WorkItemId::new("wi-merge-pr-ready-failure");
        let session_id = WorkerSessionId::new("sess-merge-pr-ready-failure");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-awaiting-review-seed".to_owned(),
                occurred_at: "2026-02-19T00:00:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: None,
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed awaiting-review workflow transition");

        let code_host = MockCodeHost::with_ready_error(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-merge-pr-ready-failure"),
                },
                number: 613,
                url: "https://github.com/acme/repo/pull/613".to_owned(),
            }),
            PullRequestMergeState {
                merged: false,
                is_draft: true,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-613-fix".to_owned()),
            },
            "ready conversion blocked by policy",
        );

        let (_stream_id, mut stream) = super::execute_workflow_merge_pr(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("merge command should return runtime output payload");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse merge response");
        assert_eq!(parsed["completed"], false);
        assert!(parsed["error"]
            .as_str()
            .expect("error message")
            .contains("failed to mark draft PR #613 ready for review"));

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let events = persisted_store.read_ordered().expect("read events");
        let transition_targets = events
            .iter()
            .filter_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if event.work_item_id.as_ref() == Some(&work_item_id) =>
                {
                    Some(payload.to.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(transition_targets.contains(&WorkflowState::PendingMerge));
        assert_eq!(transition_targets.last(), Some(&WorkflowState::InReview));
    }

    #[tokio::test]
    async fn workflow_merge_pr_attempts_merge_when_merge_state_probe_fails() {
        let temp_db = TestDbPath::new("app-runtime-command-merge-pr-merge-state-probe-failure");
        let work_item_id = WorkItemId::new("wi-merge-pr-merge-state-probe-failure");
        let session_id = WorkerSessionId::new("sess-merge-pr-merge-state-probe-failure");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-awaiting-review-seed".to_owned(),
                occurred_at: "2026-02-19T00:00:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: None,
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed awaiting-review workflow transition");

        let code_host = MockCodeHost::with_merge_state_and_merge_error(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from(
                        "/workspace/wi-merge-pr-merge-state-probe-failure",
                    ),
                },
                number: 614,
                url: "https://github.com/acme/repo/pull/614".to_owned(),
            }),
            "pre-merge state probe unavailable",
            "merge blocked by required checks",
        );

        let (_stream_id, mut stream) = super::execute_workflow_merge_pr(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("merge command should return runtime output payload");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse merge response");
        assert_eq!(parsed["completed"], false);
        assert!(parsed["error"]
            .as_str()
            .expect("error message")
            .contains("merge blocked by required checks"));
    }

    #[tokio::test]
    async fn workflow_merge_pr_rolls_back_to_in_review_when_merge_fails() {
        let temp_db = TestDbPath::new("app-runtime-command-merge-pr-rollback-on-failure");
        let work_item_id = WorkItemId::new("wi-merge-pr-rollback-on-failure");
        let session_id = WorkerSessionId::new("sess-merge-pr-rollback-on-failure");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-awaiting-review-seed".to_owned(),
                occurred_at: "2026-02-19T00:00:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: None,
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed awaiting-review workflow transition");

        let code_host = MockCodeHost::with_merge_error(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-merge-pr-rollback-on-failure"),
                },
                number: 446,
                url: "https://github.com/acme/repo/pull/446".to_owned(),
            }),
            PullRequestMergeState {
                merged: false,
                is_draft: false,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-446-fix".to_owned()),
            },
            "merge blocked by required checks",
        );

        let (_stream_id, mut stream) = super::execute_workflow_merge_pr(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("merge command should return runtime output payload");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse merge response");
        assert_eq!(parsed["completed"], false);
        assert!(parsed["error"]
            .as_str()
            .expect("error message")
            .contains("merge blocked by required checks"));

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let events = persisted_store.read_ordered().expect("read events");
        let transition_targets = events
            .iter()
            .filter_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if event.work_item_id.as_ref() == Some(&work_item_id) =>
                {
                    Some(payload.to.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(transition_targets.contains(&WorkflowState::PendingMerge));
        assert_eq!(transition_targets.last(), Some(&WorkflowState::InReview));
    }

    #[tokio::test]
    async fn reconcile_pr_merge_progresses_merging_to_done_after_merge() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-merging-to-done");
        let work_item_id = WorkItemId::new("wi-reconcile-merging");
        let session_id = WorkerSessionId::new("sess-reconcile-merging");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");
        store
            .append(NewEventEnvelope {
                event_id: "evt-merging-seed".to_owned(),
                occurred_at: "2026-02-19T00:10:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: work_item_id.clone(),
                    from: WorkflowState::InReview,
                    to: WorkflowState::PendingMerge,
                    reason: Some(WorkflowTransitionReason::MergeInitiated),
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })
            .expect("seed merging workflow transition");

        let code_host = MockCodeHost::with_merge_state(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-reconcile-merging"),
                },
                number: 447,
                url: "https://github.com/acme/repo/pull/447".to_owned(),
            }),
            PullRequestMergeState {
                merged: true,
                is_draft: false,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-447-fix".to_owned()),
            },
        );

        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("reconcile should transition merged merging flow to done");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse reconcile response");
        assert_eq!(parsed["completed"], true);
        assert!(parsed["transitions"]
            .as_array()
            .expect("transitions")
            .iter()
            .any(|transition| transition == "PendingMerge->Done"));

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let events = persisted_store.read_ordered().expect("read events");
        let latest_state = events
            .iter()
            .filter_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if event.work_item_id.as_ref() == Some(&work_item_id) =>
                {
                    Some(payload.to.clone())
                }
                _ => None,
            })
            .last()
            .expect("workflow transitions recorded");
        assert_eq!(latest_state, WorkflowState::Done);
    }

    #[tokio::test]
    async fn reconcile_pr_merge_forces_planning_state_to_done_after_merge() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-planning-to-done");
        let work_item_id = WorkItemId::new("wi-reconcile-planning");
        let session_id = WorkerSessionId::new("sess-reconcile-planning");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, session_id.as_str()).expect("seed mapping");

        let code_host = MockCodeHost::with_merge_state(
            Some(PullRequestRef {
                repository: RepositoryRef {
                    id: "acme/repo".to_owned(),
                    name: "acme/repo".to_owned(),
                    root: std::path::PathBuf::from("/workspace/wi-reconcile-planning"),
                },
                number: 446,
                url: "https://github.com/acme/repo/pull/446".to_owned(),
            }),
            PullRequestMergeState {
                merged: true,
                is_draft: false,
                state: None,
                review_decision: None,
                review_summary: None,
                merge_conflict: false,
                base_branch: Some("main".to_owned()),
                head_branch: Some("ap/AP-446-fix".to_owned()),
            },
        );

        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            &AppConfig::default().database_runtime(),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: Some(session_id.as_str().to_owned()),
                scope: Some(format!("session:{}", session_id.as_str())),
            },
        )
        .await
        .expect("reconcile should force merged planning flow to done");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        let parsed: serde_json::Value =
            serde_json::from_str(first_chunk.delta.as_str()).expect("parse reconcile response");
        assert_eq!(parsed["completed"], true);
        assert_eq!(
            parsed["completion_mode"],
            serde_json::Value::String("forced_from_non_review_state".to_owned())
        );

        let persisted_store =
            open_event_store(temp_db.path().to_str().expect("path"), &AppConfig::default().database_runtime()).expect("reopen store");
        let events = persisted_store.read_ordered().expect("read events");
        let latest_state = events
            .iter()
            .filter_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if event.work_item_id.as_ref() == Some(&work_item_id) =>
                {
                    Some(payload.to.clone())
                }
                _ => None,
            })
            .last()
            .expect("workflow transitions recorded");
        assert_eq!(latest_state, WorkflowState::Done);
    }
}
