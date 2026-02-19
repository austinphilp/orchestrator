fn normalize_pull_request_url(url: &str) -> &str {
    url.trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '(' || ch == ')')
        .trim_end_matches(|ch: char| ch == ',' || ch == ';' || ch == '.')
}

fn sanitize_error_display_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\t' => output.push(ch),
            '\r' => output.push('\n'),
            '\u{2400}' => {}
            _ if ch.is_control() => {}
            _ => output.push(ch),
        }
    }
    output
}

fn resolve_runtime_mapping_for_context(
    store: &SqliteEventStore,
    context: &SupervisorCommandContext,
) -> Result<RuntimeMappingRecord, CoreError> {
    resolve_runtime_mapping_for_context_with_command(
        store,
        context,
        command_ids::WORKFLOW_APPROVE_PR_READY,
    )
}

fn resolve_runtime_mapping_for_context_with_command(
    store: &SqliteEventStore,
    context: &SupervisorCommandContext,
    command_id: &str,
) -> Result<RuntimeMappingRecord, CoreError> {
    let mappings = store.list_runtime_mappings()?;

    if let Some(session_id) = context.selected_session_id.as_deref() {
        if let Some(mapping) = mappings
            .iter()
            .find(|mapping| mapping.session.session_id.as_str() == session_id)
        {
            return Ok(mapping.clone());
        }

        return Err(CoreError::Configuration(format!(
            "{command_id} could not resolve runtime mapping for selected session '{session_id}'"
        )));
    }

    if let Some(work_item_id) = context.selected_work_item_id.as_deref() {
        if let Some(mapping) = mappings
            .iter()
            .find(|mapping| mapping.work_item_id.as_str() == work_item_id)
        {
            return Ok(mapping.clone());
        }

        return Err(CoreError::Configuration(format!(
            "{command_id} could not resolve runtime mapping for selected work item '{work_item_id}'"
        )));
    }

    Err(CoreError::Configuration(
        format!("{command_id} requires an active runtime session or selected work item context")
            .to_owned(),
    ))
}

fn resolve_pull_request_for_mapping_with_command(
    store: &SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    command_id: &str,
) -> Result<PullRequestRef, CoreError> {
    let events = store.read_event_with_artifacts(&mapping.work_item_id)?;
    let mut candidate_parse_error: Option<CoreError> = None;

    for stored in events.iter().rev() {
        for artifact_id in stored.artifact_ids.iter().rev() {
            let Some(artifact) = store.get_artifact(artifact_id)? else {
                continue;
            };

            if !is_pull_request_artifact_candidate(&artifact.kind, artifact.storage_ref.as_str()) {
                continue;
            }

            let url = artifact.storage_ref.trim();
            if url.is_empty() {
                continue;
            }

            let number = match parse_pull_request_number(url) {
                Ok(number) => number,
                Err(error) => {
                    candidate_parse_error = Some(error);
                    continue;
                }
            };
            let (repository_name, repository_id) = match parse_pull_request_repository(url) {
                Ok(repository) => repository,
                Err(error) => {
                    candidate_parse_error = Some(error);
                    continue;
                }
            };
            return Ok(PullRequestRef {
                repository: RepositoryRef {
                    id: repository_id,
                    name: repository_name,
                    root: PathBuf::from(mapping.worktree.path.clone()),
                },
                number,
                url: url.to_owned(),
            });
        }
    }

    if let Some(candidate_error) = candidate_parse_error {
        return Err(CoreError::Configuration(format!(
            "{command_id} could not resolve a usable PR artifact for work item '{}': {candidate_error}",
            mapping.work_item_id.as_str(),
        )));
    }

    Err(CoreError::Configuration(format!(
        "{command_id} could not resolve a PR artifact for work item '{}'",
        mapping.work_item_id.as_str()
    )))
}

fn repository_ref_for_runtime_mapping(mapping: &RuntimeMappingRecord) -> RepositoryRef {
    let root = PathBuf::from(mapping.worktree.path.clone());
    let inferred_name = root
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    RepositoryRef {
        id: inferred_name.to_owned(),
        name: inferred_name.to_owned(),
        root,
    }
}

