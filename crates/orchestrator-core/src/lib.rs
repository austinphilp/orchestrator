mod adapters;
mod commands;
mod error;
mod events;
mod identifiers;
mod normalization;
mod projection;
mod status;
mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use adapters::{
    AddTicketCommentRequest, CodeHostKind, CodeHostProvider, CreatePullRequestRequest,
    CreateTicketRequest, CreateWorktreeRequest, DeleteWorktreeRequest, GithubClient,
    LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider, LlmProviderKind, LlmRateLimitState,
    LlmResponseStream, LlmResponseSubscription, LlmRole, LlmStreamChunk, LlmTokenUsage,
    PullRequestRef, PullRequestSummary, RepositoryRef, ReviewerRequest, Supervisor,
    TicketAttachment, TicketQuery, TicketSummary, TicketingProvider, UpdateTicketStateRequest,
    UrlOpener, VcsProvider, WorktreeManager, WorktreeStatus, WorktreeSummary,
};
pub use commands::{
    ids as command_ids, Command, CommandArgSummary, CommandDefinition, CommandMetadata,
    CommandRegistry, SupervisorQueryArgs, UntypedCommandInvocation,
};
pub use error::CoreError;
pub use events::{
    ArtifactCreatedPayload, InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope,
    OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
    SessionCheckpointPayload, SessionCompletedPayload, SessionCrashedPayload,
    SessionNeedsInputPayload, SessionSpawnedPayload, StoredEventEnvelope, TicketSyncedPayload,
    UserRespondedPayload, WorkItemCreatedPayload, WorkflowTransitionPayload,
    WorktreeCreatedPayload,
};
pub use identifiers::{
    ArtifactId, InboxItemId, ProjectId, TicketId, TicketProvider, WorkItemId, WorkerSessionId,
    WorktreeId,
};
pub use normalization::{
    normalize_backend_event, BackendEventNormalizationContext, NormalizedBackendEvent,
    NormalizedDomainEvent, DOMAIN_EVENT_SCHEMA_VERSION,
};
pub use orchestrator_runtime::{
    BackendArtifactEvent, BackendArtifactKind, BackendBlockedEvent, BackendCapabilities,
    BackendCheckpointEvent, BackendCrashedEvent, BackendDoneEvent, BackendEvent, BackendKind,
    BackendNeedsInputEvent, BackendOutputEvent, BackendOutputStream, RuntimeArtifactId,
    RuntimeError, RuntimeResult, RuntimeSessionId, SessionHandle, SessionLifecycle, SpawnSpec,
    TerminalSnapshot, WorkerBackend, WorkerEventStream, WorkerEventSubscription,
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
