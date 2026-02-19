async fn start_new_mapping(
    store: &mut SqliteEventStore,
    selected_ticket: &TicketSummary,
    config: &SelectedTicketFlowConfig,
    project_id: ProjectId,
    repository_override: Option<PathBuf>,
    vcs: &dyn VcsProvider,
    worker_backend: &dyn WorkerBackend,
) -> Result<SelectedTicketFlowResult, CoreError> {
    let (provider, provider_ticket_id) =
        parse_ticket_provider_identity(&selected_ticket.ticket_id)?;
    let repository =
        resolve_repository_for_ticket(store, vcs, &provider, &project_id, repository_override)
            .await?;
    store.upsert_project_repository_mapping(&ProjectRepositoryMappingRecord {
        provider: provider.clone(),
        project_id: project_id.clone(),
        repository_path: repository.root.to_string_lossy().into_owned(),
    })?;
    let issue_key = normalize_issue_key(selected_ticket.identifier.as_str())?;
    let title_slug = title_slug(
        selected_ticket.title.as_str(),
        DEFAULT_WORKTREE_TITLE_SLUG_LIMIT,
    );
    let stable_ticket_component = stable_component(selected_ticket.ticket_id.as_str());

    let work_item_id = WorkItemId::new(format!("wi-{stable_ticket_component}"));
    let worktree_id = WorktreeId::new(format!("wt-{stable_ticket_component}"));
    let session_id = WorkerSessionId::new(format!("sess-{stable_ticket_component}"));

    let branch = format!("{WORKTREE_BRANCH_PREFIX}{issue_key}-{title_slug}");
    let worktree_dir = format!("{}-{title_slug}", issue_key.to_ascii_lowercase());
    let worktree_path = config.worktrees_root.join(worktree_dir);
    let base_branch = normalize_base_branch(config.base_branch.as_str());
    let model = config.model.clone();
    let now = now_timestamp();
    let instruction = start_instruction(&provider, selected_ticket);

    let worktree_summary = vcs
        .create_worktree(CreateWorktreeRequest {
            worktree_id: worktree_id.clone(),
            repository: repository.clone(),
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            base_branch: base_branch.clone(),
            ticket_identifier: Some(issue_key),
        })
        .await?;
    let resolved_worktree_path = worktree_summary.path.clone();

    let spawned = match worker_backend
        .spawn(SpawnSpec {
            session_id: session_id.clone().into(),
            workdir: resolved_worktree_path.clone(),
            model: model.clone(),
            instruction_prelude: Some(instruction),
            environment: Vec::new(),
        })
        .await
    {
        Ok(spawned) => spawned,
        Err(spawn_error) => {
            if let Err(cleanup_error) = vcs
                .delete_worktree(DeleteWorktreeRequest::non_destructive(
                    worktree_summary.clone(),
                ))
                .await
            {
                return Err(CoreError::Runtime(RuntimeError::Internal(format!(
                    "worker spawn failed for ticket '{}' and non-destructive worktree cleanup also failed: spawn error: {spawn_error}; cleanup error: {cleanup_error}",
                    selected_ticket.identifier
                ))));
            }

            return Err(CoreError::Runtime(spawn_error));
        }
    };

    if spawned.session_id.as_str() != session_id.as_str() {
        return Err(CoreError::Runtime(RuntimeError::Internal(format!(
            "worker backend returned mismatched session id for ticket '{}': expected '{}', got '{}'",
            selected_ticket.identifier,
            session_id.as_str(),
            spawned.session_id.as_str()
        ))));
    }
    let kickoff_message = start_turn_instruction(&provider, selected_ticket);
    let mut kickoff_bytes = kickoff_message.as_bytes().to_vec();
    if !kickoff_bytes.ends_with(b"\n") {
        kickoff_bytes.push(b'\n');
    }
    let kickoff_handle = SessionHandle {
        session_id: session_id.clone().into(),
        backend: worker_backend.kind(),
    };
    if let Err(send_error) = worker_backend
        .send_input(&kickoff_handle, &kickoff_bytes)
        .await
    {
        let kill_error = worker_backend.kill(&kickoff_handle).await.err();
        let cleanup_error = vcs
            .delete_worktree(DeleteWorktreeRequest::non_destructive(
                worktree_summary.clone(),
            ))
            .await
            .err();
        return Err(CoreError::Runtime(RuntimeError::Internal(format!(
            "worker kickoff failed for ticket '{}': send error: {send_error}; kill error: {:?}; worktree cleanup error: {:?}",
            selected_ticket.identifier, kill_error, cleanup_error
        ))));
    }

    let mapping = RuntimeMappingRecord {
        ticket: ticket_record_from_summary(selected_ticket, provider, provider_ticket_id, &now),
        work_item_id: work_item_id.clone(),
        worktree: WorktreeRecord {
            worktree_id,
            work_item_id: work_item_id.clone(),
            path: worktree_summary.path.to_string_lossy().to_string(),
            branch: worktree_summary.branch,
            base_branch: worktree_summary.base_branch,
            created_at: now.clone(),
        },
        session: SessionRecord {
            session_id: session_id.clone(),
            work_item_id: work_item_id.clone(),
            backend_kind: worker_backend.kind(),
            workdir: resolved_worktree_path.to_string_lossy().to_string(),
            model,
            status: WorkerSessionStatus::Running,
            created_at: now.clone(),
            updated_at: now,
        },
    };
    store.upsert_runtime_mapping(&mapping)?;
    persist_harness_session_binding(
        store,
        worker_backend,
        &mapping.session.session_id,
        &mapping.session.backend_kind,
    )
    .await?;
    ensure_lifecycle_events(store, selected_ticket, &mapping, &project_id, true)?;

    Ok(SelectedTicketFlowResult {
        action: SelectedTicketFlowAction::Started,
        mapping,
    })
}