fn persist_pull_request_artifact_from_vcs_fallback(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    command_id: &str,
    pr: &PullRequestRef,
) -> Result<(), CoreError> {
    let occurred_at = now_timestamp();
    let artifact_id = ArtifactId::new(next_event_id("artifact-pr-vcs-fallback"));
    let artifact = ArtifactRecord {
        artifact_id: artifact_id.clone(),
        work_item_id: mapping.work_item_id.clone(),
        kind: ArtifactKind::PR,
        metadata: json!({
            "source": "vcs_fallback",
            "command_id": command_id,
            "branch": mapping.worktree.branch.as_str(),
        }),
        storage_ref: pr.url.clone(),
        created_at: occurred_at.clone(),
    };
    store.create_artifact(&artifact)?;
    store.append_event(
        NewEventEnvelope {
            event_id: next_event_id("artifact-created-pr-vcs-fallback"),
            occurred_at,
            work_item_id: Some(mapping.work_item_id.clone()),
            session_id: Some(mapping.session.session_id.clone()),
            payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                artifact_id: artifact_id.clone(),
                work_item_id: mapping.work_item_id.clone(),
                kind: ArtifactKind::PR,
                label: format!("Pull request #{} (vcs fallback)", pr.number),
                uri: pr.url.clone(),
            }),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        },
        &[artifact_id],
    )?;
    Ok(())
}

async fn resolve_pull_request_for_mapping_with_vcs_fallback<C>(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    command_id: &str,
    code_host: &C,
) -> Result<PullRequestRef, CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    match resolve_pull_request_for_mapping_with_command(store, mapping, command_id) {
        Ok(pr) => return Ok(pr),
        Err(CoreError::Configuration(_)) => {}
        Err(error) => return Err(error),
    }

    let repository = repository_ref_for_runtime_mapping(mapping);
    let fallback = code_host
        .find_open_pull_request_for_branch(&repository, mapping.worktree.branch.as_str())
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "{command_id} failed to resolve PR via VCS fallback for work item '{}': {detail}",
                mapping.work_item_id.as_str(),
            ))
        })?;

    let Some(pr) = fallback else {
        return resolve_pull_request_for_mapping_with_command(store, mapping, command_id);
    };
    persist_pull_request_artifact_from_vcs_fallback(store, mapping, command_id, &pr)?;
    Ok(pr)
}

fn is_pull_request_artifact_candidate(kind: &ArtifactKind, url: &str) -> bool {
    matches!(kind, ArtifactKind::PR)
        || (matches!(kind, ArtifactKind::Link) && looks_like_pull_request_url(url))
}

fn looks_like_pull_request_url(url: &str) -> bool {
    let normalized = normalize_pull_request_url(url).to_ascii_lowercase();
    normalized.contains("/pull/")
        || normalized.contains("pullrequest")
        || normalized.contains("pull-request")
}

async fn execute_workflow_approve_pr_ready<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let command_id = command_ids::WORKFLOW_APPROVE_PR_READY;
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context(&store, &context)?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store, &runtime, command_id, code_host,
    )
    .await?;

    code_host
        .mark_ready_for_review(&pr)
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "workflow.approve_pr_ready failed for PR #{}: {detail}",
                pr.number
            ))
        })?;

    runtime_command_response(&format!(
        "workflow.approve_pr_ready command completed for PR #{}",
        pr.number
    ))
}

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

fn append_workflow_transition_event_for_runtime(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    from: &WorkflowState,
    to: &WorkflowState,
    reason: WorkflowTransitionReason,
    guards: &WorkflowGuardContext,
    command_id: &str,
) -> Result<WorkflowState, CoreError> {
    let next = apply_workflow_transition(from, to, &reason, guards).map_err(|error| {
        CoreError::InvalidCommandArgs {
            command_id: command_id.to_owned(),
            reason: format!(
                "workflow transition validation failed for work item '{}': {error}",
                mapping.work_item_id.as_str()
            ),
        }
    })?;
    let session_id = mapping.session.session_id.clone();
    store.append(NewEventEnvelope {
        event_id: next_event_id("workflow-transition"),
        occurred_at: now_timestamp(),
        work_item_id: Some(mapping.work_item_id.clone()),
        session_id: Some(session_id),
        payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
            work_item_id: mapping.work_item_id.clone(),
            from: from.clone(),
            to: to.clone(),
            reason: Some(reason),
        }),
        schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
    })?;
    Ok(next)
}

