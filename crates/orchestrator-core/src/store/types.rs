use std::path::Path;

use orchestrator_runtime::BackendKind;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::error::CoreError;
use crate::events::{NewEventEnvelope, OrchestrationEventPayload, StoredEventEnvelope};
use crate::identifiers::{ArtifactId, TicketId, TicketProvider, WorkItemId, WorkerSessionId};
use crate::status::{ArtifactKind, WorkerSessionStatus};

const CURRENT_SCHEMA_VERSION: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalScope {
    Global,
    WorkItem(WorkItemId),
    Session(WorkerSessionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRecord {
    pub ticket_id: TicketId,
    pub provider: TicketProvider,
    pub provider_ticket_id: String,
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketWorkItemMapping {
    pub ticket_id: TicketId,
    pub work_item_id: WorkItemId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepositoryMappingRecord {
    pub provider: TicketProvider,
    pub project_id: crate::identifiers::ProjectId,
    pub repository_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub worktree_id: crate::identifiers::WorktreeId,
    pub work_item_id: WorkItemId,
    pub path: String,
    pub branch: String,
    pub base_branch: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: WorkerSessionId,
    pub work_item_id: WorkItemId,
    pub backend_kind: BackendKind,
    pub workdir: String,
    pub model: Option<String>,
    pub status: WorkerSessionStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMappingRecord {
    pub ticket: TicketRecord,
    pub work_item_id: WorkItemId,
    pub worktree: WorktreeRecord,
    pub session: SessionRecord,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRecord {
    pub artifact_id: ArtifactId,
    pub work_item_id: WorkItemId,
    pub kind: ArtifactKind,
    pub metadata: Value,
    pub storage_ref: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEventWithArtifacts {
    pub event: StoredEventEnvelope,
    pub artifact_ids: Vec<ArtifactId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventPrunePolicy {
    pub retention_days: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventPruneReport {
    pub cutoff_unix_seconds: u64,
    pub candidate_sessions: u64,
    pub eligible_sessions: u64,
    pub pruned_work_items: u64,
    pub deleted_events: u64,
    pub deleted_event_artifact_refs: u64,
    pub skipped_invalid_timestamps: u64,
}

pub trait EventStore {
    fn append(&mut self, event: NewEventEnvelope) -> Result<StoredEventEnvelope, CoreError>;
    fn read_ordered(&self) -> Result<Vec<StoredEventEnvelope>, CoreError>;
    fn query(
        &self,
        scope: RetrievalScope,
        limit: usize,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError>;
}

pub struct SqliteEventStore {
    conn: Connection,
}
