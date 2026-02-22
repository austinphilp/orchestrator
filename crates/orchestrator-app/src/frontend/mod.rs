use async_trait::async_trait;

use crate::projection::ProjectionState;
use orchestrator_core::{
    BackendNeedsInputAnswer, BackendNeedsInputEvent, BackendOutputEvent, BackendTurnStateEvent,
    CoreError, WorkItemId, WorkerSessionId,
};

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

impl From<FrontendApplicationMode> for orchestrator_core::FrontendApplicationMode {
    fn from(value: FrontendApplicationMode) -> Self {
        match value {
            FrontendApplicationMode::Manual => Self::Manual,
            FrontendApplicationMode::Autopilot => Self::Autopilot,
        }
    }
}

impl From<orchestrator_core::FrontendApplicationMode> for FrontendApplicationMode {
    fn from(value: orchestrator_core::FrontendApplicationMode) -> Self {
        match value {
            orchestrator_core::FrontendApplicationMode::Manual => Self::Manual,
            orchestrator_core::FrontendApplicationMode::Autopilot => Self::Autopilot,
        }
    }
}

impl From<FrontendSnapshot> for orchestrator_core::FrontendSnapshot {
    fn from(value: FrontendSnapshot) -> Self {
        Self {
            status: value.status,
            projection: value.projection,
            application_mode: value.application_mode.into(),
        }
    }
}

impl From<orchestrator_core::FrontendSnapshot> for FrontendSnapshot {
    fn from(value: orchestrator_core::FrontendSnapshot) -> Self {
        Self {
            status: value.status,
            projection: value.projection,
            application_mode: value.application_mode.into(),
        }
    }
}

impl From<FrontendNotificationLevel> for orchestrator_core::FrontendNotificationLevel {
    fn from(value: FrontendNotificationLevel) -> Self {
        match value {
            FrontendNotificationLevel::Info => Self::Info,
            FrontendNotificationLevel::Warning => Self::Warning,
            FrontendNotificationLevel::Error => Self::Error,
        }
    }
}

impl From<orchestrator_core::FrontendNotificationLevel> for FrontendNotificationLevel {
    fn from(value: orchestrator_core::FrontendNotificationLevel) -> Self {
        match value {
            orchestrator_core::FrontendNotificationLevel::Info => Self::Info,
            orchestrator_core::FrontendNotificationLevel::Warning => Self::Warning,
            orchestrator_core::FrontendNotificationLevel::Error => Self::Error,
        }
    }
}

impl From<FrontendNotification> for orchestrator_core::FrontendNotification {
    fn from(value: FrontendNotification) -> Self {
        Self {
            level: value.level.into(),
            message: value.message,
            work_item_id: value.work_item_id,
            session_id: value.session_id,
        }
    }
}

impl From<orchestrator_core::FrontendNotification> for FrontendNotification {
    fn from(value: orchestrator_core::FrontendNotification) -> Self {
        Self {
            level: value.level.into(),
            message: value.message,
            work_item_id: value.work_item_id,
            session_id: value.session_id,
        }
    }
}

impl From<FrontendTerminalEvent> for orchestrator_core::FrontendTerminalEvent {
    fn from(value: FrontendTerminalEvent) -> Self {
        match value {
            FrontendTerminalEvent::Output { session_id, output } => {
                Self::Output { session_id, output }
            }
            FrontendTerminalEvent::TurnState {
                session_id,
                turn_state,
            } => Self::TurnState {
                session_id,
                turn_state,
            },
            FrontendTerminalEvent::NeedsInput {
                session_id,
                needs_input,
            } => Self::NeedsInput {
                session_id,
                needs_input,
            },
            FrontendTerminalEvent::StreamFailed {
                session_id,
                message,
                is_session_not_found,
            } => Self::StreamFailed {
                session_id,
                message,
                is_session_not_found,
            },
            FrontendTerminalEvent::StreamEnded { session_id } => Self::StreamEnded { session_id },
        }
    }
}