async fn execute_workflow_reconcile_pr_merge<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let command_id = command_ids::WORKFLOW_RECONCILE_PR_MERGE;
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context_with_command(&store, &context, command_id)?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store, &runtime, command_id, code_host,
    )
    .await?;

    let merge_state = code_host
        .get_pull_request_merge_state(&pr)
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "{command_id} failed for PR #{}: {detail}",
                pr.number
            ))
        })?;

    if !merge_state.merged {
        let message = json!({
            "command": command_id,
            "work_item_id": runtime.work_item_id.as_str(),
            "merged": false,
            "merge_conflict": merge_state.merge_conflict,
            "base_branch": merge_state.base_branch,
            "head_branch": merge_state.head_branch,
            "completed": false,
            "transitions": []
        })
        .to_string();
        return runtime_command_response(message.as_str());
    }

    let mut transitions = Vec::new();
    let mut current = latest_workflow_state_for_work_item(&store, &runtime.work_item_id)?;

    loop {
        if current == WorkflowState::Done {
            break;
        }

        if current == WorkflowState::ReadyForReview {
            let next = append_workflow_transition_event_for_runtime(
                &mut store,
                &runtime,
                &current,
                &WorkflowState::InReview,
                WorkflowTransitionReason::ReviewStarted,
                &WorkflowGuardContext::default(),
                command_id,
            )?;
            transitions.push("ReadyForReview->InReview".to_owned());
            current = next;
            continue;
        }

        if current == WorkflowState::InReview {
            let next = append_workflow_transition_event_for_runtime(
                &mut store,
                &runtime,
                &current,
                &WorkflowState::Done,
                WorkflowTransitionReason::ReviewApprovedAndMerged,
                &WorkflowGuardContext {
                    merge_completed: true,
                    ..WorkflowGuardContext::default()
                },
                command_id,
            )?;
            transitions.push("InReview->Done".to_owned());
            current = next;
            continue;
        }

        break;
    }

    let message = json!({
        "command": command_id,
        "work_item_id": runtime.work_item_id.as_str(),
        "merged": merge_state.merged,
        "merge_conflict": merge_state.merge_conflict,
        "base_branch": merge_state.base_branch,
        "head_branch": merge_state.head_branch,
        "completed": current == WorkflowState::Done,
        "transitions": transitions
    })
    .to_string();

    runtime_command_response(message.as_str())
}

async fn execute_workflow_merge_pr<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let command_id = command_ids::WORKFLOW_MERGE_PR;
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context_with_command(&store, &context, command_id)?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store, &runtime, command_id, code_host,
    )
    .await?;

    code_host.merge_pull_request(&pr).await.map_err(|error| {
        let detail = sanitize_error_display_text(error.to_string().as_str());
        CoreError::DependencyUnavailable(format!(
            "{command_id} failed for PR #{}: {detail}",
            pr.number
        ))
    })?;

    execute_workflow_reconcile_pr_merge(code_host, event_store_path, context).await
}

fn append_supervisor_event(
    event_store_path: &str,
    event_id: String,
    occurred_at: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    payload: OrchestrationEventPayload,
) -> Result<(), CoreError> {
    let mut store = open_event_store(event_store_path)?;
    store.append(NewEventEnvelope {
        event_id,
        occurred_at,
        work_item_id,
        session_id,
        payload,
        schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
    })?;
    Ok(())
}

fn append_supervisor_event_best_effort(
    event_store_path: &str,
    event_id: String,
    occurred_at: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    payload: OrchestrationEventPayload,
) {
    if let Err(error) = append_supervisor_event(
        event_store_path,
        event_id,
        occurred_at,
        work_item_id,
        session_id,
        payload,
    ) {
        warn!(
            event_store_path,
            "failed to append supervisor lifecycle event: {error}"
        );
    }
}

struct SupervisorLifecycleRecordingStream {
    event_store_path: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    query_id: String,
    stream_id: String,
    started_at: String,
    started_time: SystemTime,
    cancellation_requested_by_user: Arc<AtomicBool>,
    chunk_count: u32,
    output_chars: u32,
    latest_usage: Option<LlmTokenUsage>,
    finished: bool,
    inner: LlmResponseStream,
}

