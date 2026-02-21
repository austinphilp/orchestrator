use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use orchestrator_core::{
    ArchiveTicketRequest, CodeHostProvider, Command, CommandRegistry, CoreError, CreateTicketRequest,
    GithubClient, LlmChatRequest, LlmMessage, LlmProvider, LlmRole, ProjectionState,
    SelectedTicketFlowResult, Supervisor, TicketQuery, TicketSummary, TicketingProvider,
    VcsProvider, WorkerBackend, WorkerSessionId, WorkerSessionStatus, WorkflowState,
};
use orchestrator_ui::{
    CiCheckStatus, CreateTicketFromPickerRequest, InboxPublishRequest, InboxResolveRequest,
    MergeQueueCommandKind, MergeQueueEvent, SessionWorktreeDiff, SessionWorkflowAdvanceOutcome,
    SupervisorCommandContext, SupervisorCommandDispatcher, TicketPickerProvider,
};
use std::path::PathBuf;
use tokio::sync::{mpsc, watch, Mutex};
use tracing::warn;

use crate::App;

pub struct AppTicketPickerProvider<S, G, V>
where
    S: Supervisor + 'static,
    G: GithubClient + 'static,
    V: VcsProvider + 'static,
{
    app: Arc<App<S, G>>,
    ticketing: Arc<dyn TicketingProvider + Send + Sync>,
    vcs: Arc<V>,
    worker_backend: Arc<dyn WorkerBackend + Send + Sync>,
    pr_pipeline_polling: Mutex<Option<PrPipelinePollingControl>>,
}

