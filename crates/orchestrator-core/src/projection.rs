use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::events::{OrchestrationEventPayload, StoredEventEnvelope};
use crate::identifiers::{
    ArtifactId, InboxItemId, ProjectId, TicketId, WorkItemId, WorkerSessionId, WorktreeId,
};
use crate::status::{ArtifactKind, InboxItemKind, WorkerSessionStatus, WorkflowState};
use crate::store::RetrievalScope;

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
        OrchestrationEventPayload::SessionCompleted(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::Done);
            }
        }
        OrchestrationEventPayload::SessionCrashed(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::Crashed);
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
            if !work_item.inbox_items.contains(&payload.inbox_item_id) {
                work_item.inbox_items.push(payload.inbox_item_id.clone());
            }
        }
        OrchestrationEventPayload::InboxItemResolved(payload) => {
            if let Some(item) = state.inbox_items.get_mut(&payload.inbox_item_id) {
                item.resolved = true;
            }
        }
        OrchestrationEventPayload::TicketSynced(_)
        | OrchestrationEventPayload::UserResponded(_)
        | OrchestrationEventPayload::SupervisorQueryStarted(_)
        | OrchestrationEventPayload::SupervisorQueryChunk(_)
        | OrchestrationEventPayload::SupervisorQueryCancelled(_)
        | OrchestrationEventPayload::SupervisorQueryFinished(_) => {}
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
