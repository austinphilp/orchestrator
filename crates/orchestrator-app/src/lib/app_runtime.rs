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
                Some("Workflow transition approved: Planning -> Implementing. End planning mode and begin implementation in this worktree now. Do not run the full local build or full local test suite by default; rely on GitHub Actions pipeline results to validate full verification."),
            )),
            WorkflowState::Implementing => Ok((
                WorkflowState::PRDrafted,
                WorkflowTransitionReason::DraftPullRequestCreated,
                WorkflowGuardContext {
                    tests_passed: true,
                    has_draft_pr: true,
                    ..WorkflowGuardContext::default()
                },
                Some("Workflow transition approved: Implementing -> PR Drafted. Pause implementation, open/update the GitHub PR using the gh CLI, and use GitHub Actions pipeline results as the source of truth for build/test health. If a pipeline fails, fix the failure and push updates; avoid running the full local build or full local test suite unless a targeted repro is needed."),
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
            | WorkflowState::PendingMerge => Err(CoreError::Configuration(
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

        let projection = self.projection_state()?;

        Ok(StartupState {
            status: format!("ready ({})", self.config.workspace),
            projection,
        })
    }

    pub fn projection_state(&self) -> Result<ProjectionState, CoreError> {
        let store = open_event_store(&self.config.event_store_path)?;
        let events = store.read_ordered()?;
        let mut projection = rebuild_projection(&events);
        let session_working_states = store.list_session_working_states()?;
        for (session_id, is_working) in session_working_states {
            projection
                .session_runtime
                .insert(session_id, SessionRuntimeProjection { is_working });
        }
        Ok(projection)
    }

    pub fn publish_inbox_item(
        &self,
        request: &InboxPublishRequest,
    ) -> Result<ProjectionState, CoreError> {
        let title = request.title.trim();
        if title.is_empty() {
            return Err(CoreError::InvalidCommandArgs {
                command_id: "ui.publish_inbox_item".to_owned(),
                reason: "inbox publish requires a non-empty title".to_owned(),
            });
        }

        let coalesce_key = normalize_inbox_coalesce_key(request.coalesce_key.as_str());
        if coalesce_key.is_empty() {
            return Err(CoreError::InvalidCommandArgs {
                command_id: "ui.publish_inbox_item".to_owned(),
                reason: "inbox publish requires a non-empty coalesce key".to_owned(),
            });
        }

        let coalesce_scope = request
            .session_id
            .as_ref()
            .map(|session_id| session_id.as_str())
            .unwrap_or_else(|| request.work_item_id.as_str());
        let inbox_item_id = InboxItemId::new(format!("inbox-{coalesce_scope}-{coalesce_key}"));

        let mut store = open_event_store(&self.config.event_store_path)?;
        store.append(NewEventEnvelope {
            event_id: format!("evt-inbox-fanout-{}", now_nanos()),
            occurred_at: now_timestamp(),
            work_item_id: Some(request.work_item_id.clone()),
            session_id: request.session_id.clone(),
            payload: OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                inbox_item_id,
                work_item_id: request.work_item_id.clone(),
                kind: request.kind.clone(),
                title: title.to_owned(),
            }),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        })?;

        let events = store.read_ordered()?;
        Ok(rebuild_projection(&events))
    }

    pub fn resolve_inbox_item(
        &self,
        request: &InboxResolveRequest,
    ) -> Result<ProjectionState, CoreError> {
        let mut store = open_event_store(&self.config.event_store_path)?;
        let projection = rebuild_projection(&store.read_ordered()?);
        let Some(inbox_item) = projection.inbox_items.get(&request.inbox_item_id) else {
            return Err(CoreError::InvalidCommandArgs {
                command_id: "ui.resolve_inbox_item".to_owned(),
                reason: format!(
                    "inbox item '{}' was not found",
                    request.inbox_item_id.as_str()
                ),
            });
        };
        if inbox_item.work_item_id != request.work_item_id {
            return Err(CoreError::InvalidCommandArgs {
                command_id: "ui.resolve_inbox_item".to_owned(),
                reason: format!(
                    "inbox item '{}' does not belong to work item '{}'",
                    request.inbox_item_id.as_str(),
                    request.work_item_id.as_str()
                ),
            });
        }
        if inbox_item.resolved {
            return Ok(projection);
        }

        let session_id = projection
            .work_items
            .get(&request.work_item_id)
            .and_then(|work_item| work_item.session_id.clone());
        store.append(NewEventEnvelope {
            event_id: format!("evt-inbox-resolved-{}", now_nanos()),
            occurred_at: now_timestamp(),
            work_item_id: Some(request.work_item_id.clone()),
            session_id,
            payload: OrchestrationEventPayload::InboxItemResolved(InboxItemResolvedPayload {
                inbox_item_id: request.inbox_item_id.clone(),
                work_item_id: request.work_item_id.clone(),
            }),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        })?;

        let events = store.read_ordered()?;
        Ok(rebuild_projection(&events))
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
        let selected_ticket_description = self
            .ticketing
            .get_ticket(GetTicketRequest {
                ticket_id: selected_ticket.ticket_id.clone(),
            })
            .await
            .ok()
            .and_then(|details| details.description);

        orchestrator_core::start_or_resume_selected_ticket(
            &mut store,
            selected_ticket,
            selected_ticket_description.as_deref(),
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
        if self.config.runtime.event_prune_enabled {
            match store.prune_completed_session_events(
                orchestrator_core::EventPrunePolicy {
                    retention_days: self.config.runtime.event_retention_days,
                },
                now_unix_seconds(),
            ) {
                Ok(report) => {
                    tracing::info!(
                        retention_days = self.config.runtime.event_retention_days,
                        cutoff_unix_seconds = report.cutoff_unix_seconds,
                        candidate_sessions = report.candidate_sessions,
                        eligible_sessions = report.eligible_sessions,
                        pruned_work_items = report.pruned_work_items,
                        deleted_events = report.deleted_events,
                        deleted_event_artifact_refs = report.deleted_event_artifact_refs,
                        skipped_invalid_timestamps = report.skipped_invalid_timestamps,
                        trigger = "session_crashed",
                        "event prune maintenance completed"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        session_id = session_id.as_str(),
                        error = %error,
                        "event prune maintenance failed after session crash"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn set_session_working_state(
        &self,
        session_id: &WorkerSessionId,
        is_working: bool,
    ) -> Result<(), CoreError> {
        let store = open_event_store(&self.config.event_store_path)?;
        store.set_session_working_state(session_id, is_working)
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

        if !cleanup_warnings.is_empty() {
            tracing::warn!(
                session_id = session_id.as_str(),
                warnings = %cleanup_warnings.join("; "),
                "merged session finalized with cleanup warnings"
            );
        }
        Ok(())
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

        let git_bin = std::ffi::OsString::from(git_binary_from_config());

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
        if self.config.runtime.event_prune_enabled {
            match store.prune_completed_session_events(
                orchestrator_core::EventPrunePolicy {
                    retention_days: self.config.runtime.event_retention_days,
                },
                now_unix_seconds(),
            ) {
                Ok(report) => {
                    tracing::info!(
                        retention_days = self.config.runtime.event_retention_days,
                        cutoff_unix_seconds = report.cutoff_unix_seconds,
                        candidate_sessions = report.candidate_sessions,
                        eligible_sessions = report.eligible_sessions,
                        pruned_work_items = report.pruned_work_items,
                        deleted_events = report.deleted_events,
                        deleted_event_artifact_refs = report.deleted_event_artifact_refs,
                        skipped_invalid_timestamps = report.skipped_invalid_timestamps,
                        trigger = "session_completed",
                        "event prune maintenance completed"
                    );
                }
                Err(error) => {
                    cleanup_warnings.push(format!(
                        "event prune maintenance failed after session completion: {error}"
                    ));
                }
            }
        }

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

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalize_inbox_coalesce_key(raw: &str) -> String {
    let mut key = String::new();
    let mut previous_was_dash = false;
    for ch in raw.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            previous_was_dash = false;
            ch.to_ascii_lowercase()
        } else {
            if previous_was_dash {
                continue;
            }
            previous_was_dash = true;
            '-'
        };
        key.push(mapped);
    }
    key.trim_matches('-').to_owned()
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

fn run_git_command(cwd: &std::path::Path, args: &[&str]) -> Result<std::process::Output, CoreError> {
    let git_bin = std::ffi::OsString::from(git_binary_from_config());

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

static SUPERVISOR_MODEL_CONFIG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static GIT_BINARY_CONFIG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn set_supervisor_model_config(model: String) {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return;
    }
    let _ = SUPERVISOR_MODEL_CONFIG.set(trimmed.to_owned());
}

pub fn set_git_binary_config(binary: String) {
    let trimmed = binary.trim();
    if trimmed.is_empty() {
        return;
    }
    let _ = GIT_BINARY_CONFIG.set(trimmed.to_owned());
}

fn supervisor_model_from_env() -> String {
    SUPERVISOR_MODEL_CONFIG
        .get()
        .cloned()
        .unwrap_or_else(|| DEFAULT_SUPERVISOR_MODEL.to_owned())
}

fn git_binary_from_config() -> String {
    GIT_BINARY_CONFIG
        .get()
        .cloned()
        .unwrap_or_else(|| "git".to_owned())
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