struct PrPipelinePollingControl {
    merge_request_sender: mpsc::Sender<WorkerSessionId>,
    shutdown_sender: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl<S, G, V> AppTicketPickerProvider<S, G, V>
where
    S: Supervisor + 'static,
    G: GithubClient + 'static,
    V: VcsProvider + 'static,
{
    pub fn new(
        app: Arc<App<S, G>>,
        ticketing: Arc<dyn TicketingProvider + Send + Sync>,
        vcs: Arc<V>,
        worker_backend: Arc<dyn WorkerBackend + Send + Sync>,
    ) -> Self {
        Self {
            app,
            ticketing,
            vcs,
            worker_backend,
            pr_pipeline_polling: Mutex::new(None),
        }
    }
}

#[async_trait]
impl<S, G, V> TicketPickerProvider for AppTicketPickerProvider<S, G, V>
where
    S: Supervisor + LlmProvider + Send + Sync + 'static,
    G: GithubClient + CodeHostProvider + Send + Sync + 'static,
    V: VcsProvider + Send + Sync + 'static,
{
    async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
        let mut tickets = self
            .ticketing
            .list_tickets(TicketQuery {
                assigned_to_me: true,
                states: Vec::new(),
                search: None,
                limit: None,
            })
            .await?;

        tickets.retain(|ticket| is_unfinished_ticket_state(ticket.state.as_str()));

        Ok(tickets)
    }

    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        self.ticketing.list_projects().await
    }

    async fn start_or_resume_ticket(
        &self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) -> Result<SelectedTicketFlowResult, CoreError> {
        let app = self.app.clone();
        let ticket = ticket.clone();
        let vcs = self.vcs.clone();
        let worker_backend = self.worker_backend.clone();

        tokio::task::spawn_blocking(move || -> Result<SelectedTicketFlowResult, CoreError> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    CoreError::Configuration(format!(
                        "failed to initialize ticket picker runtime: {error}"
                    ))
                })?;

            runtime.block_on(async {
                app.start_or_resume_selected_ticket(
                    &ticket,
                    repository_override,
                    vcs.as_ref(),
                    worker_backend.as_ref(),
                )
                .await
            })
        })
        .await
        .map_err(|error| CoreError::Configuration(format!("ticket picker task failed: {error}")))?
    }

    async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.projection_state())
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while reloading projection: {error}"
                ))
            })?
    }

    async fn create_ticket_from_brief(
        &self,
        request: CreateTicketFromPickerRequest,
    ) -> Result<TicketSummary, CoreError> {
        let brief = request.brief.trim();
        if brief.is_empty() {
            return Err(CoreError::InvalidCommandArgs {
                command_id: "ui.ticket_picker.create".to_owned(),
                reason: "ticket brief cannot be empty".to_owned(),
            });
        }

        let (title, description) = draft_ticket_from_brief(&self.app.supervisor, brief).await?;
        let project = request
            .selected_project
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("No Project"));
        let ticket = self
            .ticketing
            .create_ticket(CreateTicketRequest {
                title,
                description: Some(description),
                project,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: true,
            })
            .await?;

        Ok(ticket)
    }

    async fn archive_ticket(&self, ticket: TicketSummary) -> Result<(), CoreError> {
        self.ticketing
            .archive_ticket(ArchiveTicketRequest {
                ticket_id: ticket.ticket_id,
            })
            .await
    }

    async fn archive_session(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<SessionArchiveOutcome, CoreError> {
        let app = self.app.clone();
        let worker_backend = self.worker_backend.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    CoreError::Configuration(format!(
                        "failed to initialize session-archive runtime: {error}"
                    ))
                })?;

            runtime.block_on(async {
                app.archive_session(&session_id, worker_backend.as_ref())
                    .await
            })
        })
        .await
        .map_err(|error| {
            CoreError::Configuration(format!(
                "ticket picker task failed while archiving session: {error}"
            ))
        })?
    }

    async fn mark_session_crashed(
        &self,
        session_id: WorkerSessionId,
        reason: String,
    ) -> Result<(), CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.mark_session_crashed(&session_id, reason.as_str()))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while marking session crashed: {error}"
                ))
            })?
    }

    async fn set_session_working_state(
        &self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) -> Result<(), CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || {
            app.set_session_working_state(&session_id, is_working)
        })
        .await
        .map_err(|error| {
            CoreError::Configuration(format!(
                "ticket picker task failed while persisting session working state: {error}"
            ))
        })?
    }

    async fn session_worktree_diff(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<SessionWorktreeDiff, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.session_worktree_diff(&session_id))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while loading session diff: {error}"
                ))
            })?
    }

    async fn advance_session_workflow(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<SessionWorkflowAdvanceOutcome, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.advance_session_workflow(&session_id))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while advancing session workflow: {error}"
                ))
            })?
    }

    async fn complete_session_after_merge(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<SessionMergeFinalizeOutcome, CoreError> {
        let app = self.app.clone();
        let worker_backend = self.worker_backend.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    CoreError::Configuration(format!(
                        "failed to initialize merge-finalization runtime: {error}"
                    ))
                })?;

            runtime.block_on(async {
                app.complete_session_after_merge(&session_id, worker_backend.as_ref())
                    .await
            })
        })
        .await
        .map_err(|error| {
            CoreError::Configuration(format!(
                "ticket picker task failed while finalizing merged session: {error}"
            ))
        })?
    }

    async fn publish_inbox_item(
        &self,
        request: InboxPublishRequest,
    ) -> Result<StoredEventEnvelope, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.publish_inbox_item(&request))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while publishing inbox item: {error}"
                ))
            })?
    }

    async fn resolve_inbox_item(
        &self,
        request: InboxResolveRequest,
    ) -> Result<Option<StoredEventEnvelope>, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.resolve_inbox_item(&request))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while resolving inbox item: {error}"
                ))
            })?
    }

    async fn start_pr_pipeline_polling(
        &self,
        sender: mpsc::Sender<MergeQueueEvent>,
    ) -> Result<(), CoreError> {
        let mut guard = self.pr_pipeline_polling.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let (merge_request_sender, merge_request_receiver) = mpsc::channel(32);
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let poll_interval = Duration::from_secs(
            self.app
                .config
                .runtime
                .pr_pipeline_poll_interval_secs
                .max(1),
        );
        let app = self.app.clone();
        let task = tokio::spawn(async move {
            run_pr_pipeline_polling_task(
                app,
                sender,
                merge_request_receiver,
                shutdown_receiver,
                poll_interval,
            )
            .await;
        });
        *guard = Some(PrPipelinePollingControl {
            merge_request_sender,
            shutdown_sender,
            task,
        });
        Ok(())
    }

    async fn stop_pr_pipeline_polling(&self) -> Result<(), CoreError> {
        let control = {
            let mut guard = self.pr_pipeline_polling.lock().await;
            guard.take()
        };
        if let Some(control) = control {
            let _ = control.shutdown_sender.send(true);
            let _ = control.task.await;
        }
        Ok(())
    }

    async fn enqueue_pr_merge(&self, session_id: WorkerSessionId) -> Result<(), CoreError> {
        let sender = {
            let guard = self.pr_pipeline_polling.lock().await;
            guard
                .as_ref()
                .map(|control| control.merge_request_sender.clone())
        }
        .ok_or_else(|| {
            CoreError::DependencyUnavailable(
                "PR merge queue is unavailable: polling service is not running".to_owned(),
            )
        })?;
        sender
            .send(session_id)
            .await
            .map_err(|error| CoreError::DependencyUnavailable(format!("PR merge queue closed: {error}")))
    }
}

