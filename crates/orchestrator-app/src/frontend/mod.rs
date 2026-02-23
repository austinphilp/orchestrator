use async_trait::async_trait;

use crate::projection::ProjectionState;
use orchestrator_domain::{
    BackendNeedsInputAnswer, BackendNeedsInputEvent, BackendOutputEvent, BackendTurnStateEvent,
    CoreError, WorkItemId, WorkerSessionId,
};

pub mod ui_boundary {
    pub use super::{
        FrontendApplicationMode, FrontendCommandIntent, FrontendController, FrontendEvent,
        FrontendEventStream, FrontendEventSubscription, FrontendIntent, FrontendNotification,
        FrontendNotificationLevel, FrontendSnapshot, FrontendTerminalEvent,
    };
    pub use crate::attention::{
        attention_inbox_snapshot, AttentionBatchKind, AttentionEngineConfig,
        AttentionInboxSnapshot, AttentionPriorityBand,
    };
    pub use crate::commands::{
        ids as command_ids, Command, CommandRegistry, SupervisorQueryArgs,
        SupervisorQueryContextArgs, UntypedCommandInvocation,
    };
    pub use crate::controller::action_runners::{
        run_reset_inbox_lane_colors_task, run_set_inbox_lane_color_task,
        run_publish_inbox_item_task, run_resolve_inbox_item_task, run_session_merge_finalize_task,
        run_session_workflow_advance_task, run_start_pr_pipeline_polling_task,
        run_stop_pr_pipeline_polling_task, run_supervisor_command_task, run_supervisor_stream_task,
        InboxPublishRunnerEvent as ControllerInboxPublishRunnerEvent,
        InboxLaneColorsResetRunnerEvent as ControllerInboxLaneColorsResetRunnerEvent,
        InboxLaneColorSetRunnerEvent as ControllerInboxLaneColorSetRunnerEvent,
        InboxResolveRunnerEvent as ControllerInboxResolveRunnerEvent,
        SupervisorStreamRunnerEvent as ControllerSupervisorStreamRunnerEvent,
        WorkflowAdvanceRunnerEvent as ControllerWorkflowAdvanceRunnerEvent,
    };
    pub use crate::controller::contracts::{
        CiCheckStatus, CreateTicketFromPickerRequest, InboxLaneColorsResetRequest,
        InboxLaneColorSetRequest, InboxPublishRequest, InboxResolveRequest,
        MergeQueueCommandKind, MergeQueueEvent, SessionArchiveOutcome, SessionMergeFinalizeOutcome,
        SessionWorkflowAdvanceOutcome, SessionWorktreeDiff, SupervisorCommandContext,
        SupervisorCommandDispatcher, TicketCreateSubmitMode, TicketPickerProvider,
    };
    pub use crate::events::{
        InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope,
        InboxLaneColorsResetPayload, InboxLaneColorSetPayload,
        OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
        SessionCheckpointPayload, SessionCompletedPayload, SessionNeedsInputPayload,
        StoredEventEnvelope, SupervisorQueryFinishedPayload, TicketDetailsSyncedPayload,
        TicketSyncedPayload, UserRespondedPayload, WorkflowTransitionPayload,
        WorktreeCreatedPayload,
    };
    pub use crate::projection::{
        apply_event, rebuild_projection, ArtifactProjection, InboxItemProjection, ProjectionState,
        SessionProjection, SessionRuntimeProjection, WorkItemProjection,
    };
    pub use orchestrator_domain::{
        ArtifactId, ArtifactKind, CoreError, InboxItemId, InboxItemKind, LlmChatRequest,
        InboxLane, InboxLaneColor, InboxLaneColorPreferences,
        LlmFinishReason, LlmMessage, LlmProvider, LlmProviderKind, LlmRateLimitState,
        LlmResponseStream, LlmResponseSubscription, LlmRole, LlmStreamChunk, LlmTokenUsage,
        ProjectId, SelectedTicketFlowResult, SessionLifecycle, TicketId, TicketProvider,
        WorkItemId, WorkerBackend, WorkerEventStream, WorkerEventSubscription, WorkerSessionId,
        WorkerSessionStatus, WorkflowState, WorkflowTransitionReason, WorktreeId,
    };
    pub use orchestrator_ticketing::{TicketId as TicketingTicketId, TicketSummary};
    pub use orchestrator_worker_protocol::{
        WorkerBackendCapabilities as BackendCapabilities, WorkerBackendKind as BackendKind,
        WorkerEvent as BackendEvent, WorkerNeedsInputAnswer as BackendNeedsInputAnswer,
        WorkerNeedsInputEvent as BackendNeedsInputEvent,
        WorkerNeedsInputOption as BackendNeedsInputOption,
        WorkerNeedsInputQuestion as BackendNeedsInputQuestion,
        WorkerOutputEvent as BackendOutputEvent, WorkerOutputStream as BackendOutputStream,
        WorkerRuntimeError as RuntimeError, WorkerRuntimeResult as RuntimeResult,
        WorkerSessionHandle as SessionHandle, WorkerSessionId as RuntimeSessionId,
        WorkerSpawnRequest as SpawnSpec, WorkerTurnStateEvent as BackendTurnStateEvent,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Hash)]