async fn resume_existing_mapping(
    store: &mut SqliteEventStore,
    selected_ticket: &TicketSummary,
    _project_id: ProjectId,
    config: &SelectedTicketFlowConfig,
    worker_backend: &dyn WorkerBackend,
    existing_mapping: RuntimeMappingRecord,
) -> Result<SelectedTicketFlowResult, CoreError> {
    if existing_mapping.session.status == WorkerSessionStatus::Done {
        return Err(CoreError::Configuration(format!(
            "Ticket '{}' is already completed and cannot be resumed by this flow.",
            selected_ticket.identifier
        )));
    }

    let backend_kind = worker_backend.kind();
    if existing_mapping.session.backend_kind != backend_kind {
        return Err(CoreError::Configuration(format!(
            "Ticket '{}' is mapped to backend '{:?}' but active backend is '{:?}'.",
            selected_ticket.identifier, existing_mapping.session.backend_kind, backend_kind
        )));
    }
    let resume_model = existing_mapping
        .session
        .model
        .clone()
        .or(config.model.clone());

    let resume_message = resume_instruction(&existing_mapping.ticket.provider, selected_ticket);
    let mut resume_bytes = resume_message.as_bytes().to_vec();
    if !resume_bytes.ends_with(b"\n") {
        resume_bytes.push(b'\n');
    }

    let handle = SessionHandle {
        session_id: existing_mapping.session.session_id.clone().into(),
        backend: backend_kind.clone(),
    };
    let persisted_harness_session_id = store.find_harness_session_binding(
        &existing_mapping.session.session_id,
        &existing_mapping.session.backend_kind,
    )?;

    let mut spawned_new_runtime_session = false;
    if existing_mapping.session.status == WorkerSessionStatus::Crashed {
        spawn_resume_session(
            worker_backend,
            &existing_mapping,
            resume_model.clone(),
            resume_message,
            persisted_harness_session_id.as_deref(),
        )
        .await?;
        spawned_new_runtime_session = true;
    } else {
        match worker_backend.send_input(&handle, &resume_bytes).await {
            Ok(()) => {}
            Err(RuntimeError::SessionNotFound(_)) => {
                spawn_resume_session(
                    worker_backend,
                    &existing_mapping,
                    resume_model.clone(),
                    resume_message,
                    persisted_harness_session_id.as_deref(),
                )
                .await?;
                spawned_new_runtime_session = true;
            }
            Err(error) => return Err(CoreError::Runtime(error)),
        }
    }

    let (provider, provider_ticket_id) =
        parse_ticket_provider_identity(&selected_ticket.ticket_id)?;
    let now = now_timestamp();
    let mut mapping = existing_mapping;
    mapping.ticket =
        ticket_record_from_summary(selected_ticket, provider, provider_ticket_id, &now);
    mapping.session.model = resume_model;
    mapping.session.status = WorkerSessionStatus::Running;
    mapping.session.updated_at = now;
    store.upsert_runtime_mapping(&mapping)?;
    persist_harness_session_binding(
        store,
        worker_backend,
        &mapping.session.session_id,
        &mapping.session.backend_kind,
    )
    .await?;
    ensure_lifecycle_events(
        store,
        selected_ticket,
        &mapping,
        &config.project_id,
        spawned_new_runtime_session,
    )?;

    Ok(SelectedTicketFlowResult {
        action: SelectedTicketFlowAction::Resumed,
        mapping,
    })
}