impl From<orchestrator_core::FrontendTerminalEvent> for FrontendTerminalEvent {
    fn from(value: orchestrator_core::FrontendTerminalEvent) -> Self {
        match value {
            orchestrator_core::FrontendTerminalEvent::Output { session_id, output } => {
                Self::Output { session_id, output }
            }
            orchestrator_core::FrontendTerminalEvent::TurnState {
                session_id,
                turn_state,
            } => Self::TurnState {
                session_id,
                turn_state,
            },
            orchestrator_core::FrontendTerminalEvent::NeedsInput {
                session_id,
                needs_input,
            } => Self::NeedsInput {
                session_id,
                needs_input,
            },
            orchestrator_core::FrontendTerminalEvent::StreamFailed {
                session_id,
                message,
                is_session_not_found,
            } => Self::StreamFailed {
                session_id,
                message,
                is_session_not_found,
            },
            orchestrator_core::FrontendTerminalEvent::StreamEnded { session_id } => {
                Self::StreamEnded { session_id }
            }
        }
    }
}

impl From<FrontendEvent> for orchestrator_core::FrontendEvent {
    fn from(value: FrontendEvent) -> Self {
        match value {
            FrontendEvent::SnapshotUpdated(snapshot) => Self::SnapshotUpdated(snapshot.into()),
            FrontendEvent::Notification(notification) => Self::Notification(notification.into()),
            FrontendEvent::TerminalSession(event) => Self::TerminalSession(event.into()),
        }
    }
}

impl From<orchestrator_core::FrontendEvent> for FrontendEvent {
    fn from(value: orchestrator_core::FrontendEvent) -> Self {
        match value {
            orchestrator_core::FrontendEvent::SnapshotUpdated(snapshot) => {
                Self::SnapshotUpdated(snapshot.into())
            }
            orchestrator_core::FrontendEvent::Notification(notification) => {
                Self::Notification(notification.into())
            }
            orchestrator_core::FrontendEvent::TerminalSession(event) => {
                Self::TerminalSession(event.into())
            }
        }
    }
}

impl From<FrontendCommandIntent> for orchestrator_core::FrontendCommandIntent {
    fn from(value: FrontendCommandIntent) -> Self {
        match value {
            FrontendCommandIntent::EnterNormalMode => Self::EnterNormalMode,
            FrontendCommandIntent::EnterInsertMode => Self::EnterInsertMode,
            FrontendCommandIntent::ToggleGlobalSupervisorChat => Self::ToggleGlobalSupervisorChat,
            FrontendCommandIntent::OpenTerminalForSelected => Self::OpenTerminalForSelected,
            FrontendCommandIntent::OpenDiffInspectorForSelected => {
                Self::OpenDiffInspectorForSelected
            }
            FrontendCommandIntent::OpenTestInspectorForSelected => {
                Self::OpenTestInspectorForSelected
            }
            FrontendCommandIntent::OpenPrInspectorForSelected => Self::OpenPrInspectorForSelected,
            FrontendCommandIntent::OpenChatInspectorForSelected => {
                Self::OpenChatInspectorForSelected
            }
            FrontendCommandIntent::StartTerminalEscapeChord => Self::StartTerminalEscapeChord,
            FrontendCommandIntent::QuitShell => Self::QuitShell,
            FrontendCommandIntent::FocusNextInbox => Self::FocusNextInbox,
            FrontendCommandIntent::FocusPreviousInbox => Self::FocusPreviousInbox,
            FrontendCommandIntent::CycleBatchNext => Self::CycleBatchNext,
            FrontendCommandIntent::CycleBatchPrevious => Self::CycleBatchPrevious,
            FrontendCommandIntent::JumpFirstInbox => Self::JumpFirstInbox,
            FrontendCommandIntent::JumpLastInbox => Self::JumpLastInbox,
            FrontendCommandIntent::JumpBatchDecideOrUnblock => Self::JumpBatchDecideOrUnblock,
            FrontendCommandIntent::JumpBatchApprovals => Self::JumpBatchApprovals,
            FrontendCommandIntent::JumpBatchReviewReady => Self::JumpBatchReviewReady,
            FrontendCommandIntent::JumpBatchFyiDigest => Self::JumpBatchFyiDigest,
            FrontendCommandIntent::OpenTicketPicker => Self::OpenTicketPicker,
            FrontendCommandIntent::CloseTicketPicker => Self::CloseTicketPicker,
            FrontendCommandIntent::TicketPickerMoveNext => Self::TicketPickerMoveNext,
            FrontendCommandIntent::TicketPickerMovePrevious => Self::TicketPickerMovePrevious,
            FrontendCommandIntent::TicketPickerFoldProject => Self::TicketPickerFoldProject,
            FrontendCommandIntent::TicketPickerUnfoldProject => Self::TicketPickerUnfoldProject,
            FrontendCommandIntent::TicketPickerStartSelected => Self::TicketPickerStartSelected,
            FrontendCommandIntent::ToggleWorktreeDiffModal => Self::ToggleWorktreeDiffModal,
            FrontendCommandIntent::AdvanceTerminalWorkflowStage => {
                Self::AdvanceTerminalWorkflowStage
            }
            FrontendCommandIntent::ArchiveSelectedSession => Self::ArchiveSelectedSession,
            FrontendCommandIntent::OpenSessionOutputForSelectedInbox => {
                Self::OpenSessionOutputForSelectedInbox
            }
            FrontendCommandIntent::SetApplicationModeAutopilot => Self::SetApplicationModeAutopilot,
            FrontendCommandIntent::SetApplicationModeManual => Self::SetApplicationModeManual,
        }
    }
}

