use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::normalization::DOMAIN_EVENT_SCHEMA_VERSION;
use crate::{
    apply_workflow_transition, CoreError, CreateWorktreeRequest, DeleteWorktreeRequest, EventStore,
    NewEventEnvelope, OrchestrationEventPayload, ProjectId, RepositoryRef, RuntimeError,
    RuntimeMappingRecord, SessionHandle, SessionRecord, SessionSpawnedPayload, SpawnSpec,
    SqliteEventStore, TicketId, TicketProvider, TicketRecord, TicketSummary, VcsProvider,
    WorkItemCreatedPayload, WorkItemId, WorkerBackend, WorkerSessionId, WorkerSessionStatus,
    WorkflowGuardContext, WorkflowState, WorkflowTransitionPayload, WorkflowTransitionReason,
    WorktreeCreatedPayload, WorktreeId, WorktreeRecord,
};

const DEFAULT_BASE_BRANCH: &str = "main";
const DEFAULT_PROJECT_ID: &str = "project-default";
const DEFAULT_WORKTREE_SLUG: &str = "ticket";
const DEFAULT_WORKTREE_TITLE_SLUG_LIMIT: usize = 48;
const DEFAULT_WORKTREE_DIR: &str = ".orchestrator/worktrees";
const WORKTREE_BRANCH_PREFIX: &str = "ap/";

static EVENT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedTicketFlowConfig {
    pub repository_roots: Vec<PathBuf>,
    pub worktrees_root: PathBuf,
    pub project_id: ProjectId,
    pub base_branch: String,
    pub model: Option<String>,
}