pub enum FrontendApplicationMode {
    Manual,
    Autopilot,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrontendSnapshot {
    pub status: String,
    pub projection: ProjectionState,
    pub application_mode: FrontendApplicationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Hash)]
pub enum FrontendNotificationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrontendNotification {
    pub level: FrontendNotificationLevel,
    pub message: String,
    pub work_item_id: Option<WorkItemId>,
    pub session_id: Option<WorkerSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FrontendEvent {
    SnapshotUpdated(FrontendSnapshot),
    Notification(FrontendNotification),
    TerminalSession(FrontendTerminalEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FrontendTerminalEvent {
    Output {
        session_id: WorkerSessionId,
        output: BackendOutputEvent,
    },
    TurnState {
        session_id: WorkerSessionId,
        turn_state: BackendTurnStateEvent,
    },
    NeedsInput {
        session_id: WorkerSessionId,
        needs_input: BackendNeedsInputEvent,
    },
    StreamFailed {
        session_id: WorkerSessionId,
        message: String,
        is_session_not_found: bool,
    },
    StreamEnded {
        session_id: WorkerSessionId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Hash)]
pub enum FrontendCommandIntent {
    EnterNormalMode,
    EnterInsertMode,
    ToggleGlobalSupervisorChat,
    OpenTerminalForSelected,
    OpenDiffInspectorForSelected,
    OpenTestInspectorForSelected,
    OpenPrInspectorForSelected,
    OpenChatInspectorForSelected,
    StartTerminalEscapeChord,
    QuitShell,
    FocusNextInbox,
    FocusPreviousInbox,
    CycleBatchNext,
    CycleBatchPrevious,
    JumpFirstInbox,
    JumpLastInbox,
    JumpBatchDecideOrUnblock,
    JumpBatchApprovals,
    JumpBatchReviewReady,
    JumpBatchFyiDigest,
    OpenTicketPicker,
    CloseTicketPicker,
    TicketPickerMoveNext,
    TicketPickerMovePrevious,
    TicketPickerFoldProject,
    TicketPickerUnfoldProject,
    TicketPickerStartSelected,
    ToggleWorktreeDiffModal,
    AdvanceTerminalWorkflowStage,
    ArchiveSelectedSession,
    OpenSessionOutputForSelectedInbox,
    SetApplicationModeAutopilot,
    SetApplicationModeManual,
}

impl FrontendCommandIntent {
    pub const ALL: [Self; 33] = [
        Self::EnterNormalMode,
        Self::EnterInsertMode,
        Self::ToggleGlobalSupervisorChat,
        Self::OpenTerminalForSelected,
        Self::OpenDiffInspectorForSelected,
        Self::OpenTestInspectorForSelected,
        Self::OpenPrInspectorForSelected,
        Self::OpenChatInspectorForSelected,
        Self::StartTerminalEscapeChord,
        Self::QuitShell,
        Self::FocusNextInbox,
        Self::FocusPreviousInbox,
        Self::CycleBatchNext,
        Self::CycleBatchPrevious,
        Self::JumpFirstInbox,
        Self::JumpLastInbox,
        Self::JumpBatchDecideOrUnblock,
        Self::JumpBatchApprovals,
        Self::JumpBatchReviewReady,
        Self::JumpBatchFyiDigest,
        Self::OpenTicketPicker,
        Self::CloseTicketPicker,
        Self::TicketPickerMoveNext,
        Self::TicketPickerMovePrevious,
        Self::TicketPickerFoldProject,
        Self::TicketPickerUnfoldProject,
        Self::TicketPickerStartSelected,
        Self::ToggleWorktreeDiffModal,
        Self::AdvanceTerminalWorkflowStage,
        Self::ArchiveSelectedSession,
        Self::OpenSessionOutputForSelectedInbox,
        Self::SetApplicationModeAutopilot,
        Self::SetApplicationModeManual,
    ];

    pub const fn command_id(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "ui.mode.normal",
            Self::EnterInsertMode => "ui.mode.insert",
            Self::ToggleGlobalSupervisorChat => "ui.supervisor_chat.toggle",
            Self::OpenTerminalForSelected => crate::commands::ids::UI_OPEN_TERMINAL_FOR_SELECTED,
            Self::OpenDiffInspectorForSelected => {
                crate::commands::ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED
            }
            Self::OpenTestInspectorForSelected => {
                crate::commands::ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED
            }
            Self::OpenPrInspectorForSelected => {
                crate::commands::ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED
            }
            Self::OpenChatInspectorForSelected => {
                crate::commands::ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED
            }
            Self::StartTerminalEscapeChord => "ui.mode.terminal_escape_prefix",
            Self::QuitShell => "ui.shell.quit",
            Self::FocusNextInbox => crate::commands::ids::UI_FOCUS_NEXT_INBOX,
            Self::FocusPreviousInbox => "ui.focus_previous_inbox",
            Self::CycleBatchNext => "ui.cycle_batch_next",
            Self::CycleBatchPrevious => "ui.cycle_batch_previous",
            Self::JumpFirstInbox => "ui.jump_first_inbox",
            Self::JumpLastInbox => "ui.jump_last_inbox",
            Self::JumpBatchDecideOrUnblock => "ui.jump_batch.decide_or_unblock",
            Self::JumpBatchApprovals => "ui.jump_batch.approvals",
            Self::JumpBatchReviewReady => "ui.jump_batch.review_ready",
            Self::JumpBatchFyiDigest => "ui.jump_batch.fyi_digest",
            Self::OpenTicketPicker => "ui.ticket_picker.open",
            Self::CloseTicketPicker => "ui.ticket_picker.close",
            Self::TicketPickerMoveNext => "ui.ticket_picker.move_next",
            Self::TicketPickerMovePrevious => "ui.ticket_picker.move_previous",
            Self::TicketPickerFoldProject => "ui.ticket_picker.fold_project",
            Self::TicketPickerUnfoldProject => "ui.ticket_picker.unfold_project",
            Self::TicketPickerStartSelected => "ui.ticket_picker.start_selected",
            Self::ToggleWorktreeDiffModal => "ui.worktree.diff.toggle",
            Self::AdvanceTerminalWorkflowStage => "ui.terminal.workflow.advance",
            Self::ArchiveSelectedSession => "ui.terminal.archive_selected_session",
            Self::OpenSessionOutputForSelectedInbox => "ui.open_session_output_for_selected_inbox",
            Self::SetApplicationModeAutopilot => "ui.app_mode.autopilot",
            Self::SetApplicationModeManual => "ui.app_mode.manual",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FrontendIntent {
    Command(FrontendCommandIntent),
    SendTerminalInput {
        session_id: WorkerSessionId,
        input: String,
    },
    RespondToNeedsInput {
        session_id: WorkerSessionId,
        prompt_id: String,
        answers: Vec<BackendNeedsInputAnswer>,
    },
}

#[async_trait]
pub trait FrontendEventSubscription: Send {
    async fn next_event(&mut self) -> Result<Option<FrontendEvent>, CoreError>;
}

pub type FrontendEventStream = Box<dyn FrontendEventSubscription>;

#[async_trait]
pub trait FrontendController: Send + Sync {
    async fn snapshot(&self) -> Result<FrontendSnapshot, CoreError>;
    async fn submit_intent(&self, intent: FrontendIntent) -> Result<(), CoreError>;
    async fn subscribe(&self) -> Result<FrontendEventStream, CoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn frontend_command_intent_ids_are_unique_and_complete() {
        let ids = FrontendCommandIntent::ALL
            .iter()
            .map(|intent| intent.command_id())
            .collect::<Vec<_>>();
        let unique = ids.iter().copied().collect::<HashSet<_>>();
        assert_eq!(ids.len(), 33);
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn frontend_snapshot_round_trips_json() {
        let snapshot = FrontendSnapshot {
            status: "ready".to_owned(),
            projection: ProjectionState::default(),
            application_mode: FrontendApplicationMode::Manual,
        };
        let encoded = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded: FrontendSnapshot =
            serde_json::from_str(encoded.as_str()).expect("deserialize snapshot");
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn frontend_event_notification_round_trips_json() {
        let event = FrontendEvent::Notification(FrontendNotification {
            level: FrontendNotificationLevel::Warning,
            message: "example warning".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-1")),
            session_id: Some(WorkerSessionId::new("sess-1")),
        });
        let encoded = serde_json::to_string(&event).expect("serialize event");
        let decoded: FrontendEvent =
            serde_json::from_str(encoded.as_str()).expect("deserialize event");
        assert_eq!(decoded, event);
    }

    #[test]
    fn frontend_event_terminal_session_round_trips_json() {
        let event = FrontendEvent::TerminalSession(FrontendTerminalEvent::StreamFailed {
            session_id: WorkerSessionId::new("sess-1"),
            message: "session not found".to_owned(),
            is_session_not_found: true,
        });
        let encoded = serde_json::to_string(&event).expect("serialize terminal event");
        let decoded: FrontendEvent =
            serde_json::from_str(encoded.as_str()).expect("deserialize terminal event");
        assert_eq!(decoded, event);
    }

    #[test]
    fn frontend_intent_round_trips_json() {
        let intent = FrontendIntent::RespondToNeedsInput {
            session_id: WorkerSessionId::new("sess-1"),
            prompt_id: "prompt-1".to_owned(),
            answers: vec![BackendNeedsInputAnswer {
                question_id: "q-1".to_owned(),
                answers: vec!["yes".to_owned()],
            }],
        };
        let encoded = serde_json::to_string(&intent).expect("serialize frontend intent");
        let decoded: FrontendIntent =
            serde_json::from_str(encoded.as_str()).expect("deserialize frontend intent");
        assert_eq!(decoded, intent);
    }
}
