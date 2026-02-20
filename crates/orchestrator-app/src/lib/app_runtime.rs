pub struct App<S: Supervisor, G: GithubClient> {
    pub config: AppConfig,
    pub ticketing: Arc<dyn TicketingProvider + Send + Sync>,
    pub supervisor: S,
    pub github: G,
}

impl<S: Supervisor, G: GithubClient> App<S, G> {
    fn latest_workflow_state_for_work_item(
        store: &SqliteEventStore,
        work_item_id: &WorkItemId,
    ) -> Result<WorkflowState, CoreError> {
        let mut current = WorkflowState::New;
        let events = store.read_ordered()?;
        for event in events {
            if event.work_item_id.as_ref() != Some(work_item_id) {
                continue;
            }
            if let OrchestrationEventPayload::WorkflowTransition(payload) = event.payload {
                current = payload.to;
            }
        }
        Ok(current)
    }

    fn workflow_advance_target(
        from: &WorkflowState,
    ) -> Result<
        (
            WorkflowState,
            WorkflowTransitionReason,
            WorkflowGuardContext,
            Option<&'static str>,
        ),
        CoreError,
    > {
        match from {
            WorkflowState::New => Ok((
                WorkflowState::Planning,
                WorkflowTransitionReason::TicketAccepted,
                WorkflowGuardContext::default(),
                Some("Workflow transition approved: New -> Planning. Begin planning mode for this ticket and produce a concrete implementation plan before coding."),
            )),
            WorkflowState::Planning => Ok((
                WorkflowState::Implementing,
                WorkflowTransitionReason::PlanCommitted,
                WorkflowGuardContext {
                    has_active_session: true,
                    plan_ready: true,
                    ..WorkflowGuardContext::default()
                },
                Some("Workflow transition approved: Planning -> Implementing. End planning mode and begin implementation in this worktree now. Before moving out of implementation, run the full test suite for this repository and verify it passes."),
            )),
            WorkflowState::Implementing | WorkflowState::Testing => Ok((
                WorkflowState::PRDrafted,
                WorkflowTransitionReason::DraftPullRequestCreated,
                WorkflowGuardContext {
                    tests_passed: true,
                    has_draft_pr: true,
                    ..WorkflowGuardContext::default()
                },
                Some("Workflow transition approved: Implementation/Testing -> PR Drafted. Pause implementation, run the build and fix all errors and warnings, run the full test suite and verify it passes, then open a GitHub PR using the gh CLI and provide a review-ready summary with PR link, evidence, tests, and open risks."),
            )),
            WorkflowState::PRDrafted => Ok((
                WorkflowState::AwaitingYourReview,
                WorkflowTransitionReason::AwaitingApproval,
                WorkflowGuardContext::default(),
                Some("Workflow transition approved: PR Drafted -> Awaiting Your Review. Keep the PR and branch up to date while awaiting review and merge."),
            )),
            WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview
            | WorkflowState::Merging => Err(CoreError::Configuration(
                "workflow advance in review stages is merge-driven; use merge confirm/reconcile"
                    .to_owned(),
            )),
            WorkflowState::Done | WorkflowState::Abandoned => Err(CoreError::Configuration(
                "workflow is already complete and cannot be advanced".to_owned(),
            )),
        }
    }

    pub async fn startup_state(&self) -> Result<StartupState, CoreError> {
        self.supervisor.health_check().await?;
        self.github.health_check().await?;

        let store = open_event_store(&self.config.event_store_path)?;
        let events = store.read_ordered()?;
        let projection = rebuild_projection(&events);

        Ok(StartupState {
            status: format!("ready ({})", self.config.workspace),
            projection,
        })
    }

    pub async fn start_linear_polling(
        &self,
        linear_ticketing_provider: Option<&LinearTicketingProvider>,
    ) -> Result<(), CoreError> {
        if let Some(provider) = linear_ticketing_provider {
            provider.start_polling().await?;
        }
        Ok(())
    }

    pub async fn stop_linear_polling(
        &self,
        linear_ticketing_provider: Option<&LinearTicketingProvider>,
    ) -> Result<(), CoreError> {
        if let Some(provider) = linear_ticketing_provider {
            provider.stop_polling().await?;
        }
        Ok(())
    }