impl SelectedTicketFlowConfig {
    pub fn from_workspace_root(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            repository_roots: vec![workspace_root.clone()],
            worktrees_root: workspace_root.join(DEFAULT_WORKTREE_DIR),
            project_id: ProjectId::new(DEFAULT_PROJECT_ID),
            base_branch: DEFAULT_BASE_BRANCH.to_owned(),
            model: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedTicketFlowAction {
    Started,
    Resumed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedTicketFlowResult {
    pub action: SelectedTicketFlowAction,
    pub mapping: RuntimeMappingRecord,
}

pub async fn start_or_resume_selected_ticket(
    store: &mut SqliteEventStore,
    selected_ticket: &TicketSummary,
    config: &SelectedTicketFlowConfig,
    vcs: &dyn VcsProvider,
    worker_backend: &dyn WorkerBackend,
) -> Result<SelectedTicketFlowResult, CoreError> {
    let (provider, provider_ticket_id) =
        parse_ticket_provider_identity(&selected_ticket.ticket_id)?;
    validate_flow_config(config)?;

    if let Some(existing_mapping) =
        store.find_runtime_mapping_by_ticket(&provider, provider_ticket_id.as_str())?
    {
        return resume_existing_mapping(
            store,
            selected_ticket,
            config,
            worker_backend,
            existing_mapping,
        )
        .await;
    }

    start_new_mapping(store, selected_ticket, config, vcs, worker_backend).await
}

async fn start_new_mapping(
    store: &mut SqliteEventStore,
    selected_ticket: &TicketSummary,
    config: &SelectedTicketFlowConfig,
    vcs: &dyn VcsProvider,
    worker_backend: &dyn WorkerBackend,
) -> Result<SelectedTicketFlowResult, CoreError> {
    let (provider, provider_ticket_id) =
        parse_ticket_provider_identity(&selected_ticket.ticket_id)?;
    let repository = resolve_single_repository(vcs, config).await?;
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
    let instruction = start_instruction(selected_ticket);

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
    ensure_lifecycle_events(store, selected_ticket, &mapping, &config.project_id, true)?;

    Ok(SelectedTicketFlowResult {
        action: SelectedTicketFlowAction::Started,
        mapping,
    })
}

async fn resume_existing_mapping(
    store: &mut SqliteEventStore,
    selected_ticket: &TicketSummary,
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

    let resume_message = resume_instruction(selected_ticket);
    let mut resume_bytes = resume_message.as_bytes().to_vec();
    if !resume_bytes.ends_with(b"\n") {
        resume_bytes.push(b'\n');
    }

    let handle = SessionHandle {
        session_id: existing_mapping.session.session_id.clone().into(),
        backend: backend_kind.clone(),
    };

    let mut spawned_new_runtime_session = false;
    if existing_mapping.session.status == WorkerSessionStatus::Crashed {
        spawn_resume_session(
            worker_backend,
            &existing_mapping,
            resume_model.clone(),
            resume_message,
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
) -> Result<(), CoreError> {
    let spawned = worker_backend
        .spawn(SpawnSpec {
            session_id: mapping.session.session_id.clone().into(),
            workdir: PathBuf::from(mapping.session.workdir.as_str()),
            model,
            instruction_prelude: Some(instruction),
            environment: Vec::new(),
        })
        .await?;

    if spawned.session_id.as_str() != mapping.session.session_id.as_str() {
        return Err(CoreError::Runtime(RuntimeError::Internal(format!(
            "worker backend returned mismatched resumed session id: expected '{}', got '{}'",
            mapping.session.session_id.as_str(),
            spawned.session_id.as_str()
        ))));
    }

    Ok(())
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
    let mut current_state = latest_workflow_state.unwrap_or(WorkflowState::New);

    if current_state == WorkflowState::New {
        current_state = append_workflow_transition_event(
            store,
            mapping,
            &current_state,
            &WorkflowState::Planning,
            WorkflowTransitionReason::TicketAccepted,
            &guard_context,
        )?;
    }

    if current_state != WorkflowState::Implementing {
        let reason = resume_to_implementing_reason(&current_state).ok_or_else(|| {
            CoreError::Configuration(format!(
                "Cannot automatically transition work item '{}' from '{current_state:?}' to Implementing.",
                mapping.work_item_id.as_str()
            ))
        })?;
        append_workflow_transition_event(
            store,
            mapping,
            &current_state,
            &WorkflowState::Implementing,
            reason,
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

fn resume_to_implementing_reason(from: &WorkflowState) -> Option<WorkflowTransitionReason> {
    match from {
        WorkflowState::Planning
        | WorkflowState::Testing
        | WorkflowState::PRDrafted
        | WorkflowState::AwaitingYourReview
        | WorkflowState::ReadyForReview
        | WorkflowState::InReview => Some(WorkflowTransitionReason::ImplementationResumed),
        WorkflowState::Implementing
        | WorkflowState::New
        | WorkflowState::Done
        | WorkflowState::Abandoned => None,
    }
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

async fn resolve_single_repository(
    vcs: &dyn VcsProvider,
    config: &SelectedTicketFlowConfig,
) -> Result<RepositoryRef, CoreError> {
    let repositories = vcs.discover_repositories(&config.repository_roots).await?;
    if repositories.is_empty() {
        return Err(CoreError::Configuration(format!(
            "No git repositories were discovered under configured roots: {}",
            config
                .repository_roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if repositories.len() > 1 {
        return Err(CoreError::Configuration(format!(
            "Multiple repositories were discovered; ticket-selected start/resume requires a single repository context (found: {}).",
            repositories
                .iter()
                .map(|repo| repo.root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    Ok(repositories[0].clone())
}

fn validate_flow_config(config: &SelectedTicketFlowConfig) -> Result<(), CoreError> {
    if config.repository_roots.is_empty() {
        return Err(CoreError::Configuration(
            "SelectedTicketFlowConfig.repository_roots cannot be empty.".to_owned(),
        ));
    }
    if config.worktrees_root.as_os_str().is_empty() {
        return Err(CoreError::Configuration(
            "SelectedTicketFlowConfig.worktrees_root cannot be empty.".to_owned(),
        ));
    }
    if config.project_id.as_str().trim().is_empty() {
        return Err(CoreError::Configuration(
            "SelectedTicketFlowConfig.project_id cannot be empty.".to_owned(),
        ));
    }

    Ok(())
}

fn parse_ticket_provider_identity(
    ticket_id: &TicketId,
) -> Result<(TicketProvider, String), CoreError> {
    let raw = ticket_id.as_str();
    let (provider, provider_ticket_id) = raw.split_once(':').ok_or_else(|| {
        CoreError::Configuration(format!(
            "Ticket id '{}' is not in '<provider>:<id>' format.",
            raw
        ))
    })?;

    let provider = match provider {
        "linear" => TicketProvider::Linear,
        other => {
            return Err(CoreError::Configuration(format!(
                "Unsupported ticket provider '{}' in ticket id '{}'.",
                other, raw
            )))
        }
    };

    let provider_ticket_id = provider_ticket_id.trim();
    if provider_ticket_id.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Ticket id '{}' has an empty provider-specific id.",
            raw
        )));
    }

    Ok((provider, provider_ticket_id.to_owned()))
}

fn normalize_issue_key(identifier: &str) -> Result<String, CoreError> {
    let token = identifier
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'));

    if token.is_empty() {
        return Err(CoreError::Configuration(
            "Selected ticket identifier is empty; cannot derive managed branch name.".to_owned(),
        ));
    }

    let normalized = token.to_ascii_uppercase();
    let Some((prefix, number)) = normalized.split_once('-') else {
        return Err(CoreError::Configuration(format!(
            "Ticket identifier '{}' must include an issue key and number (e.g. AP-126).",
            identifier
        )));
    };

    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(CoreError::Configuration(format!(
            "Ticket identifier '{}' has an invalid issue-key prefix '{}'.",
            identifier, prefix
        )));
    }
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CoreError::Configuration(format!(
            "Ticket identifier '{}' has an invalid issue number '{}'.",
            identifier, number
        )));
    }

    Ok(format!("{prefix}-{number}"))
}

fn title_slug(title: &str, max_len: usize) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            if slug.len() >= max_len {
                break;
            }
            slug.push(ch.to_ascii_lowercase());
            previous_was_dash = false;
            continue;
        }

        if !previous_was_dash && !slug.is_empty() {
            if slug.len() >= max_len {
                break;
            }
            slug.push('-');
            previous_was_dash = true;
        }
    }

    let trimmed = slug.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        DEFAULT_WORKTREE_SLUG.to_owned()
    } else {
        trimmed
    }
}

fn stable_component(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            previous_was_dash = false;
        } else if !previous_was_dash {
            normalized.push('-');
            previous_was_dash = true;
        }
    }

    let normalized = normalized.trim_matches('-').to_owned();
    if normalized.is_empty() {
        "ticket".to_owned()
    } else {
        normalized
    }
}

fn normalize_base_branch(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        DEFAULT_BASE_BRANCH.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn start_instruction(ticket: &TicketSummary) -> String {
    format!(
        "Begin implementation for {}: {}. Continue until the next meaningful checkpoint and report test results.",
        ticket.identifier, ticket.title
    )
}

fn resume_instruction(ticket: &TicketSummary) -> String {
    format!(
        "Resume implementation for {}: {}. Continue from the existing worktree/session context and drive to the next checkpoint.",
        ticket.identifier, ticket.title
    )
}

fn ticket_record_from_summary(
    summary: &TicketSummary,
    provider: TicketProvider,
    provider_ticket_id: String,
    fallback_updated_at: &str,
) -> TicketRecord {
    let updated_at = if summary.updated_at.trim().is_empty() {
        fallback_updated_at.to_owned()
    } else {
        summary.updated_at.clone()
    };

    TicketRecord {
        ticket_id: summary.ticket_id.clone(),
        provider,
        provider_ticket_id,
        identifier: summary.identifier.clone(),
        title: summary.title.clone(),
        state: summary.state.clone(),
        updated_at,
    }
}

fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", now.as_secs(), now.subsec_nanos())
}

fn next_event_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = EVENT_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("evt-{prefix}-{now}-{count}")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::{
        BackendCapabilities, BackendKind, RuntimeResult, TerminalSnapshot, WorktreeStatus,
    };

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
    }

    impl StubWorkerBackend {
        fn new(kind: BackendKind) -> Self {
            Self {
                kind,
                spawn_specs: Mutex::new(Vec::new()),
                spawn_results: Mutex::new(VecDeque::new()),
                send_inputs: Mutex::new(Vec::new()),
                send_input_results: Mutex::new(VecDeque::new()),
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

        async fn snapshot(&self, _session: &SessionHandle) -> RuntimeResult<TerminalSnapshot> {
            Ok(TerminalSnapshot {
                cols: 80,
                rows: 24,
                cursor_col: 0,
                cursor_row: 0,
                lines: Vec::new(),
            })
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
            &vcs,
            &backend,
        )
        .await
        .expect("start flow succeeds");

        assert_eq!(result.action, SelectedTicketFlowAction::Started);
        assert_eq!(vcs.created.lock().expect("lock").len(), 1);
        assert_eq!(backend.spawn_specs.lock().expect("lock").len(), 1);
        assert!(backend
            .spawn_specs
            .lock()
            .expect("lock")
            .first()
            .expect("spawn spec")
            .instruction_prelude
            .as_deref()
            .is_some_and(|prelude| prelude.contains("Begin implementation for AP-126")));

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
                    to: WorkflowState::Implementing,
                    ..
                })
            )
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
        assert!(events.iter().any(|event| {
            matches!(
                event.payload,
                OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    from: WorkflowState::Planning,
                    to: WorkflowState::Implementing,
                    reason: Some(WorkflowTransitionReason::ImplementationResumed),
                    ..
                })
            )
        }));
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
                .contains("Resume implementation for AP-126")
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
            &vcs,
            &backend,
        )
        .await
        .expect("resume with respawn succeeds");

