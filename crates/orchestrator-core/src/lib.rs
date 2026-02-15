use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(ProjectId);
string_id!(WorkItemId);
string_id!(WorktreeId);
string_id!(WorkerSessionId);
string_id!(InboxItemId);
string_id!(ArtifactId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketProvider {
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketId(String);

impl TicketId {
    pub fn from_provider_uuid(provider: TicketProvider, provider_uuid: impl AsRef<str>) -> Self {
        let provider_name = match provider {
            TicketProvider::Linear => "linear",
        };

        Self(format!("{provider_name}:{}", provider_uuid.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for TicketId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TicketId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowState {
    New,
    Planning,
    Implementing,
    Testing,
    PRDrafted,
    AwaitingYourReview,
    ReadyForReview,
    InReview,
    Done,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerSessionStatus {
    Running,
    WaitingForUser,
    Blocked,
    Done,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxItemKind {
    NeedsDecision,
    NeedsApproval,
    Blocked,
    FYI,
    ReadyForReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    Diff,
    PR,
    TestRun,
    LogSnippet,
    Link,
    Export,
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("persistence error: {0}")]
    Persistence(String),
}

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchestrationEventType {
    TicketSynced,
    WorkItemCreated,
    WorktreeCreated,
    SessionSpawned,
    SessionCheckpoint,
    SessionNeedsInput,
    SessionBlocked,
    ArtifactCreated,
    WorkflowTransition,
    InboxItemCreated,
    InboxItemResolved,
    UserResponded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSyncedPayload {
    pub ticket_id: TicketId,
    pub identifier: String,
    pub title: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemCreatedPayload {
    pub work_item_id: WorkItemId,
    pub ticket_id: TicketId,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreatedPayload {
    pub worktree_id: WorktreeId,
    pub work_item_id: WorkItemId,
    pub path: String,
    pub branch: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpawnedPayload {
    pub session_id: WorkerSessionId,
    pub work_item_id: WorkItemId,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpointPayload {
    pub session_id: WorkerSessionId,
    pub artifact_id: ArtifactId,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionNeedsInputPayload {
    pub session_id: WorkerSessionId,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBlockedPayload {
    pub session_id: WorkerSessionId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCreatedPayload {
    pub artifact_id: ArtifactId,
    pub work_item_id: WorkItemId,
    pub kind: ArtifactKind,
    pub label: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTransitionPayload {
    pub work_item_id: WorkItemId,
    pub from: WorkflowState,
    pub to: WorkflowState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItemCreatedPayload {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: WorkItemId,
    pub kind: InboxItemKind,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItemResolvedPayload {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: WorkItemId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRespondedPayload {
    pub session_id: Option<WorkerSessionId>,
    pub work_item_id: Option<WorkItemId>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum OrchestrationEventPayload {
    TicketSynced(TicketSyncedPayload),
    WorkItemCreated(WorkItemCreatedPayload),
    WorktreeCreated(WorktreeCreatedPayload),
    SessionSpawned(SessionSpawnedPayload),
    SessionCheckpoint(SessionCheckpointPayload),
    SessionNeedsInput(SessionNeedsInputPayload),
    SessionBlocked(SessionBlockedPayload),
    ArtifactCreated(ArtifactCreatedPayload),
    WorkflowTransition(WorkflowTransitionPayload),
    InboxItemCreated(InboxItemCreatedPayload),
    InboxItemResolved(InboxItemResolvedPayload),
    UserResponded(UserRespondedPayload),
}

impl OrchestrationEventPayload {
    fn event_type(&self) -> OrchestrationEventType {
        match self {
            Self::TicketSynced(_) => OrchestrationEventType::TicketSynced,
            Self::WorkItemCreated(_) => OrchestrationEventType::WorkItemCreated,
            Self::WorktreeCreated(_) => OrchestrationEventType::WorktreeCreated,
            Self::SessionSpawned(_) => OrchestrationEventType::SessionSpawned,
            Self::SessionCheckpoint(_) => OrchestrationEventType::SessionCheckpoint,
            Self::SessionNeedsInput(_) => OrchestrationEventType::SessionNeedsInput,
            Self::SessionBlocked(_) => OrchestrationEventType::SessionBlocked,
            Self::ArtifactCreated(_) => OrchestrationEventType::ArtifactCreated,
            Self::WorkflowTransition(_) => OrchestrationEventType::WorkflowTransition,
            Self::InboxItemCreated(_) => OrchestrationEventType::InboxItemCreated,
            Self::InboxItemResolved(_) => OrchestrationEventType::InboxItemResolved,
            Self::UserResponded(_) => OrchestrationEventType::UserResponded,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEventEnvelope {
    pub event_id: String,
    pub occurred_at: String,
    pub work_item_id: Option<WorkItemId>,
    pub session_id: Option<WorkerSessionId>,
    pub payload: OrchestrationEventPayload,
    pub schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEventEnvelope {
    pub event_id: String,
    pub sequence: u64,
    pub occurred_at: String,
    pub work_item_id: Option<WorkItemId>,
    pub session_id: Option<WorkerSessionId>,
    pub event_type: OrchestrationEventType,
    pub payload: OrchestrationEventPayload,
    pub schema_version: u32,
}

impl From<(u64, NewEventEnvelope)> for StoredEventEnvelope {
    fn from((sequence, event): (u64, NewEventEnvelope)) -> Self {
        Self {
            event_id: event.event_id,
            sequence,
            occurred_at: event.occurred_at,
            work_item_id: event.work_item_id,
            session_id: event.session_id,
            event_type: event.payload.event_type(),
            payload: event.payload,
            schema_version: event.schema_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalScope {
    Global,
    WorkItem(WorkItemId),
    Session(WorkerSessionId),
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

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let conn = Connection::open(path).map_err(|err| CoreError::Persistence(err.to_string()))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        let conn =
            Connection::open_in_memory().map_err(|err| CoreError::Persistence(err.to_string()))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), CoreError> {
        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS events (
                    event_id TEXT PRIMARY KEY,
                    sequence INTEGER NOT NULL UNIQUE,
                    occurred_at TEXT NOT NULL,
                    work_item_id TEXT NULL,
                    session_id TEXT NULL,
                    event_type TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    schema_version INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_events_sequence ON events(sequence);
                CREATE INDEX IF NOT EXISTS idx_events_work_item ON events(work_item_id, sequence DESC);
                CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, sequence DESC);
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))
    }

    fn map_row(row: &rusqlite::Row<'_>) -> Result<StoredEventEnvelope, CoreError> {
        let payload_json: String = row
            .get(6)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let payload: OrchestrationEventPayload = serde_json::from_str(&payload_json)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok(StoredEventEnvelope {
            event_id: row
                .get(0)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
            sequence: row
                .get(1)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
            occurred_at: row
                .get(2)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
            work_item_id: row
                .get::<_, Option<String>>(3)
                .map_err(|err| CoreError::Persistence(err.to_string()))?
                .map(WorkItemId::from),
            session_id: row
                .get::<_, Option<String>>(4)
                .map_err(|err| CoreError::Persistence(err.to_string()))?
                .map(WorkerSessionId::from),
            event_type: serde_json::from_str(
                &row.get::<_, String>(5)
                    .map_err(|err| CoreError::Persistence(err.to_string()))?,
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?,
            payload,
            schema_version: row
                .get(7)
                .map_err(|err| CoreError::Persistence(err.to_string()))?,
        })
    }
}

impl EventStore for SqliteEventStore {
    fn append(&mut self, event: NewEventEnvelope) -> Result<StoredEventEnvelope, CoreError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let max_sequence: Option<u64> = tx
            .query_row("SELECT MAX(sequence) FROM events", [], |row| row.get(0))
            .optional()
            .map_err(|err| CoreError::Persistence(err.to_string()))?
            .flatten();
        let sequence = max_sequence.unwrap_or(0) + 1;

        let payload_json = serde_json::to_string(&event.payload)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;
        let event_type = serde_json::to_string(&event.payload.event_type())
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let work_item_id = event.work_item_id.as_ref().map(|id| id.as_str().to_owned());
        let session_id = event.session_id.as_ref().map(|id| id.as_str().to_owned());

        tx.execute(
            "
            INSERT INTO events (
                event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                event.event_id,
                sequence,
                event.occurred_at,
                work_item_id,
                session_id,
                event_type,
                payload_json,
                event.schema_version,
            ],
        )
        .map_err(|err| CoreError::Persistence(err.to_string()))?;
        tx.commit()
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        Ok((sequence, event).into())
    }

    fn read_ordered(&self) -> Result<Vec<StoredEventEnvelope>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                ORDER BY sequence ASC
                ",
            )
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                Self::map_row(row).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(err.to_string())),
                    )
                })
            })
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|err| CoreError::Persistence(err.to_string()))?);
        }

        Ok(result)
    }

    fn query(
        &self,
        scope: RetrievalScope,
        limit: usize,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError> {
        if limit == 0 {
            return Ok(vec![]);
        }

        let (sql, bind): (&str, Option<String>) = match scope {
            RetrievalScope::Global => (
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                ORDER BY sequence DESC
                LIMIT ?1
                ",
                None,
            ),
            RetrievalScope::WorkItem(id) => (
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                WHERE work_item_id = ?1
                ORDER BY sequence DESC
                LIMIT ?2
                ",
                Some(id.as_str().to_owned()),
            ),
            RetrievalScope::Session(id) => (
                "
                SELECT event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
                FROM events
                WHERE session_id = ?1
                ORDER BY sequence DESC
                LIMIT ?2
                ",
                Some(id.as_str().to_owned()),
            ),
        };

        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|err| CoreError::Persistence(err.to_string()))?;

        let mut result = Vec::new();
        if let Some(value) = bind {
            let rows = stmt
                .query_map(params![value, limit], |row| {
                    Self::map_row(row).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other(err.to_string())),
                        )
                    })
                })
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            for row in rows {
                result.push(row.map_err(|err| CoreError::Persistence(err.to_string()))?);
            }
        } else {
            let rows = stmt
                .query_map(params![limit], |row| {
                    Self::map_row(row).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other(err.to_string())),
                        )
                    })
                })
                .map_err(|err| CoreError::Persistence(err.to_string()))?;
            for row in rows {
                result.push(row.map_err(|err| CoreError::Persistence(err.to_string()))?);
            }
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemProjection {
    pub id: WorkItemId,
    pub ticket_id: Option<TicketId>,
    pub project_id: Option<ProjectId>,
    pub workflow_state: Option<WorkflowState>,
    pub session_id: Option<WorkerSessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub inbox_items: Vec<InboxItemId>,
    pub artifacts: Vec<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProjection {
    pub id: WorkerSessionId,
    pub work_item_id: Option<WorkItemId>,
    pub status: Option<WorkerSessionStatus>,
    pub latest_checkpoint: Option<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItemProjection {
    pub id: InboxItemId,
    pub work_item_id: WorkItemId,
    pub kind: InboxItemKind,
    pub title: String,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactProjection {
    pub id: ArtifactId,
    pub work_item_id: WorkItemId,
    pub kind: ArtifactKind,
    pub label: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectionState {
    pub orchestrator_status: Option<String>,
    pub work_items: HashMap<WorkItemId, WorkItemProjection>,
    pub sessions: HashMap<WorkerSessionId, SessionProjection>,
    pub inbox_items: HashMap<InboxItemId, InboxItemProjection>,
    pub artifacts: HashMap<ArtifactId, ArtifactProjection>,
    pub events: Vec<StoredEventEnvelope>,
}

fn empty_work_item_projection(id: WorkItemId) -> WorkItemProjection {
    WorkItemProjection {
        id,
        ticket_id: None,
        project_id: None,
        workflow_state: None,
        session_id: None,
        worktree_id: None,
        inbox_items: vec![],
        artifacts: vec![],
    }
}

pub fn apply_event(state: &mut ProjectionState, event: StoredEventEnvelope) {
    match &event.payload {
        OrchestrationEventPayload::WorkItemCreated(payload) => {
            let entry = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            entry.ticket_id = Some(payload.ticket_id.clone());
            entry.project_id = Some(payload.project_id.clone());
        }
        OrchestrationEventPayload::WorktreeCreated(payload) => {
            let entry = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            entry.worktree_id = Some(payload.worktree_id.clone());
        }
        OrchestrationEventPayload::SessionSpawned(payload) => {
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.session_id = Some(payload.session_id.clone());

            state.sessions.insert(
                payload.session_id.clone(),
                SessionProjection {
                    id: payload.session_id.clone(),
                    work_item_id: Some(payload.work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
        }
        OrchestrationEventPayload::SessionCheckpoint(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.latest_checkpoint = Some(payload.artifact_id.clone());
                session.status = Some(WorkerSessionStatus::Running);
            }
        }
        OrchestrationEventPayload::SessionNeedsInput(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::WaitingForUser);
            }
        }
        OrchestrationEventPayload::SessionBlocked(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::Blocked);
            }
        }
        OrchestrationEventPayload::ArtifactCreated(payload) => {
            state.artifacts.insert(
                payload.artifact_id.clone(),
                ArtifactProjection {
                    id: payload.artifact_id.clone(),
                    work_item_id: payload.work_item_id.clone(),
                    kind: payload.kind.clone(),
                    label: payload.label.clone(),
                    uri: payload.uri.clone(),
                },
            );
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.artifacts.push(payload.artifact_id.clone());
        }
        OrchestrationEventPayload::WorkflowTransition(payload) => {
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.workflow_state = Some(payload.to.clone());
            state.orchestrator_status = Some(format!("{:?}", payload.to));
        }
        OrchestrationEventPayload::InboxItemCreated(payload) => {
            state.inbox_items.insert(
                payload.inbox_item_id.clone(),
                InboxItemProjection {
                    id: payload.inbox_item_id.clone(),
                    work_item_id: payload.work_item_id.clone(),
                    kind: payload.kind.clone(),
                    title: payload.title.clone(),
                    resolved: false,
                },
            );
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.inbox_items.push(payload.inbox_item_id.clone());
        }
        OrchestrationEventPayload::InboxItemResolved(payload) => {
            if let Some(item) = state.inbox_items.get_mut(&payload.inbox_item_id) {
                item.resolved = true;
            }
        }
        OrchestrationEventPayload::TicketSynced(_)
        | OrchestrationEventPayload::UserResponded(_) => {}
    }

    state.events.push(event);
}

pub fn rebuild_projection(events: &[StoredEventEnvelope]) -> ProjectionState {
    let mut state = ProjectionState::default();
    for event in events {
        apply_event(&mut state, event.clone());
    }
    state
}

pub fn retrieve_events(
    state: &ProjectionState,
    scope: RetrievalScope,
    limit: usize,
) -> Vec<StoredEventEnvelope> {
    if limit == 0 {
        return vec![];
    }

    let mut events: Vec<_> = state
        .events
        .iter()
        .filter(|event| match &scope {
            RetrievalScope::Global => true,
            RetrievalScope::WorkItem(id) => event.work_item_id.as_ref() == Some(id),
            RetrievalScope::Session(id) => event.session_id.as_ref() == Some(id),
        })
        .cloned()
        .collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.sequence));
    events.into_iter().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(event_id: &str, payload: OrchestrationEventPayload) -> NewEventEnvelope {
        NewEventEnvelope {
            event_id: event_id.to_owned(),
            occurred_at: "2026-02-15T14:00:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-1")),
            session_id: Some(WorkerSessionId::new("sess-1")),
            payload,
            schema_version: 1,
        }
    }

    #[test]
    fn event_envelope_serialization_round_trip() {
        let event = StoredEventEnvelope::from((
            1,
            sample_event(
                "evt-1",
                OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    artifact_id: ArtifactId::new("artifact-1"),
                    summary: "checkpoint text only".to_owned(),
                }),
            ),
        ));

        let json = serde_json::to_string(&event).expect("serialize event");
        let parsed: StoredEventEnvelope = serde_json::from_str(&json).expect("deserialize event");
        assert_eq!(parsed, event);
        assert_eq!(parsed.schema_version, 1);
    }

    #[test]
    fn append_assigns_monotonic_sequence_and_ordered_reads() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");

        let first = store
            .append(sample_event(
                "evt-1",
                OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                    work_item_id: WorkItemId::new("wi-1"),
                    ticket_id: TicketId::from("linear:123"),
                    project_id: ProjectId::new("proj-1"),
                }),
            ))
            .expect("append first");
        let second = store
            .append(sample_event(
                "evt-2",
                OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: WorkItemId::new("wi-1"),
                    from: WorkflowState::Planning,
                    to: WorkflowState::Implementing,
                }),
            ))
            .expect("append second");

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);

        let ordered = store.read_ordered().expect("read ordered");
        assert_eq!(ordered[0].sequence, 1);
        assert_eq!(ordered[1].sequence, 2);
    }

    #[test]
    fn deterministic_replay_produces_identical_projection_state() {
        let events = vec![
            StoredEventEnvelope::from((
                1,
                sample_event(
                    "evt-1",
                    OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                        work_item_id: WorkItemId::new("wi-1"),
                        ticket_id: TicketId::from("linear:123"),
                        project_id: ProjectId::new("proj-1"),
                    }),
                ),
            )),
            StoredEventEnvelope::from((
                2,
                sample_event(
                    "evt-2",
                    OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                        session_id: WorkerSessionId::new("sess-1"),
                        work_item_id: WorkItemId::new("wi-1"),
                        model: "gpt-5.2-codex".to_owned(),
                    }),
                ),
            )),
            StoredEventEnvelope::from((
                3,
                sample_event(
                    "evt-3",
                    OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                        work_item_id: WorkItemId::new("wi-1"),
                        from: WorkflowState::Planning,
                        to: WorkflowState::Implementing,
                    }),
                ),
            )),
        ];

        let first = rebuild_projection(&events);
        let second = rebuild_projection(&events);

        assert_eq!(first, second);
    }

    #[test]
    fn durable_restart_replay_matches_pre_restart_state() {
        let path =
            std::env::temp_dir().join(format!("orchestrator-events-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut writer = SqliteEventStore::open(&path).expect("open writer store");
        writer
            .append(sample_event(
                "evt-1",
                OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                    work_item_id: WorkItemId::new("wi-1"),
                    ticket_id: TicketId::from("linear:123"),
                    project_id: ProjectId::new("proj-1"),
                }),
            ))
            .expect("append work item");
        writer
            .append(sample_event(
                "evt-2",
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: InboxItemId::new("inbox-1"),
                    work_item_id: WorkItemId::new("wi-1"),
                    kind: InboxItemKind::NeedsDecision,
                    title: "Need choice".to_owned(),
                }),
            ))
            .expect("append inbox");

        let pre_restart = rebuild_projection(&writer.read_ordered().expect("read writer events"));

        let reader = SqliteEventStore::open(&path).expect("open reader store");
        let post_restart = rebuild_projection(&reader.read_ordered().expect("read reader events"));

        assert_eq!(pre_restart, post_restart);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn scoped_retrieval_filters_by_work_item_and_session() {
        let events = vec![
            StoredEventEnvelope {
                event_id: "evt-1".to_owned(),
                sequence: 1,
                occurred_at: "2026-02-15T14:00:00Z".to_owned(),
                work_item_id: Some(WorkItemId::new("wi-1")),
                session_id: Some(WorkerSessionId::new("sess-1")),
                event_type: OrchestrationEventType::SessionSpawned,
                payload: OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    work_item_id: WorkItemId::new("wi-1"),
                    model: "gpt".to_owned(),
                }),
                schema_version: 1,
            },
            StoredEventEnvelope {
                event_id: "evt-2".to_owned(),
                sequence: 2,
                occurred_at: "2026-02-15T14:01:00Z".to_owned(),
                work_item_id: Some(WorkItemId::new("wi-2")),
                session_id: Some(WorkerSessionId::new("sess-2")),
                event_type: OrchestrationEventType::SessionSpawned,
                payload: OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: WorkerSessionId::new("sess-2"),
                    work_item_id: WorkItemId::new("wi-2"),
                    model: "gpt".to_owned(),
                }),
                schema_version: 1,
            },
        ];
        let state = rebuild_projection(&events);

        let work_item_events = retrieve_events(
            &state,
            RetrievalScope::WorkItem(WorkItemId::new("wi-1")),
            10,
        );
        assert_eq!(work_item_events.len(), 1);
        assert_eq!(work_item_events[0].event_id, "evt-1");

        let session_events = retrieve_events(
            &state,
            RetrievalScope::Session(WorkerSessionId::new("sess-2")),
            10,
        );
        assert_eq!(session_events.len(), 1);
        assert_eq!(session_events[0].event_id, "evt-2");
    }

    #[test]
    fn retrieval_orders_newest_first_with_limit() {
        let events = (1..=5)
            .map(|seq| StoredEventEnvelope {
                event_id: format!("evt-{seq}"),
                sequence: seq,
                occurred_at: "2026-02-15T14:00:00Z".to_owned(),
                work_item_id: Some(WorkItemId::new("wi-1")),
                session_id: Some(WorkerSessionId::new("sess-1")),
                event_type: OrchestrationEventType::UserResponded,
                payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: Some(WorkerSessionId::new("sess-1")),
                    work_item_id: Some(WorkItemId::new("wi-1")),
                    message: "ok".to_owned(),
                }),
                schema_version: 1,
            })
            .collect::<Vec<_>>();
        let state = rebuild_projection(&events);

        let top3 = retrieve_events(&state, RetrievalScope::Global, 3);
        assert_eq!(top3.len(), 3);
        assert_eq!(top3[0].sequence, 5);
        assert_eq!(top3[2].sequence, 3);
    }

    #[test]
    fn artifact_payload_uses_reference_metadata_not_embedded_blob() {
        let payload = ArtifactCreatedPayload {
            artifact_id: ArtifactId::new("artifact-1"),
            work_item_id: WorkItemId::new("wi-1"),
            kind: ArtifactKind::LogSnippet,
            label: "worker logs".to_owned(),
            uri: "artifact://logs/worker-1".to_owned(),
        };
        let serialized = serde_json::to_string(&payload).expect("serialize artifact payload");

        assert!(serialized.contains("artifact://logs/worker-1"));
        assert!(!serialized.contains("BEGIN RAW LOG CONTENT"));
    }
}