    pub async fn start_or_resume_selected_ticket(
        &self,
        selected_ticket: &TicketSummary,
        repository_override: Option<PathBuf>,
        vcs: &dyn VcsProvider,
        worker_backend: &dyn WorkerBackend,
    ) -> Result<SelectedTicketFlowResult, CoreError> {
        let mut store = open_event_store(&self.config.event_store_path)?;
        let flow_config = SelectedTicketFlowConfig::from_workspace_root(&self.config.workspace);

        orchestrator_core::start_or_resume_selected_ticket(
            &mut store,
            selected_ticket,
            &flow_config,
            repository_override,
            vcs,
            worker_backend,
        )
        .await
    }

    pub fn mark_session_crashed(
        &self,
        session_id: &WorkerSessionId,
        reason: &str,
    ) -> Result<(), CoreError> {
        let mut store = open_event_store(&self.config.event_store_path)?;
        let mapping = store.find_runtime_mapping_by_session_id(session_id)?;
        if let Some(mut mapping) = mapping {
            mapping.session.status = WorkerSessionStatus::Crashed;
            mapping.session.updated_at = now_timestamp();
            store.upsert_runtime_mapping(&mapping)?;
            store.delete_harness_session_binding(
                &mapping.session.session_id,
                &mapping.session.backend_kind,
            )?;

            store.append(NewEventEnvelope {
                event_id: format!("evt-session-crashed-{}", now_nanos()),
                occurred_at: now_timestamp(),
                work_item_id: Some(mapping.work_item_id.clone()),
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::SessionCrashed(SessionCrashedPayload {
                    session_id: session_id.clone(),
                    reason: reason.to_owned(),
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })?;
        } else {
            store.append(NewEventEnvelope {
                event_id: format!("evt-session-crashed-{}", now_nanos()),
                occurred_at: now_timestamp(),
                work_item_id: None,
                session_id: Some(session_id.clone()),
                payload: OrchestrationEventPayload::SessionCrashed(SessionCrashedPayload {
                    session_id: session_id.clone(),
                    reason: reason.to_owned(),
                }),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            })?;
        }
        Ok(())
    }

    pub async fn complete_session_after_merge(
        &self,
        session_id: &WorkerSessionId,
        worker_backend: &dyn WorkerBackend,
    ) -> Result<(), CoreError> {
        let cleanup_warnings = self
            .archive_session_internal(
                session_id,
                worker_backend,
                "Session archived after PR merge.",
            )
            .await?;

        if cleanup_warnings.is_empty() {
            Ok(())
        } else {
            Err(CoreError::DependencyUnavailable(format!(
                "merged session '{}' finalized with cleanup warnings: {}",
                session_id.as_str(),
                cleanup_warnings.join("; ")
            )))
        }
    }

    pub async fn archive_session(
        &self,
        session_id: &WorkerSessionId,
        worker_backend: &dyn WorkerBackend,
    ) -> Result<Option<String>, CoreError> {
        let cleanup_warnings = self
            .archive_session_internal(
                session_id,
                worker_backend,
                "Session archived from terminal session panel.",
            )
            .await?;
        if cleanup_warnings.is_empty() {
            Ok(None)
        } else {
            Ok(Some(cleanup_warnings.join("; ")))
        }
    }

    pub fn session_worktree_diff(
        &self,
        session_id: &WorkerSessionId,
    ) -> Result<orchestrator_ui::SessionWorktreeDiff, CoreError> {
        let store = open_event_store(&self.config.event_store_path)?;
        let mapping = store
            .find_runtime_mapping_by_session_id(session_id)?
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "could not resolve runtime mapping for session '{}'",
                    session_id.as_str()
                ))
            })?;

        let worktree_path = PathBuf::from(mapping.worktree.path.clone());
        if !worktree_path.exists() {
            return Err(CoreError::Configuration(format!(
                "worktree path does not exist for session '{}': {}",
                session_id.as_str(),
                worktree_path.display()
            )));
        }

        let git_bin = std::env::var_os("ORCHESTRATOR_GIT_BIN")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "git".into());

        let run_git = |args: &[&str]| -> Result<std::process::Output, CoreError> {
            Command::new(&git_bin)
                .arg("-C")
                .arg(&worktree_path)
                .args(args)
                .output()
                .map_err(|error| {
                    CoreError::DependencyUnavailable(format!(
                        "failed to execute git in '{}': {error}",
                        worktree_path.display()
                    ))
                })
        };

        let mut base_candidates = Vec::new();
        let mapped_base = mapping.worktree.base_branch.trim();
        if !mapped_base.is_empty() {
            base_candidates.push(mapped_base.to_owned());
        }

        if let Ok(origin_head) = run_git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
            if origin_head.status.success() {
                let resolved = String::from_utf8_lossy(&origin_head.stdout)
                    .trim()
                    .to_owned();
                if !resolved.is_empty() {
                    base_candidates.push(resolved.clone());
                    if let Some(stripped) = resolved.strip_prefix("origin/") {
                        if !stripped.trim().is_empty() {
                            base_candidates.push(stripped.trim().to_owned());
                        }
                    }
                }
            }
        }
        base_candidates.extend(
            ["main", "master", "develop"]
                .iter()
                .map(|entry| (*entry).to_owned()),
        );

        let mut seen = HashSet::new();
        base_candidates.retain(|candidate| {
            let trimmed = candidate.trim();
            !trimmed.is_empty() && seen.insert(trimmed.to_owned())
        });

        let mut resolved_base_ref = None;
        'outer: for candidate in base_candidates {
            let refs_to_try = if candidate.contains('/') {
                vec![candidate]
            } else {
                vec![candidate.clone(), format!("origin/{candidate}")]
            };
            for ref_name in refs_to_try {
                let verify_arg = format!("{ref_name}^{{commit}}");
                let verify = run_git(&["rev-parse", "--verify", "--quiet", verify_arg.as_str()])?;
                if verify.status.success() {
                    resolved_base_ref = Some(ref_name);
                    break 'outer;
                }
            }
        }

        let base_branch = resolved_base_ref.ok_or_else(|| {
            CoreError::Configuration(format!(
                "could not resolve a base branch for session '{}' (tried mapped, origin/HEAD, main/master/develop)",
                session_id.as_str()
            ))
        })?;

        let merge_base_output = run_git(&["merge-base", base_branch.as_str(), "HEAD"])?;
        if !merge_base_output.status.success() {
            let stderr = String::from_utf8_lossy(&merge_base_output.stderr)
                .trim()
                .to_owned();
            let detail = if stderr.is_empty() {
                format!("exit status {}", merge_base_output.status)
            } else {
                stderr
            };
            return Err(CoreError::DependencyUnavailable(format!(
                "failed to resolve merge-base for session '{}': {detail}",
                session_id.as_str()
            )));
        }
        let merge_base = String::from_utf8_lossy(&merge_base_output.stdout)
            .trim()
            .to_owned();
        if merge_base.is_empty() {
            return Err(CoreError::DependencyUnavailable(format!(
                "failed to resolve merge-base for session '{}': merge-base output was empty",
                session_id.as_str()
            )));
        }

        let output = run_git(&[
            "-c",
            "color.ui=never",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            "--unified=3",
            merge_base.as_str(),
            "--",
        ])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let detail = if stderr.is_empty() {
                format!("exit status {}", output.status)
            } else {
                stderr
            };
            return Err(CoreError::DependencyUnavailable(format!(
                "git diff failed for session '{}': {detail}",
                session_id.as_str()
            )));
        }

        let diff = sanitize_terminal_display_text(String::from_utf8_lossy(&output.stdout).as_ref());
        Ok(orchestrator_ui::SessionWorktreeDiff {
            session_id: session_id.clone(),
            base_branch,
            diff,
        })
    }

    pub fn advance_session_workflow(
        &self,
        session_id: &WorkerSessionId,
    ) -> Result<SessionWorkflowAdvanceOutcome, CoreError> {
        let mut store = open_event_store(&self.config.event_store_path)?;
        let mapping = store
            .find_runtime_mapping_by_session_id(session_id)?
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "could not resolve runtime mapping for session '{}'",
                    session_id.as_str()
                ))
            })?;

        let from = Self::latest_workflow_state_for_work_item(&store, &mapping.work_item_id)?;
        let (to, reason, guards, instruction) = Self::workflow_advance_target(&from)?;
        let next = apply_workflow_transition(&from, &to, &reason, &guards).map_err(|error| {
            CoreError::Configuration(format!(
                "workflow transition validation failed for work item '{}': {error}",
                mapping.work_item_id.as_str()
            ))
        })?;

        store.append(NewEventEnvelope {
            event_id: format!("evt-workflow-transition-{}", now_nanos()),
            occurred_at: now_timestamp(),
            work_item_id: Some(mapping.work_item_id.clone()),
            session_id: Some(mapping.session.session_id.clone()),
            payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                work_item_id: mapping.work_item_id.clone(),
                from: from.clone(),
                to: next.clone(),
                reason: Some(reason),
            }),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        })?;

        Ok(SessionWorkflowAdvanceOutcome {
            session_id: mapping.session.session_id,
            work_item_id: mapping.work_item_id,
            from,
            to: next,
            instruction: instruction.map(str::to_owned),
        })
    }
}