async fn run_pr_pipeline_polling_task<S, G>(
    app: Arc<App<S, G>>,
    sender: mpsc::Sender<MergeQueueEvent>,
    mut merge_request_receiver: mpsc::Receiver<WorkerSessionId>,
    mut shutdown_receiver: watch::Receiver<bool>,
    poll_interval: Duration,
) where
    S: Supervisor + LlmProvider + Send + Sync + 'static,
    G: GithubClient + CodeHostProvider + Send + Sync + 'static,
{
    let mut ticker = tokio::time::interval(poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown_receiver.changed() => {
                break;
            }
            maybe_session_id = merge_request_receiver.recv() => {
                let Some(session_id) = maybe_session_id else {
                    break;
                };
                match load_projection_state(app.clone()).await {
                    Ok(projection) => {
                        if let Some(context) = supervisor_context_for_session(&projection, &session_id) {
                            dispatch_merge_queue_command(app.clone(), sender.clone(), session_id, MergeQueueCommandKind::Merge, context).await;
                        } else {
                            send_merge_queue_error(
                                &sender,
                                session_id,
                                MergeQueueCommandKind::Merge,
                                "workflow.merge_pr requires an active session context",
                            ).await;
                        }
                    }
                    Err(error) => {
                        send_merge_queue_error(
                            &sender,
                            session_id,
                            MergeQueueCommandKind::Merge,
                            error.to_string().as_str(),
                        ).await;
                    }
                }
            }
            _ = ticker.tick() => {
                match load_projection_state(app.clone()).await {
                    Ok(projection) => {
                        for (session_id, context) in review_session_contexts(&projection) {
                            dispatch_merge_queue_command(
                                app.clone(),
                                sender.clone(),
                                session_id,
                                MergeQueueCommandKind::Reconcile,
                                context,
                            ).await;
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "failed to load projection for PR/pipeline polling");
                    }
                }
            }
        }
    }
}

async fn load_projection_state<S, G>(app: Arc<App<S, G>>) -> Result<ProjectionState, CoreError>
where
    S: Supervisor + LlmProvider + Send + Sync + 'static,
    G: GithubClient + CodeHostProvider + Send + Sync + 'static,
{
    tokio::task::spawn_blocking(move || app.projection_state())
        .await
        .map_err(|error| CoreError::Configuration(format!("projection polling task failed: {error}")))?
}

fn review_session_contexts(projection: &ProjectionState) -> Vec<(WorkerSessionId, SupervisorCommandContext)> {
    projection
        .sessions
        .iter()
        .filter_map(|(session_id, session)| {
            if !is_open_session_status(session.status.as_ref()) {
                return None;
            }
            if !session_is_in_review_stage(projection, session_id) {
                return None;
            }
            supervisor_context_for_session(projection, session_id).map(|context| (session_id.clone(), context))
        })
        .collect()
}

fn supervisor_context_for_session(
    projection: &ProjectionState,
    session_id: &WorkerSessionId,
) -> Option<SupervisorCommandContext> {
    let session = projection.sessions.get(session_id)?;
    let work_item_id = session
        .work_item_id
        .as_ref()
        .map(|id| id.as_str().to_owned());
    Some(SupervisorCommandContext {
        selected_work_item_id: work_item_id,
        selected_session_id: Some(session_id.as_str().to_owned()),
        scope: Some(format!("session:{}", session_id.as_str())),
    })
}

fn session_is_in_review_stage(projection: &ProjectionState, session_id: &WorkerSessionId) -> bool {
    projection
        .sessions
        .get(session_id)
        .and_then(|session| session.work_item_id.as_ref())
        .and_then(|work_item_id| projection.work_items.get(work_item_id))
        .and_then(|work_item| work_item.workflow_state.as_ref())
        .map(|state| {
            matches!(
                state,
                WorkflowState::AwaitingYourReview
                    | WorkflowState::ReadyForReview
                    | WorkflowState::InReview
                    | WorkflowState::PendingMerge
            )
        })
        .unwrap_or(false)
}

fn is_open_session_status(status: Option<&WorkerSessionStatus>) -> bool {
    !matches!(
        status,
        Some(WorkerSessionStatus::Done) | Some(WorkerSessionStatus::Crashed)
    )
}