async fn spawn_resume_session(
    worker_backend: &dyn WorkerBackend,
    mapping: &RuntimeMappingRecord,
    model: Option<String>,
    instruction: String,
    persisted_harness_session_id: Option<&str>,
) -> Result<(), CoreError> {
    let spawned = worker_backend
        .spawn(SpawnSpec {
            session_id: mapping.session.session_id.clone().into(),
            workdir: PathBuf::from(mapping.session.workdir.as_str()),
            model,
            instruction_prelude: Some(instruction.clone()),
            environment: resume_spawn_environment(
                &mapping.session.backend_kind,
                persisted_harness_session_id,
            ),
        })
        .await?;

    if spawned.session_id.as_str() != mapping.session.session_id.as_str() {
        return Err(CoreError::Runtime(RuntimeError::Internal(format!(
            "worker backend returned mismatched resumed session id: expected '{}', got '{}'",
            mapping.session.session_id.as_str(),
            spawned.session_id.as_str()
        ))));
    }

    let mut resume_bytes = instruction.as_bytes().to_vec();
    if !resume_bytes.ends_with(b"\n") {
        resume_bytes.push(b'\n');
    }
    let handle = SessionHandle {
        session_id: mapping.session.session_id.clone().into(),
        backend: mapping.session.backend_kind.clone(),
    };
    worker_backend
        .send_input(&handle, &resume_bytes)
        .await
        .map_err(CoreError::Runtime)?;

    Ok(())
}

async fn persist_harness_session_binding(
    store: &SqliteEventStore,
    worker_backend: &dyn WorkerBackend,
    session_id: &WorkerSessionId,
    backend_kind: &BackendKind,
) -> Result<(), CoreError> {
    let handle = SessionHandle {
        session_id: session_id.clone().into(),
        backend: backend_kind.clone(),
    };

    let harness_session_id = match worker_backend.harness_session_id(&handle).await {
        Ok(value) => value,
        Err(RuntimeError::SessionNotFound(_)) => None,
        Err(error) => return Err(CoreError::Runtime(error)),
    };

    let Some(harness_session_id) = harness_session_id else {
        return Ok(());
    };
    let harness_session_id = harness_session_id.trim();
    if harness_session_id.is_empty() {
        return Ok(());
    }

    store.upsert_harness_session_binding(session_id, backend_kind, harness_session_id)
}

fn resume_spawn_environment(
    backend_kind: &BackendKind,
    persisted_harness_session_id: Option<&str>,
) -> Vec<(String, String)> {
    if *backend_kind != BackendKind::Codex {
        return Vec::new();
    }

    let Some(harness_session_id) = persisted_harness_session_id else {
        return Vec::new();
    };
    let harness_session_id = harness_session_id.trim();
    if harness_session_id.is_empty() {
        return Vec::new();
    }

    vec![(
        ENV_HARNESS_SESSION_ID.to_owned(),
        harness_session_id.to_owned(),
    )]
}