impl<S: Supervisor, G: GithubClient> App<S, G> {
    async fn archive_session_internal(
        &self,
        session_id: &WorkerSessionId,
        worker_backend: &dyn WorkerBackend,
        summary: &str,
    ) -> Result<Vec<String>, CoreError> {
        let existing_mapping = {
            let store = open_event_store(&self.config.event_store_path)?;
            store
                .find_runtime_mapping_by_session_id(session_id)?
                .ok_or_else(|| {
                    CoreError::Configuration(format!(
                        "could not resolve runtime mapping for session '{}'",
                        session_id.as_str()
                    ))
                })?
        };

        let mut cleanup_warnings = Vec::new();
        let handle = SessionHandle {
            session_id: RuntimeSessionId::new(session_id.as_str().to_owned()),
            backend: existing_mapping.session.backend_kind.clone(),
        };
        if let Err(error) = worker_backend.kill(&handle).await {
            if !matches!(error, orchestrator_core::RuntimeError::SessionNotFound(_)) {
                cleanup_warnings.push(format!(
                    "failed to archive runtime session '{}': {error}",
                    session_id.as_str()
                ));
            }
        }

        if let Err(error) = cleanup_worktree_after_merge(
            existing_mapping.worktree.path.as_str(),
            existing_mapping.worktree.branch.as_str(),
        ) {
            cleanup_warnings.push(error.to_string());
        }

        let mut store = open_event_store(&self.config.event_store_path)?;
        let mut mapping = store
            .find_runtime_mapping_by_session_id(session_id)?
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "could not resolve runtime mapping for session '{}'",
                    session_id.as_str()
                ))
            })?;
        mapping.session.status = WorkerSessionStatus::Done;
        mapping.session.updated_at = now_timestamp();
        store.upsert_runtime_mapping(&mapping)?;
        store.delete_harness_session_binding(
            &mapping.session.session_id,
            &mapping.session.backend_kind,
        )?;
        store.append(NewEventEnvelope {
            event_id: format!("evt-session-completed-{}", now_nanos()),
            occurred_at: now_timestamp(),
            work_item_id: Some(mapping.work_item_id.clone()),
            session_id: Some(session_id.clone()),
            payload: OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                session_id: session_id.clone(),
                summary: Some(summary.to_owned()),
            }),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        })?;

        Ok(cleanup_warnings)
    }
}

fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", now.as_secs(), now.subsec_nanos())
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn cleanup_worktree_after_merge(worktree_path_raw: &str, branch: &str) -> Result<(), CoreError> {
    let worktree_path = PathBuf::from(worktree_path_raw.trim());
    if worktree_path.as_os_str().is_empty() || !worktree_path.exists() {
        return Ok(());
    }

    let repository_root = resolve_repository_root_from_worktree(&worktree_path)?;
    let worktree_arg = worktree_path.to_string_lossy().to_string();

    let remove_output = run_git_command(
        repository_root.as_path(),
        &["worktree", "remove", "--force", worktree_arg.as_str()],
    )?;
    if !remove_output.status.success() {
        let detail = git_output_detail(&remove_output);
        let normalized = detail.to_ascii_lowercase();
        if !looks_like_path_missing_error(&normalized) {
            std::fs::remove_dir_all(&worktree_path).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "failed to remove worktree '{}' after merge using git ({detail}) and filesystem fallback ({error})",
                    worktree_path.display()
                ))
            })?;
        }
    }

    let branch = branch.trim();
    if branch.is_empty() {
        return Ok(());
    }

    let delete_output = run_git_command(repository_root.as_path(), &["branch", "-d", branch])?;
    if delete_output.status.success() {
        return Ok(());
    }

    let delete_detail = git_output_detail(&delete_output);
    let delete_normalized = delete_detail.to_ascii_lowercase();
    if looks_like_branch_missing_error(&delete_normalized) {
        return Ok(());
    }

    let force_delete_output =
        run_git_command(repository_root.as_path(), &["branch", "-D", branch])?;
    if force_delete_output.status.success() {
        return Ok(());
    }

    let force_delete_detail = git_output_detail(&force_delete_output);
    let force_delete_normalized = force_delete_detail.to_ascii_lowercase();
    if looks_like_branch_missing_error(&force_delete_normalized) {
        return Ok(());
    }

    Err(CoreError::DependencyUnavailable(format!(
        "failed to delete local merged branch '{branch}': {force_delete_detail}"
    )))
}

