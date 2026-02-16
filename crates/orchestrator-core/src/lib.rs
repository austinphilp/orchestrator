mod adapters;
mod error;
mod events;
mod identifiers;
mod projection;
mod status;
mod store;

pub use adapters::{GithubClient, Supervisor};
pub use error::CoreError;
pub use events::{
    ArtifactCreatedPayload, InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope,
    OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
    SessionCheckpointPayload, SessionNeedsInputPayload, SessionSpawnedPayload, StoredEventEnvelope,
    TicketSyncedPayload, UserRespondedPayload, WorkItemCreatedPayload, WorkflowTransitionPayload,
    WorktreeCreatedPayload,
};
pub use identifiers::{
    ArtifactId, InboxItemId, ProjectId, TicketId, TicketProvider, WorkItemId, WorkerSessionId,
    WorktreeId,
};
pub use projection::{
    apply_event, rebuild_projection, retrieve_events, ArtifactProjection, InboxItemProjection,
    ProjectionState, SessionProjection, WorkItemProjection,
};
pub use status::{ArtifactKind, InboxItemKind, WorkerSessionStatus, WorkflowState};
pub use store::{
    ArtifactRecord, EventStore, RetrievalScope, SqliteEventStore, StoredEventWithArtifacts,
    TicketRecord, TicketWorkItemMapping,
};

#[cfg(test)]
mod tests;
