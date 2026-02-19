pub struct App<S: Supervisor, G: GithubClient> {
    pub config: AppConfig,
    pub ticketing: Arc<dyn TicketingProvider + Send + Sync>,
    pub supervisor: S,
    pub github: G,
}

impl<S: Supervisor, G: GithubClient> App<S, G> {
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

    pub fn set_session_workflow_stage(
        &self,
        session_id: &WorkerSessionId,
        workflow_stage: &str,
    ) -> Result<(), CoreError> {
        let store = open_event_store(&self.config.event_store_path)?;
        store.upsert_session_workflow_stage(session_id, workflow_stage)
    }

    pub fn list_session_workflow_stages(
        &self,
    ) -> Result<Vec<(WorkerSessionId, String)>, CoreError> {
        let store = open_event_store(&self.config.event_store_path)?;
        store.list_session_workflow_stages()
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