impl SupervisorLifecycleRecordingStream {
    fn new(
        event_store_path: String,
        work_item_id: Option<WorkItemId>,
        session_id: Option<WorkerSessionId>,
        query_id: String,
        stream_id: String,
        started_at: String,
        started_time: SystemTime,
        cancellation_requested_by_user: Arc<AtomicBool>,
        inner: LlmResponseStream,
    ) -> Self {
        Self {
            event_store_path,
            work_item_id,
            session_id,
            query_id,
            stream_id,
            started_at,
            started_time,
            cancellation_requested_by_user,
            chunk_count: 0,
            output_chars: 0,
            latest_usage: None,
            finished: false,
            inner,
        }
    }

    fn observe_chunk(&mut self, chunk: &LlmStreamChunk) {
        let delta_chars = u32::try_from(chunk.delta.chars().count()).unwrap_or(u32::MAX);
        self.chunk_count = self.chunk_count.saturating_add(1);
        self.output_chars = self.output_chars.saturating_add(delta_chars);
        if let Some(usage) = chunk.usage.as_ref() {
            self.latest_usage = Some(usage.clone());
        }

        let observed_at = now_timestamp();
        append_supervisor_event_best_effort(
            self.event_store_path.as_str(),
            next_event_id("supervisor-query-chunk"),
            observed_at.clone(),
            self.work_item_id.clone(),
            self.session_id.clone(),
            OrchestrationEventPayload::SupervisorQueryChunk(SupervisorQueryChunkPayload {
                query_id: self.query_id.clone(),
                stream_id: self.stream_id.clone(),
                chunk_index: self.chunk_count,
                observed_at,
                delta_chars,
                cumulative_output_chars: self.output_chars,
                usage: chunk.usage.clone(),
                finish_reason: chunk.finish_reason.clone(),
            }),
        );
    }

    fn finalize(
        &mut self,
        reason: LlmFinishReason,
        usage: Option<LlmTokenUsage>,
        error: Option<String>,
    ) {
        if self.finished {
            return;
        }

        self.finished = true;
        let finished_at = now_timestamp();
        let duration_ms = SystemTime::now()
            .duration_since(self.started_time)
            .unwrap_or_default()
            .as_millis();
        let duration_ms = u64::try_from(duration_ms).unwrap_or(u64::MAX);
        let final_usage = usage.or_else(|| self.latest_usage.clone());

        let cancellation_source = if reason == LlmFinishReason::Cancelled {
            if self.cancellation_requested_by_user.load(Ordering::Relaxed) {
                Some(SupervisorQueryCancellationSource::UserInitiated)
            } else {
                Some(SupervisorQueryCancellationSource::Runtime)
            }
        } else {
            None
        };

        append_supervisor_event_best_effort(
            self.event_store_path.as_str(),
            next_event_id("supervisor-query-finished"),
            finished_at.clone(),
            self.work_item_id.clone(),
            self.session_id.clone(),
            OrchestrationEventPayload::SupervisorQueryFinished(SupervisorQueryFinishedPayload {
                query_id: self.query_id.clone(),
                stream_id: self.stream_id.clone(),
                started_at: self.started_at.clone(),
                finished_at,
                duration_ms,
                finish_reason: reason,
                chunk_count: self.chunk_count,
                output_chars: self.output_chars,
                usage: final_usage,
                error,
                cancellation_source,
            }),
        );

        let _ = remove_active_supervisor_query_for_query(
            self.event_store_path.as_str(),
            self.stream_id.as_str(),
            self.query_id.as_str(),
        );
    }
}

impl Drop for SupervisorLifecycleRecordingStream {
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        let source = if self.cancellation_requested_by_user.load(Ordering::Relaxed) {
            SupervisorQueryCancellationSource::UserInitiated
        } else {
            SupervisorQueryCancellationSource::Runtime
        };

        self.finalize(
            LlmFinishReason::Cancelled,
            None,
            Some("supervisor stream dropped before terminal chunk".to_owned()),
        );

        if source == SupervisorQueryCancellationSource::Runtime {
            let cancelled_at = now_timestamp();
            append_supervisor_event_best_effort(
                self.event_store_path.as_str(),
                next_event_id("supervisor-query-cancelled"),
                cancelled_at.clone(),
                self.work_item_id.clone(),
                self.session_id.clone(),
                OrchestrationEventPayload::SupervisorQueryCancelled(
                    SupervisorQueryCancelledPayload {
                        query_id: self.query_id.clone(),
                        stream_id: self.stream_id.clone(),
                        cancelled_at,
                        source,
                    },
                ),
            );
        }
    }
}

