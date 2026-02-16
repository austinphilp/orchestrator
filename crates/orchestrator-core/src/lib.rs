mod adapters;
mod commands;
mod error;
mod events;
mod identifiers;
mod projection;
mod status;
mod store;

pub use adapters::{
    AddTicketCommentRequest, BackendArtifactEvent, BackendBlockedEvent, BackendCapabilities,
    BackendCheckpointEvent, BackendCrashedEvent, BackendDoneEvent, BackendEvent, BackendKind,
    BackendNeedsInputEvent, BackendOutputEvent, BackendOutputStream, CodeHostKind,
    CodeHostProvider, CreatePullRequestRequest, CreateTicketRequest, CreateWorktreeRequest,
    DeleteWorktreeRequest, GithubClient, LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider,
    LlmProviderKind, LlmRateLimitState, LlmResponseStream, LlmResponseSubscription, LlmRole,
    LlmStreamChunk, LlmTokenUsage, PullRequestRef, PullRequestSummary, RepositoryRef,
    ReviewerRequest, SessionHandle, SpawnSpec, Supervisor, TerminalSnapshot, TicketAttachment,
    TicketQuery, TicketSummary, TicketingProvider, UpdateTicketStateRequest, UrlOpener,
    VcsProvider, WorkerBackend, WorkerEventStream, WorkerEventSubscription, WorktreeManager,
    WorktreeStatus, WorktreeSummary,
};
pub use commands::{
    ids as command_ids, Command, CommandArgSummary, CommandDefinition, CommandMetadata,
    CommandRegistry, SupervisorQueryArgs, UntypedCommandInvocation,
};
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
    ArtifactRecord, EventStore, RetrievalScope, RuntimeMappingRecord, SessionRecord,
    SqliteEventStore, StoredEventWithArtifacts, TicketRecord, TicketWorkItemMapping,
    WorktreeRecord,
};

#[cfg(test)]
mod tests;
