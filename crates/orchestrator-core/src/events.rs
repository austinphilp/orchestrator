use serde::{Deserialize, Serialize};

use crate::identifiers::{
    ArtifactId, InboxItemId, ProjectId, TicketId, WorkItemId, WorkerSessionId, WorktreeId,
};
use crate::status::{ArtifactKind, InboxItemKind, WorkflowState};

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
    pub(crate) fn event_type(&self) -> OrchestrationEventType {
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
