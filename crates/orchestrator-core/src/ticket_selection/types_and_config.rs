use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::normalization::DOMAIN_EVENT_SCHEMA_VERSION;
use crate::store::ProjectRepositoryMappingRecord;
use crate::{
    apply_workflow_transition, BackendKind, CoreError, CreateWorktreeRequest,
    DeleteWorktreeRequest, EventStore, NewEventEnvelope, OrchestrationEventPayload, ProjectId,
    RepositoryRef, RuntimeError, RuntimeMappingRecord, SessionHandle, SessionRecord,
    SessionSpawnedPayload, SpawnSpec, SqliteEventStore, TicketId, TicketProvider, TicketRecord,
    TicketSummary, VcsProvider, WorkItemCreatedPayload, WorkItemId, WorkerBackend, WorkerSessionId,
    WorkerSessionStatus, WorkflowGuardContext, WorkflowState, WorkflowTransitionPayload,
    WorkflowTransitionReason, WorktreeCreatedPayload, WorktreeId, WorktreeRecord,
};

const DEFAULT_BASE_BRANCH: &str = "main";
const DEFAULT_PROJECT_ID: &str = "project-default";
const DEFAULT_WORKTREE_SLUG: &str = "ticket";
const DEFAULT_WORKTREE_TITLE_SLUG_LIMIT: usize = 48;
const DEFAULT_WORKTREE_DIR: &str = ".orchestrator/worktrees";
const WORKTREE_BRANCH_PREFIX: &str = "ap/";
const ENV_HARNESS_SESSION_ID: &str = "ORCHESTRATOR_HARNESS_SESSION_ID";

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
    selected_ticket_description: Option<&str>,
    config: &SelectedTicketFlowConfig,
    repository_override: Option<PathBuf>,
    vcs: &dyn VcsProvider,
    worker_backend: &dyn WorkerBackend,
) -> Result<SelectedTicketFlowResult, CoreError> {
    let (provider, provider_ticket_id) =
        parse_ticket_provider_identity(&selected_ticket.ticket_id)?;
    validate_flow_config(config)?;
    cleanup_completed_session_worktrees(
        &store
            .list_runtime_mappings()?
            .into_iter()
            .collect::<Vec<_>>(),
        &config.worktrees_root,
    );
    let project_id = selected_ticket_project_id(selected_ticket, &config.project_id);

    if let Some(existing_mapping) =
        store.find_runtime_mapping_by_ticket(&provider, provider_ticket_id.as_str())?
    {
        if existing_mapping.session.status != WorkerSessionStatus::Done
            && !mapping_worktree_exists(&existing_mapping)
        {
            return start_new_mapping(
                store,
                selected_ticket,
                selected_ticket_description,
                config,
                project_id,
                repository_override,
                vcs,
                worker_backend,
            )
            .await;
        }

        return resume_existing_mapping(
            store,
            selected_ticket,
            selected_ticket_description,
            project_id,
            config,
            worker_backend,
            existing_mapping,
        )
        .await;
    }

    start_new_mapping(
        store,
        selected_ticket,
        selected_ticket_description,
        config,
        project_id,
        repository_override,
        vcs,
        worker_backend,
    )
    .await
}