impl From<orchestrator_core::FrontendCommandIntent> for FrontendCommandIntent {
    fn from(value: orchestrator_core::FrontendCommandIntent) -> Self {
        match value {
            orchestrator_core::FrontendCommandIntent::EnterNormalMode => Self::EnterNormalMode,
            orchestrator_core::FrontendCommandIntent::EnterInsertMode => Self::EnterInsertMode,
            orchestrator_core::FrontendCommandIntent::ToggleGlobalSupervisorChat => {
                Self::ToggleGlobalSupervisorChat
            }
            orchestrator_core::FrontendCommandIntent::OpenTerminalForSelected => {
                Self::OpenTerminalForSelected
            }
            orchestrator_core::FrontendCommandIntent::OpenDiffInspectorForSelected => {
                Self::OpenDiffInspectorForSelected
            }
            orchestrator_core::FrontendCommandIntent::OpenTestInspectorForSelected => {
                Self::OpenTestInspectorForSelected
            }
            orchestrator_core::FrontendCommandIntent::OpenPrInspectorForSelected => {
                Self::OpenPrInspectorForSelected
            }
            orchestrator_core::FrontendCommandIntent::OpenChatInspectorForSelected => {
                Self::OpenChatInspectorForSelected
            }
            orchestrator_core::FrontendCommandIntent::StartTerminalEscapeChord => {
                Self::StartTerminalEscapeChord
            }
            orchestrator_core::FrontendCommandIntent::QuitShell => Self::QuitShell,
            orchestrator_core::FrontendCommandIntent::FocusNextInbox => Self::FocusNextInbox,
            orchestrator_core::FrontendCommandIntent::FocusPreviousInbox => {
                Self::FocusPreviousInbox
            }
            orchestrator_core::FrontendCommandIntent::CycleBatchNext => Self::CycleBatchNext,
            orchestrator_core::FrontendCommandIntent::CycleBatchPrevious => {
                Self::CycleBatchPrevious
            }
            orchestrator_core::FrontendCommandIntent::JumpFirstInbox => Self::JumpFirstInbox,
            orchestrator_core::FrontendCommandIntent::JumpLastInbox => Self::JumpLastInbox,
            orchestrator_core::FrontendCommandIntent::JumpBatchDecideOrUnblock => {
                Self::JumpBatchDecideOrUnblock
            }
            orchestrator_core::FrontendCommandIntent::JumpBatchApprovals => {
                Self::JumpBatchApprovals
            }
            orchestrator_core::FrontendCommandIntent::JumpBatchReviewReady => {
                Self::JumpBatchReviewReady
            }
            orchestrator_core::FrontendCommandIntent::JumpBatchFyiDigest => {
                Self::JumpBatchFyiDigest
            }
            orchestrator_core::FrontendCommandIntent::OpenTicketPicker => Self::OpenTicketPicker,
            orchestrator_core::FrontendCommandIntent::CloseTicketPicker => Self::CloseTicketPicker,
            orchestrator_core::FrontendCommandIntent::TicketPickerMoveNext => {
                Self::TicketPickerMoveNext
            }
            orchestrator_core::FrontendCommandIntent::TicketPickerMovePrevious => {
                Self::TicketPickerMovePrevious
            }
            orchestrator_core::FrontendCommandIntent::TicketPickerFoldProject => {
                Self::TicketPickerFoldProject
            }
            orchestrator_core::FrontendCommandIntent::TicketPickerUnfoldProject => {
                Self::TicketPickerUnfoldProject
            }
            orchestrator_core::FrontendCommandIntent::TicketPickerStartSelected => {
                Self::TicketPickerStartSelected
            }
            orchestrator_core::FrontendCommandIntent::ToggleWorktreeDiffModal => {
                Self::ToggleWorktreeDiffModal
            }
            orchestrator_core::FrontendCommandIntent::AdvanceTerminalWorkflowStage => {
                Self::AdvanceTerminalWorkflowStage
            }
            orchestrator_core::FrontendCommandIntent::ArchiveSelectedSession => {
                Self::ArchiveSelectedSession
            }
            orchestrator_core::FrontendCommandIntent::OpenSessionOutputForSelectedInbox => {
                Self::OpenSessionOutputForSelectedInbox
            }
            orchestrator_core::FrontendCommandIntent::SetApplicationModeAutopilot => {
                Self::SetApplicationModeAutopilot
            }
            orchestrator_core::FrontendCommandIntent::SetApplicationModeManual => {
                Self::SetApplicationModeManual
            }
        }
    }
}