#[async_trait::async_trait]
impl LlmResponseSubscription for SupervisorLifecycleRecordingStream {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
        match self.inner.next_chunk().await {
            Ok(Some(chunk)) => {
                self.observe_chunk(&chunk);
                if let Some(reason) = chunk.finish_reason.clone() {
                    self.finalize(reason, chunk.usage.clone(), None);
                }
                Ok(Some(chunk))
            }
            Ok(None) => {
                self.finalize(LlmFinishReason::Stop, None, None);
                Ok(None)
            }
            Err(error) => {
                self.finalize(LlmFinishReason::Error, None, Some(error.to_string()));
                Err(error)
            }
        }
    }
}

pub(crate) fn record_user_initiated_supervisor_cancel(event_store_path: &str, stream_id: &str) {
    let Some(state) = mark_user_cancellation_requested(event_store_path, stream_id) else {
        return;
    };

    let cancelled_at = now_timestamp();
    append_supervisor_event_best_effort(
        event_store_path,
        next_event_id("supervisor-query-cancelled"),
        cancelled_at.clone(),
        state.work_item_id,
        state.session_id,
        OrchestrationEventPayload::SupervisorQueryCancelled(SupervisorQueryCancelledPayload {
            query_id: state.query_id,
            stream_id: stream_id.to_owned(),
            cancelled_at,
            source: SupervisorQueryCancellationSource::UserInitiated,
        }),
    );
}

