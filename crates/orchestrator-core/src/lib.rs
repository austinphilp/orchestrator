mod adapters;
mod attention_engine;
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
mod ticket_selection;
mod workflow;
mod workflow_automation;

pub use adapters::{
    AddTicketCommentRequest, ArchiveTicketRequest, CodeHostKind, CodeHostProvider,
    CreatePullRequestRequest, CreateTicketRequest, CreateWorktreeRequest, DeleteWorktreeRequest,
    GetTicketRequest, GithubClient, LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider,
    LlmProviderKind, LlmRateLimitState, LlmResponseStream, LlmResponseSubscription, LlmRole,
    LlmStreamChunk, LlmTokenUsage, LlmTool, LlmToolCall, LlmToolCallOutput, LlmToolChoice,
    LlmToolChoiceFunction, LlmToolFunction, LlmToolResult, PullRequestCiStatus,
    PullRequestMergeState, PullRequestRef, PullRequestSummary, RepositoryRef, ReviewerRequest,
    Supervisor, TicketAttachment, TicketDetails, TicketQuery, TicketSummary, TicketingProvider,
    UpdateTicketDescriptionRequest, UpdateTicketStateRequest, UrlOpener, VcsProvider,
    WorktreeManager, WorktreeStatus, WorktreeSummary,
};
pub use attention_engine::{
    attention_inbox_snapshot, AttentionBatchKind, AttentionBatchSurface, AttentionEngineConfig,
    AttentionInboxItem, AttentionInboxSnapshot, AttentionPriorityBand, AttentionScoreBreakdown,
};
pub use commands::{
    ids as command_ids, resolve_supervisor_query_scope, Command, CommandArgSummary,
    CommandDefinition, CommandMetadata, CommandRegistry, SupervisorQueryArgs,
    SupervisorQueryContextArgs, UntypedCommandInvocation,
};
pub use error::CoreError;
pub use events::{
    ArtifactCreatedPayload, InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope,
    OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
    SessionCheckpointPayload, SessionCompletedPayload, SessionCrashedPayload,
    SessionNeedsInputPayload, SessionSpawnedPayload, StoredEventEnvelope,
    SupervisorQueryCancellationSource, SupervisorQueryCancelledPayload,
    SupervisorQueryChunkPayload, SupervisorQueryFinishedPayload, SupervisorQueryKind,
    SupervisorQueryStartedPayload, TicketDetailsSyncedPayload, TicketSyncedPayload,
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
    BackendNeedsInputEvent, BackendOutputEvent, BackendOutputStream, BackendTurnStateEvent,
    RuntimeArtifactId, RuntimeError, RuntimeResult, RuntimeSessionId, SessionHandle,
    SessionLifecycle, SpawnSpec, WorkerBackend, WorkerEventStream, WorkerEventSubscription,
};
pub use projection::{
    apply_event, rebuild_projection, retrieve_events, ArtifactProjection, InboxItemProjection,
    ProjectionState, SessionProjection, SessionRuntimeProjection, WorkItemProjection,
};
pub use status::{ArtifactKind, InboxItemKind, WorkerSessionStatus, WorkflowState};
pub use store::{
    ArtifactRecord, EventStore, RetrievalScope, RuntimeMappingRecord, SessionRecord,
    SqliteEventStore, StoredEventWithArtifacts, TicketRecord, TicketWorkItemMapping,
    WorktreeRecord,
};
pub use ticket_selection::{
    start_or_resume_selected_ticket, SelectedTicketFlowAction, SelectedTicketFlowConfig,
    SelectedTicketFlowResult,
};
pub use workflow::{
    apply_workflow_transition, initial_workflow_state, validate_workflow_transition, WorkflowGuard,
    WorkflowGuardContext, WorkflowTransitionError, WorkflowTransitionReason,
};
pub use workflow_automation::{
    plan_workflow_automation, plan_workflow_automation_with_policy, HumanApprovalGateMode,
    WorkflowAutomationGatePolicy, WorkflowAutomationInboxIntent,
    WorkflowAutomationNotificationPolicy, WorkflowAutomationPlan, WorkflowAutomationPolicy,
    WorkflowAutomationStep, WorkflowAutomationTransitionIntent, WorkflowNotificationRoute,
    DRAFT_PULL_REQUEST_INBOX_TITLE, NEEDS_APPROVAL_INBOX_TITLE, READY_FOR_REVIEW_INBOX_TITLE,
};

#[cfg(test)]
mod tests;