fn resolve_repository_root_from_worktree(worktree_path: &PathBuf) -> Result<PathBuf, CoreError> {
    let output = run_git_command(worktree_path.as_path(), &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Err(CoreError::DependencyUnavailable(format!(
            "failed to resolve repository root for worktree '{}': {}",
            worktree_path.display(),
            git_output_detail(&output)
        )));
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if root.is_empty() {
        return Err(CoreError::DependencyUnavailable(format!(
            "failed to resolve repository root for worktree '{}': empty `git rev-parse` output",
            worktree_path.display()
        )));
    }

    Ok(PathBuf::from(root))
}

fn run_git_command(
    cwd: &std::path::Path,
    args: &[&str],
) -> Result<std::process::Output, CoreError> {
    let git_bin = std::env::var_os("ORCHESTRATOR_GIT_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "git".into());

    Command::new(&git_bin)
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "failed to execute git command in '{}': {error}",
                cwd.display()
            ))
        })
}

fn git_output_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exit status {}", output.status)
}

fn looks_like_path_missing_error(detail: &str) -> bool {
    detail.contains("does not exist")
        || detail.contains("not found")
        || detail.contains("cannot find")
        || detail.contains("no such file or directory")
}

fn looks_like_branch_missing_error(detail: &str) -> bool {
    detail.contains("not found")
        || detail.contains("does not exist")
        || detail.contains("unknown revision")
        || detail.contains("not a valid branch")
}

fn sanitize_terminal_display_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\t' => output.push(ch),
            '\r' => output.push('\n'),
            _ if ch.is_control() => {}
            _ => output.push(ch),
        }
    }
    output
}

fn supervisor_model_from_env() -> String {
    std::env::var("ORCHESTRATOR_SUPERVISOR_MODEL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SUPERVISOR_MODEL.to_owned())
}

#[async_trait::async_trait]
impl<S, G> SupervisorCommandDispatcher for App<S, G>
where
    S: Supervisor + LlmProvider + Send + Sync,
    G: GithubClient + CodeHostProvider + Send + Sync,
{
    async fn dispatch_supervisor_command(
        &self,
        invocation: UntypedCommandInvocation,
        context: SupervisorCommandContext,
    ) -> Result<(String, orchestrator_core::LlmResponseStream), CoreError> {
        command_dispatch::dispatch_supervisor_runtime_command(
            &self.supervisor,
            &self.github,
            self.ticketing.as_ref(),
            &self.config.event_store_path,
            invocation,
            context,
        )
        .await
    }

    async fn cancel_supervisor_command(&self, stream_id: &str) -> Result<(), CoreError> {
        command_dispatch::record_user_initiated_supervisor_cancel(
            &self.config.event_store_path,
            stream_id,
        );
        self.supervisor.cancel_stream(stream_id).await
    }
}