async fn dispatch_merge_queue_command<S, G>(
    app: Arc<App<S, G>>,
    sender: mpsc::Sender<MergeQueueEvent>,
    session_id: WorkerSessionId,
    kind: MergeQueueCommandKind,
    context: SupervisorCommandContext,
) where
    S: Supervisor + LlmProvider + Send + Sync + 'static,
    G: GithubClient + CodeHostProvider + Send + Sync + 'static,
{
    let command = match kind {
        MergeQueueCommandKind::Reconcile => Command::WorkflowReconcilePrMerge,
        MergeQueueCommandKind::Merge => Command::WorkflowMergePr,
    };
    let invocation = match CommandRegistry::default().to_untyped_invocation(&command) {
        Ok(invocation) => invocation,
        Err(error) => {
            send_merge_queue_error(&sender, session_id, kind, error.to_string().as_str()).await;
            return;
        }
    };

    let (_, mut stream) = match SupervisorCommandDispatcher::dispatch_supervisor_command(
        app.as_ref(),
        invocation,
        context,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            send_merge_queue_error(&sender, session_id, kind, error.to_string().as_str()).await;
            return;
        }
    };

    let mut output = String::new();
    loop {
        match stream.next_chunk().await {
            Ok(Some(chunk)) => output.push_str(chunk.delta.as_str()),
            Ok(None) => break,
            Err(error) => {
                send_merge_queue_error(&sender, session_id, kind, error.to_string().as_str()).await;
                return;
            }
        }
    }

    let parsed = parse_merge_queue_response(output.as_str());
    let _ = sender
        .send(MergeQueueEvent::Completed {
            session_id,
            kind,
            completed: parsed.completed,
            merge_conflict: parsed.merge_conflict,
            base_branch: parsed.base_branch,
            head_branch: parsed.head_branch,
            ci_checks: parsed.ci_checks,
            ci_failures: parsed.ci_failures,
            ci_has_failures: parsed.ci_has_failures,
            ci_status_error: parsed.ci_status_error,
            error: None,
        })
        .await;
}

async fn send_merge_queue_error(
    sender: &mpsc::Sender<MergeQueueEvent>,
    session_id: WorkerSessionId,
    kind: MergeQueueCommandKind,
    detail: &str,
) {
    let _ = sender
        .send(MergeQueueEvent::Completed {
            session_id,
            kind,
            completed: false,
            merge_conflict: false,
            base_branch: None,
            head_branch: None,
            ci_checks: Vec::new(),
            ci_failures: Vec::new(),
            ci_has_failures: false,
            ci_status_error: None,
            error: Some(sanitize_terminal_display_text(detail)),
        })
        .await;
}

#[derive(Debug, Default)]
struct MergeQueueResponse {
    completed: bool,
    merge_conflict: bool,
    base_branch: Option<String>,
    head_branch: Option<String>,
    ci_checks: Vec<CiCheckStatus>,
    ci_failures: Vec<String>,
    ci_has_failures: bool,
    ci_status_error: Option<String>,
}

fn parse_merge_queue_response(output: &str) -> MergeQueueResponse {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return MergeQueueResponse::default();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return MergeQueueResponse::default();
    };

    MergeQueueResponse {
        completed: value
            .get("completed")
            .and_then(|entry| entry.as_bool())
            .unwrap_or(false),
        merge_conflict: value
            .get("merge_conflict")
            .and_then(|entry| entry.as_bool())
            .unwrap_or(false),
        base_branch: value
            .get("base_branch")
            .and_then(|entry| entry.as_str())
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty()),
        head_branch: value
            .get("head_branch")
            .and_then(|entry| entry.as_str())
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty()),
        ci_checks: value
            .get("ci_statuses")
            .and_then(|entry| entry.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let name = row
                            .get("name")
                            .and_then(|entry| entry.as_str())
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_owned)?;
                        let workflow = row
                            .get("workflow")
                            .and_then(|entry| entry.as_str())
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_owned);
                        let bucket = row
                            .get("bucket")
                            .and_then(|entry| entry.as_str())
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_ascii_lowercase)
                            .unwrap_or_else(|| "unknown".to_owned());
                        let state = row
                            .get("state")
                            .and_then(|entry| entry.as_str())
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| bucket.clone());
                        let link = row
                            .get("link")
                            .and_then(|entry| entry.as_str())
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_owned);
                        Some(CiCheckStatus {
                            name,
                            workflow,
                            bucket,
                            state,
                            link,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        ci_failures: value
            .get("ci_failures")
            .and_then(|entry| entry.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|entry| entry.as_str())
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        ci_has_failures: value
            .get("ci_has_failures")
            .and_then(|entry| entry.as_bool())
            .unwrap_or(false),
        ci_status_error: value
            .get("ci_status_error")
            .and_then(|entry| entry.as_str())
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned),
    }
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

