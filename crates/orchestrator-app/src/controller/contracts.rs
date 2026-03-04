use std::path::PathBuf;

use crate::commands::{SupervisorQueryContextArgs, UntypedCommandInvocation};
use async_trait::async_trait;
use orchestrator_domain::{
    CoreError, InboxItemId, InboxItemKind, InboxLane, InboxLaneColor, LlmResponseStream,
    SelectedTicketFlowResult, WorkItemId, WorkerSessionId, WorkflowState,
};
use orchestrator_ticketing::TicketSummary;
use tokio::sync::mpsc;

use crate::events::StoredEventEnvelope;
use crate::projection::ProjectionState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketCreateSubmitMode {
    CreateOnly,
    CreateAndStart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTicketFromPickerRequest {
    pub brief: String,
    pub selected_project: Option<String>,
    pub submit_mode: TicketCreateSubmitMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArchiveOutcome {
    pub warning: Option<String>,
    pub event: StoredEventEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMergeFinalizeOutcome {
    pub event: StoredEventEnvelope,
}

#[async_trait]
pub trait TicketPickerProvider: Send + Sync {
    async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError>;
    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
    }
    async fn start_or_resume_ticket(
        &self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) -> Result<SelectedTicketFlowResult, CoreError>;
    async fn create_ticket_from_brief(
        &self,
        request: CreateTicketFromPickerRequest,
    ) -> Result<TicketSummary, CoreError>;
    async fn archive_ticket(&self, _ticket: TicketSummary) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "ticket archiving is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn archive_session(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<SessionArchiveOutcome, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "session archiving is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn reload_projection(&self) -> Result<ProjectionState, CoreError>;
    async fn mark_session_crashed(
        &self,
        _session_id: WorkerSessionId,
        _reason: String,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn set_session_working_state(
        &self,
        _session_id: WorkerSessionId,
        _is_working: bool,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn session_worktree_diff(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<SessionWorktreeDiff, CoreError> {
        Err(CoreError::Configuration(
            "session worktree diff is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn advance_session_workflow(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<SessionWorkflowAdvanceOutcome, CoreError> {
        Err(CoreError::Configuration(
            "session workflow advance is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn complete_session_after_merge(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<SessionMergeFinalizeOutcome, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "session merge finalization is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn publish_inbox_item(
        &self,
        _request: InboxPublishRequest,
    ) -> Result<StoredEventEnvelope, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "inbox publishing is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn resolve_inbox_item(
        &self,
        _request: InboxResolveRequest,
    ) -> Result<Option<StoredEventEnvelope>, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "inbox resolution is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn set_inbox_lane_color(
        &self,
        _request: InboxLaneColorSetRequest,
    ) -> Result<StoredEventEnvelope, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "inbox lane color updates are not supported by this ticket provider".to_owned(),
        ))
    }
    async fn reset_inbox_lane_colors(
        &self,
        _request: InboxLaneColorsResetRequest,
    ) -> Result<StoredEventEnvelope, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "inbox lane color resets are not supported by this ticket provider".to_owned(),
        ))
    }
    async fn start_pr_pipeline_polling(
        &self,
        _sender: mpsc::Sender<MergeQueueEvent>,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn stop_pr_pipeline_polling(&self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn enqueue_pr_merge(&self, _session_id: WorkerSessionId) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "PR merge queueing is not supported by this ticket provider".to_owned(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxPublishRequest {
    pub work_item_id: WorkItemId,
    pub session_id: Option<WorkerSessionId>,
    pub kind: InboxItemKind,
    pub title: String,
    pub coalesce_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxResolveRequest {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: WorkItemId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxLaneColorSetRequest {
    pub lane: InboxLane,
    pub color: InboxLaneColor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxLaneColorsResetRequest {
    pub lane: Option<InboxLane>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorktreeDiff {
    pub session_id: WorkerSessionId,
    pub base_branch: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorkflowAdvanceOutcome {
    pub session_id: WorkerSessionId,
    pub work_item_id: WorkItemId,
    pub from: WorkflowState,
    pub to: WorkflowState,
    pub instruction: Option<String>,
    pub event: StoredEventEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeQueueCommandKind {
    Reconcile,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiCheckStatus {
    pub name: String,
    pub workflow: Option<String>,
    pub bucket: String,
    pub state: String,
    pub link: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MergeQueueEvent {
    Completed {
        session_id: WorkerSessionId,
        kind: MergeQueueCommandKind,
        completed: bool,
        merge_conflict: bool,
        base_branch: Option<String>,
        head_branch: Option<String>,
        ci_checks: Vec<CiCheckStatus>,
        ci_failures: Vec<String>,
        ci_has_failures: bool,
        ci_status_error: Option<String>,
        error: Option<String>,
    },
    SessionFinalized {
        session_id: WorkerSessionId,
        event: StoredEventEnvelope,
    },
    SessionFinalizeFailed {
        session_id: WorkerSessionId,
        message: String,
    },
}

pub type SupervisorCommandContext = SupervisorQueryContextArgs;

#[async_trait]
pub trait SupervisorCommandDispatcher: Send + Sync {
    async fn dispatch_supervisor_command(
        &self,
        invocation: UntypedCommandInvocation,
        context: SupervisorCommandContext,
    ) -> Result<(String, LlmResponseStream), CoreError>;

    async fn cancel_supervisor_command(&self, stream_id: &str) -> Result<(), CoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubTicketPickerProvider;

    #[async_trait]
    impl TicketPickerProvider for StubTicketPickerProvider {
        async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn start_or_resume_ticket(
            &self,
            _ticket: TicketSummary,
            _repository_override: Option<PathBuf>,
        ) -> Result<SelectedTicketFlowResult, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "start_or_resume_ticket not implemented for test stub".to_owned(),
            ))
        }

        async fn create_ticket_from_brief(
            &self,
            _request: CreateTicketFromPickerRequest,
        ) -> Result<TicketSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "create_ticket_from_brief not implemented for test stub".to_owned(),
            ))
        }

        async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
            Ok(ProjectionState::default())
        }
    }

    #[tokio::test]
    async fn ticket_picker_provider_default_methods_match_contract_fallbacks() {
        let provider = StubTicketPickerProvider;

        assert!(provider
            .list_projects()
            .await
            .expect("list projects")
            .is_empty());
        provider
            .mark_session_crashed(WorkerSessionId::new("sess-1"), "boom".to_owned())
            .await
            .expect("mark session crashed default should succeed");
        provider
            .set_session_working_state(WorkerSessionId::new("sess-1"), true)
            .await
            .expect("set session working default should succeed");
        provider
            .start_pr_pipeline_polling(mpsc::channel(1).0)
            .await
            .expect("polling start default should succeed");
        provider
            .stop_pr_pipeline_polling()
            .await
            .expect("polling stop default should succeed");

        assert!(matches!(
            provider.archive_session(WorkerSessionId::new("sess-1")).await,
            Err(CoreError::DependencyUnavailable(message))
            if message.contains("session archiving")
        ));
        assert!(matches!(
            provider
                .session_worktree_diff(WorkerSessionId::new("sess-1"))
                .await,
            Err(CoreError::Configuration(message))
            if message.contains("session worktree diff")
        ));
        assert!(matches!(
            provider
                .advance_session_workflow(WorkerSessionId::new("sess-1"))
                .await,
            Err(CoreError::Configuration(message))
            if message.contains("session workflow advance")
        ));
        assert!(matches!(
            provider
                .complete_session_after_merge(WorkerSessionId::new("sess-1"))
                .await,
            Err(CoreError::DependencyUnavailable(message))
            if message.contains("session merge finalization")
        ));
        assert!(matches!(
            provider
                .publish_inbox_item(InboxPublishRequest {
                    work_item_id: WorkItemId::new("wi-1"),
                    session_id: Some(WorkerSessionId::new("sess-1")),
                    kind: InboxItemKind::ReadyForReview,
                    title: "Title".to_owned(),
                    coalesce_key: "coalesce-key".to_owned(),
                })
                .await,
            Err(CoreError::DependencyUnavailable(message))
            if message.contains("inbox publishing")
        ));
        assert!(matches!(
            provider
                .resolve_inbox_item(InboxResolveRequest {
                    inbox_item_id: InboxItemId::new("inbox-1"),
                    work_item_id: WorkItemId::new("wi-1"),
                })
                .await,
            Err(CoreError::DependencyUnavailable(message))
            if message.contains("inbox resolution")
        ));
        assert!(matches!(
            provider.enqueue_pr_merge(WorkerSessionId::new("sess-1")).await,
            Err(CoreError::DependencyUnavailable(message))
            if message.contains("PR merge queueing")
        ));
    }
}