impl From<FrontendIntent> for orchestrator_core::FrontendIntent {
    fn from(value: FrontendIntent) -> Self {
        match value {
            FrontendIntent::Command(command) => Self::Command(command.into()),
            FrontendIntent::SendTerminalInput { session_id, input } => {
                Self::SendTerminalInput { session_id, input }
            }
            FrontendIntent::RespondToNeedsInput {
                session_id,
                prompt_id,
                answers,
            } => Self::RespondToNeedsInput {
                session_id,
                prompt_id,
                answers,
            },
        }
    }
}

impl From<orchestrator_core::FrontendIntent> for FrontendIntent {
    fn from(value: orchestrator_core::FrontendIntent) -> Self {
        match value {
            orchestrator_core::FrontendIntent::Command(command) => Self::Command(command.into()),
            orchestrator_core::FrontendIntent::SendTerminalInput { session_id, input } => {
                Self::SendTerminalInput { session_id, input }
            }
            orchestrator_core::FrontendIntent::RespondToNeedsInput {
                session_id,
                prompt_id,
                answers,
            } => Self::RespondToNeedsInput {
                session_id,
                prompt_id,
                answers,
            },
        }
    }
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

    #[test]
    fn frontend_command_ids_match_legacy_core_contract() {
        let app_ids = FrontendCommandIntent::ALL
            .iter()
            .map(|intent| intent.command_id())
            .collect::<Vec<_>>();
        let core_ids = orchestrator_core::FrontendCommandIntent::ALL
            .iter()
            .map(|intent| intent.command_id())
            .collect::<Vec<_>>();

        assert_eq!(app_ids, core_ids);
    }

    #[test]
    fn frontend_command_intent_round_trips_through_core_adapter() {
        for intent in FrontendCommandIntent::ALL {
            let core: orchestrator_core::FrontendCommandIntent = intent.into();
            let roundtrip: FrontendCommandIntent = core.into();
            assert_eq!(roundtrip, intent);
        }
    }

    #[test]
    fn frontend_snapshot_round_trips_through_core_adapter() {
        let snapshot = FrontendSnapshot {
            status: "ready".to_owned(),
            projection: ProjectionState::default(),
            application_mode: FrontendApplicationMode::Autopilot,
        };

        let core: orchestrator_core::FrontendSnapshot = snapshot.clone().into();
        let roundtrip: FrontendSnapshot = core.into();
        assert_eq!(roundtrip, snapshot);
    }

    #[test]
    fn frontend_event_round_trips_through_core_adapter() {
        let event = FrontendEvent::TerminalSession(FrontendTerminalEvent::StreamEnded {
            session_id: WorkerSessionId::new("sess-2"),
        });

        let core: orchestrator_core::FrontendEvent = event.clone().into();
        let roundtrip: FrontendEvent = core.into();
        assert_eq!(roundtrip, event);
    }

    #[test]
    fn frontend_intent_round_trips_through_core_adapter() {
        let intent = FrontendIntent::Command(FrontendCommandIntent::SetApplicationModeManual);

        let core: orchestrator_core::FrontendIntent = intent.clone().into();
        let roundtrip: FrontendIntent = core.into();
        assert_eq!(roundtrip, intent);
    }

    #[test]
    fn frontend_event_json_schema_matches_legacy_core_contract() {
        let event = FrontendEvent::Notification(FrontendNotification {
            level: FrontendNotificationLevel::Info,
            message: "sync".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-5")),
            session_id: Some(WorkerSessionId::new("sess-5")),
        });

        let app_json = serde_json::to_value(&event).expect("serialize app event");
        let core_json = serde_json::to_value(orchestrator_core::FrontendEvent::from(event))
            .expect("serialize core event");
        assert_eq!(app_json, core_json);
    }
}