        assert_eq!(result.action, SelectedTicketFlowAction::Resumed);
        assert_eq!(backend.send_inputs.lock().expect("lock").len(), 1);
        assert_eq!(backend.spawn_specs.lock().expect("lock").len(), 1);
        assert_eq!(
            backend.spawn_specs.lock().expect("lock")[0]
                .session_id
                .as_str(),
            mapping.session.session_id.as_str()
        );
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
    async fn resume_flow_from_testing_uses_implementation_resumed_reason() {
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

        start_or_resume_selected_ticket(&mut store, &selected_ticket(), &config(), &vcs, &backend)
            .await
            .expect("resume flow succeeds");

        let events = store
            .read_events_for_work_item(&mapping.work_item_id)
            .expect("work-item events");
        let transition_to_implementing = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if payload.to == WorkflowState::Implementing =>
                {
                    Some(payload.clone())
                }
                _ => None,
            })
            .expect("transition back to implementing exists");

        assert_eq!(transition_to_implementing.from, WorkflowState::Testing);
        assert_eq!(
            transition_to_implementing.reason,
            Some(WorkflowTransitionReason::ImplementationResumed)
        );
    }

    #[tokio::test]
    async fn resume_flow_from_awaiting_review_uses_implementation_resumed_reason() {
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

        start_or_resume_selected_ticket(&mut store, &selected_ticket(), &config(), &vcs, &backend)
            .await
            .expect("resume flow succeeds");

        let events = store
            .read_events_for_work_item(&mapping.work_item_id)
            .expect("work-item events");
        let transition_to_implementing = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                OrchestrationEventPayload::WorkflowTransition(payload)
                    if payload.to == WorkflowState::Implementing =>
                {
                    Some(payload.clone())
                }
                _ => None,
            })
            .expect("transition back to implementing exists");

        assert_eq!(
            transition_to_implementing.from,
            WorkflowState::AwaitingYourReview
        );
        assert_eq!(
            transition_to_implementing.reason,
            Some(WorkflowTransitionReason::ImplementationResumed)
        );
    }
}