fn ensure_lifecycle_events(
    store: &mut SqliteEventStore,
    selected_ticket: &TicketSummary,
    mapping: &RuntimeMappingRecord,
    project_id: &ProjectId,
    force_session_spawned_event: bool,
) -> Result<(), CoreError> {
    let existing = store.read_events_for_work_item(&mapping.work_item_id)?;

    let mut has_work_item_created = false;
    let mut has_worktree_created = false;
    let mut has_session_spawned = false;
    let mut latest_workflow_state = None;

    for event in &existing {
        match &event.payload {
            OrchestrationEventPayload::WorkItemCreated(payload)
                if payload.work_item_id == mapping.work_item_id =>
            {
                has_work_item_created = true;
            }
            OrchestrationEventPayload::WorktreeCreated(payload)
                if payload.worktree_id == mapping.worktree.worktree_id =>
            {
                has_worktree_created = true;
            }
            OrchestrationEventPayload::SessionSpawned(payload)
                if payload.session_id == mapping.session.session_id =>
            {
                has_session_spawned = true;
            }
            OrchestrationEventPayload::WorkflowTransition(payload) => {
                latest_workflow_state = Some(payload.to.clone());
            }
            _ => {}
        }
    }

    store.append(new_event(
        "ticket-synced",
        Some(mapping.work_item_id.clone()),
        None,
        OrchestrationEventPayload::TicketSynced(crate::TicketSyncedPayload {
            ticket_id: mapping.ticket.ticket_id.clone(),
            identifier: selected_ticket.identifier.clone(),
            title: selected_ticket.title.clone(),
            state: selected_ticket.state.clone(),
            assignee: selected_ticket.assignee.clone(),
            priority: selected_ticket.priority,
        }),
    ))?;

    if !has_work_item_created {
        store.append(new_event(
            "work-item-created",
            Some(mapping.work_item_id.clone()),
            None,
            OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                work_item_id: mapping.work_item_id.clone(),
                ticket_id: mapping.ticket.ticket_id.clone(),
                project_id: project_id.clone(),
            }),
        ))?;
    }

    if !has_worktree_created {
        store.append(new_event(
            "worktree-created",
            Some(mapping.work_item_id.clone()),
            None,
            OrchestrationEventPayload::WorktreeCreated(WorktreeCreatedPayload {
                worktree_id: mapping.worktree.worktree_id.clone(),
                work_item_id: mapping.work_item_id.clone(),
                path: mapping.worktree.path.clone(),
                branch: mapping.worktree.branch.clone(),
                base_branch: mapping.worktree.base_branch.clone(),
            }),
        ))?;
    }

    if force_session_spawned_event || !has_session_spawned {
        store.append(new_event(
            "session-spawned",
            Some(mapping.work_item_id.clone()),
            Some(mapping.session.session_id.clone()),
            OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                session_id: mapping.session.session_id.clone(),
                work_item_id: mapping.work_item_id.clone(),
                model: mapping
                    .session
                    .model
                    .clone()
                    .unwrap_or_else(|| "default".to_owned()),
            }),
        ))?;
    }

    let guard_context = workflow_guard_context(mapping);
    let current_state = latest_workflow_state.unwrap_or(WorkflowState::New);

    if current_state == WorkflowState::New {
        append_workflow_transition_event(
            store,
            mapping,
            &current_state,
            &WorkflowState::Planning,
            WorkflowTransitionReason::TicketAccepted,
            &guard_context,
        )?;
    }

    Ok(())
}

fn append_workflow_transition_event(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    from: &WorkflowState,
    to: &WorkflowState,
    reason: WorkflowTransitionReason,
    guard_context: &WorkflowGuardContext,
) -> Result<WorkflowState, CoreError> {
    let next = apply_workflow_transition(from, to, &reason, guard_context).map_err(|error| {
        CoreError::Configuration(format!(
            "Workflow transition validation failed for work item '{}': {error}",
            mapping.work_item_id.as_str()
        ))
    })?;

    store.append(new_event(
        "workflow-transition",
        Some(mapping.work_item_id.clone()),
        Some(mapping.session.session_id.clone()),
        OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
            work_item_id: mapping.work_item_id.clone(),
            from: from.clone(),
            to: to.clone(),
            reason: Some(reason),
        }),
    ))?;

    Ok(next)
}

fn workflow_guard_context(mapping: &RuntimeMappingRecord) -> WorkflowGuardContext {
    WorkflowGuardContext {
        has_active_session: !matches!(
            mapping.session.status,
            WorkerSessionStatus::Done | WorkerSessionStatus::Crashed
        ),
        ..WorkflowGuardContext::default()
    }
}

fn new_event(
    prefix: &str,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    payload: OrchestrationEventPayload,
) -> NewEventEnvelope {
    NewEventEnvelope {
        event_id: next_event_id(prefix),
        occurred_at: now_timestamp(),
        work_item_id,
        session_id,
        payload,
        schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
    }
}