async fn execute_supervisor_query<P>(
    supervisor: &P,
    ticketing: &(dyn TicketingProvider + Send + Sync),
    event_store_path: &str,
    args: SupervisorQueryArgs,
    fallback_context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let context = merge_supervisor_query_context(fallback_context, args_context(&args));
    let scope = resolve_supervisor_query_scope(&context)?;
    let store = open_event_store(event_store_path)?;
    let query_engine = SupervisorQueryEngine::default();
    let context_pack =
        query_engine.build_context_pack_with_filters(&store, scope.clone(), &context)?;
    let messages = match &args {
        SupervisorQueryArgs::Template {
            template,
            variables,
            ..
        } => build_template_messages_with_variables(template.as_str(), variables, &context_pack)?,
        SupervisorQueryArgs::Freeform { query, .. } => {
            build_freeform_messages(query.as_str(), &context_pack)?
        }
    };

    let mut messages = messages;
    let mut queued_chunks = VecDeque::new();
    let mut completed = false;
    let stream_id = next_runtime_stream_id();

    for iteration in 0..MAX_TOOL_LOOP_ITERATIONS {
        let turn = run_supervisor_turn(supervisor, messages.clone()).await?;
        for chunk in turn.chunks {
            queued_chunks.push_back(chunk);
        }

        if matches!(turn.finish_reason, LlmFinishReason::ToolCall) {
            if turn.assistant_message.tool_calls.is_empty() {
                return Err(CoreError::DependencyUnavailable(
                    "assistant requested a tool call but did not provide tool arguments".to_owned(),
                ));
            }

            messages.push(turn.assistant_message.clone());
            let tool_calls = turn.assistant_message.tool_calls.clone();

            for call in &tool_calls {
                queued_chunks.push_back(tool_progress_chunk(call));
            }

            let outputs = execute_tool_calls(ticketing, tool_calls.clone()).await?;
            let names = tool_calls
                .iter()
                .map(|call| (call.id.as_str(), call.name.as_str()))
                .collect::<std::collections::HashMap<_, _>>();

            for output in outputs {
                let call_name = names
                    .get(output.tool_call_id.as_str())
                    .copied()
                    .unwrap_or("tool");
                queued_chunks.push_back(LlmStreamChunk {
                    delta: format!(
                        "{TOOL_RESULT_PREFIX} {call_name} {} {}",
                        output.tool_call_id, output.output
                    ),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    usage: None,
                    rate_limit: None,
                });
                messages.push(LlmMessage {
                    role: LlmRole::Tool,
                    content: output.output,
                    name: Some(call_name.to_owned()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(output.tool_call_id),
                });
            }

            if iteration + 1 >= MAX_TOOL_LOOP_ITERATIONS {
                return Err(CoreError::DependencyUnavailable(format!(
                    "tool calling exceeded loop limit of {MAX_TOOL_LOOP_ITERATIONS} iterations"
                )));
            }

            continue;
        }

        completed = matches!(
            turn.finish_reason,
            LlmFinishReason::Stop
                | LlmFinishReason::Length
                | LlmFinishReason::ContentFilter
                | LlmFinishReason::Cancelled
                | LlmFinishReason::Error
        );
        break;
    }

    if !completed {
        return Err(CoreError::DependencyUnavailable(format!(
            "tool-calling did not complete within {MAX_TOOL_LOOP_ITERATIONS} turns"
        )));
    }

    let stream = ToolingStream::new(queued_chunks.into_iter().collect());

    let query_id = next_supervisor_query_id();
    let started_at = now_timestamp();
    let started_time = SystemTime::now();
    let (work_item_id, session_id) = event_scope_identifiers(&scope, &context);
    let cancellation_requested_by_user = Arc::new(AtomicBool::new(false));

    append_supervisor_event(
        event_store_path,
        next_event_id("supervisor-query-started"),
        started_at.clone(),
        work_item_id.clone(),
        session_id.clone(),
        OrchestrationEventPayload::SupervisorQueryStarted(SupervisorQueryStartedPayload {
            query_id: query_id.clone(),
            stream_id: stream_id.clone(),
            scope: scope_label(&scope),
            started_at: started_at.clone(),
            kind: query_kind(&args),
            template: query_template(&args),
            query: query_text(&args),
        }),
    )?;

    register_active_supervisor_query(
        event_store_path,
        stream_id.as_str(),
        ActiveSupervisorQueryState {
            query_id: query_id.clone(),
            work_item_id: work_item_id.clone(),
            session_id: session_id.clone(),
            cancellation_requested_by_user: cancellation_requested_by_user.clone(),
        },
    );

    let stream = SupervisorLifecycleRecordingStream::new(
        event_store_path.to_owned(),
        work_item_id,
        session_id,
        query_id,
        stream_id.clone(),
        started_at,
        started_time,
        cancellation_requested_by_user,
        Box::new(stream),
    );

    Ok((stream_id, Box::new(stream)))
}

async fn execute_github_open_review_tabs_with_opener(
    code_host: &impl CodeHostProvider,
    event_store_path: &str,
    context: SupervisorCommandContext,
    url_opener: &impl UrlOpener,
) -> Result<(String, LlmResponseStream), CoreError> {
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context_with_command(
        &store,
        &context,
        command_ids::GITHUB_OPEN_REVIEW_TABS,
    )?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store,
        &runtime,
        command_ids::GITHUB_OPEN_REVIEW_TABS,
        code_host,
    )
    .await?;

    let command_id = command_ids::GITHUB_OPEN_REVIEW_TABS;
    UrlOpener::open_url(url_opener, pr.url.as_str())
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "{command_id} failed to open PR URL '{}': {detail}",
                pr.url,
            ))
        })?;
    runtime_command_response(&format!("{GITHUB_OPEN_REVIEW_TABS_ACK} {}", pr.url))
}

async fn execute_github_open_review_tabs(
    code_host: &impl CodeHostProvider,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError> {
    let opener = default_system_url_opener()?;
    execute_github_open_review_tabs_with_opener(code_host, event_store_path, context, &opener).await
}

pub(crate) async fn dispatch_supervisor_runtime_command<P>(
    supervisor: &P,
    code_host: &impl CodeHostProvider,
    ticketing: &(dyn TicketingProvider + Send + Sync),
    event_store_path: &str,
    invocation: UntypedCommandInvocation,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let command = CommandRegistry::default().parse_invocation(&invocation)?;
    match command {
        Command::SupervisorQuery(args) => {
            execute_supervisor_query(supervisor, ticketing, event_store_path, args, context).await
        }
        Command::WorkflowApprovePrReady => {
            execute_workflow_approve_pr_ready(code_host, event_store_path, context).await
        }
        Command::WorkflowReconcilePrMerge => {
            execute_workflow_reconcile_pr_merge(code_host, event_store_path, context).await
        }
        Command::WorkflowMergePr => {
            execute_workflow_merge_pr(code_host, event_store_path, context).await
        }
        Command::GithubOpenReviewTabs => {
            execute_github_open_review_tabs(code_host, event_store_path, context).await
        }
        command => Err(invalid_supervisor_runtime_usage(command.id())),
    }
}