const MAX_GENERATED_TITLE_LEN: usize = 180;

async fn draft_ticket_from_brief(
    supervisor: &dyn LlmProvider,
    brief: &str,
) -> Result<(String, String), CoreError> {
    let (_stream_id, mut stream) = supervisor
        .stream_chat(LlmChatRequest {
            model: supervisor_model_from_env(),
            tools: Vec::new(),
            messages: vec![
                LlmMessage {
                    role: LlmRole::System,
                    content: concat!(
                        "You draft software engineering tickets.\n",
                        "Return plain text with exactly this shape:\n",
                        "Title: <one concise line>\n",
                        "Description:\n",
                        "<multi-line markdown description with sections for Summary, Scope, and Acceptance Criteria>\n",
                        "Do not include code fences."
                    )
                    .to_owned(),
                    name: None,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
                LlmMessage {
                    role: LlmRole::User,
                    content: format!("Brief:\n{brief}"),
                    name: None,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            ],
            temperature: Some(0.2),
            tool_choice: None,
            max_output_tokens: Some(700),
        })
        .await?;

    let mut draft = String::new();
    while let Some(chunk) = stream.next_chunk().await? {
        if !chunk.delta.is_empty() {
            draft.push_str(chunk.delta.as_str());
        }
        if chunk.finish_reason.is_some() {
            break;
        }
    }

    Ok(parse_ticket_draft(draft.as_str(), brief))
}

fn parse_ticket_draft(draft: &str, fallback_brief: &str) -> (String, String) {
    let mut title = String::new();
    let mut description_lines = Vec::new();
    let mut in_description = false;

    for raw_line in draft.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() && !in_description {
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("title:") && title.is_empty() {
            title = trimmed[6..].trim().to_owned();
            continue;
        }

        if lower.starts_with("description:") {
            in_description = true;
            let first = trimmed[12..].trim();
            if !first.is_empty() {
                description_lines.push(first.to_owned());
            }
            continue;
        }

        if in_description {
            description_lines.push(line.to_owned());
        } else if title.is_empty() {
            title = trimmed.to_owned();
        }
    }

    let title = title.trim();
    let used_fallback_title = title.is_empty();
    let title = if used_fallback_title {
        fallback_title_from_brief(fallback_brief)
    } else {
        truncate_to_char_boundary(title, MAX_GENERATED_TITLE_LEN).to_owned()
    };
    if used_fallback_title {
        warn!(
            draft_preview = %compact_preview(draft, 220),
            brief_preview = %compact_preview(fallback_brief, 120),
            "supervisor ticket draft omitted title; using fallback title derived from brief"
        );
    }

    let description = description_lines.join("\n").trim().to_owned();
    let description = if description.is_empty() {
        fallback_brief.trim().to_owned()
    } else {
        description
    };

    (title, description)
}

fn fallback_title_from_brief(brief: &str) -> String {
    let first_line = brief
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("New ticket");
    let candidate = first_line
        .trim_start_matches('#')
        .trim_start_matches('-')
        .trim();
    if candidate.is_empty() {
        "New ticket".to_owned()
    } else {
        truncate_to_char_boundary(candidate, MAX_GENERATED_TITLE_LEN).to_owned()
    }
}

fn compact_preview(value: &str, max_chars: usize) -> String {
    let single_line = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let compact = single_line.trim();
    if compact.is_empty() {
        return "<empty>".to_owned();
    }
    truncate_to_char_boundary(compact, max_chars).to_owned()
}

fn truncate_to_char_boundary(value: &str, max_chars: usize) -> &str {
    if value.chars().count() <= max_chars {
        return value;
    }

    let mut end = value.len();
    for (count, (idx, _)) in value.char_indices().enumerate() {
        if count == max_chars {
            end = idx;
            break;
        }
    }
    &value[..end]
}

fn supervisor_model_from_env() -> String {
    crate::supervisor_model_from_env()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{SessionProjection, WorkItemProjection};

    #[test]
    fn parse_ticket_draft_extracts_title_and_description_sections() {
        let draft = "\
Title: Add create ticket shortcut in picker
Description:
## Summary
Allow creating tickets with n.

## Scope
- add key handling
- create and start
";

        let (title, description) = parse_ticket_draft(draft, "fallback brief");
        assert_eq!(title, "Add create ticket shortcut in picker");
        assert!(description.contains("## Summary"));
        assert!(description.contains("## Scope"));
    }

    #[test]
    fn parse_ticket_draft_uses_fallback_when_description_missing() {
        let (title, description) = parse_ticket_draft("Title: New work item", "fallback brief text");
        assert_eq!(title, "New work item");
        assert_eq!(description, "fallback brief text");
    }

    #[test]
    fn parse_ticket_draft_uses_fallback_when_title_missing() {
        let (title, description) = parse_ticket_draft("Description:\nNo title", "fallback brief");
        assert_eq!(title, "fallback brief");
        assert_eq!(description, "No title");
    }

    #[test]
    fn parse_merge_queue_response_extracts_ci_status_payload() {
        let parsed = parse_merge_queue_response(
            r#"{
                "completed": false,
                "merge_conflict": true,
                "base_branch": "main",
                "head_branch": "ap/AP-278",
                "ci_statuses": [
                    {"name":"tests","workflow":"CI","bucket":"fail","state":"FAILURE","link":"https://example.com"}
                ],
                "ci_failures": ["CI / tests"],
                "ci_has_failures": true,
                "ci_status_error": null
            }"#,
        );
        assert!(!parsed.completed);
        assert!(parsed.merge_conflict);
        assert_eq!(parsed.base_branch.as_deref(), Some("main"));
        assert_eq!(parsed.head_branch.as_deref(), Some("ap/AP-278"));
        assert_eq!(parsed.ci_checks.len(), 1);
        assert_eq!(parsed.ci_checks[0].name, "tests");
        assert_eq!(parsed.ci_failures, vec!["CI / tests".to_owned()]);
        assert!(parsed.ci_has_failures);
    }

    #[test]
    fn review_session_contexts_filters_non_review_or_closed_sessions() {
        let review_session_id = WorkerSessionId::new("sess-review");
        let review_work_item_id = orchestrator_core::WorkItemId::new("wi-review");
        let done_session_id = WorkerSessionId::new("sess-done");
        let done_work_item_id = orchestrator_core::WorkItemId::new("wi-done");
        let planning_session_id = WorkerSessionId::new("sess-planning");
        let planning_work_item_id = orchestrator_core::WorkItemId::new("wi-planning");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            review_work_item_id.clone(),
            WorkItemProjection {
                id: review_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::InReview),
                session_id: Some(review_session_id.clone()),
                worktree_id: None,
                inbox_items: Vec::new(),
                artifacts: Vec::new(),
            },
        );
        projection.work_items.insert(
            done_work_item_id.clone(),
            WorkItemProjection {
                id: done_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::InReview),
                session_id: Some(done_session_id.clone()),
                worktree_id: None,
                inbox_items: Vec::new(),
                artifacts: Vec::new(),
            },
        );
        projection.work_items.insert(
            planning_work_item_id.clone(),
            WorkItemProjection {
                id: planning_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Planning),
                session_id: Some(planning_session_id.clone()),
                worktree_id: None,
                inbox_items: Vec::new(),
                artifacts: Vec::new(),
            },
        );
        projection.sessions.insert(
            review_session_id.clone(),
            SessionProjection {
                id: review_session_id.clone(),
                work_item_id: Some(review_work_item_id),
                status: Some(WorkerSessionStatus::Running),
                latest_checkpoint: None,
            },
        );
        projection.sessions.insert(
            done_session_id.clone(),
            SessionProjection {
                id: done_session_id.clone(),
                work_item_id: Some(done_work_item_id),
                status: Some(WorkerSessionStatus::Done),
                latest_checkpoint: None,
            },
        );
        projection.sessions.insert(
            planning_session_id.clone(),
            SessionProjection {
                id: planning_session_id,
                work_item_id: Some(planning_work_item_id),
                status: Some(WorkerSessionStatus::Running),
                latest_checkpoint: None,
            },
        );

        let sessions = review_session_contexts(&projection);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].0, review_session_id);
        assert_eq!(sessions[0].1.scope.as_deref(), Some("session:sess-review"));
    }
}

fn is_unfinished_ticket_state(state: &str) -> bool {
    let normalized = state.trim().to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "done" | "completed" | "canceled" | "cancelled"
    )
}
