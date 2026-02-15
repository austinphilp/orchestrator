use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub description: Option<String>,
    pub work_item_ids: Vec<WorkItemId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: TicketId,
    pub provider: TicketProvider,
    pub provider_ticket_uuid: String,
    pub identifier: String,
    pub title: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub project_id: ProjectId,
    pub ticket_id: TicketId,
    pub workflow_state: WorkflowState,
    pub worktree_id: Option<WorktreeId>,
    pub worker_session_id: Option<WorkerSessionId>,
    pub inbox_item_ids: Vec<InboxItemId>,
    pub artifact_ids: Vec<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    pub id: WorktreeId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub path: String,
    pub branch: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSession {
    pub id: WorkerSessionId,
    pub work_item_id: WorkItemId,
    pub status: WorkerSessionStatus,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: InboxItemId,
    pub work_item_id: WorkItemId,
    pub kind: InboxItemKind,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub work_item_id: WorkItemId,
    pub kind: ArtifactKind,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
}

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HealthySupervisor;

    #[async_trait]
    impl Supervisor for HealthySupervisor {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    fn round_trip<T>(value: T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(&value).expect("serialize");
        let decoded: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, value);
    }

    #[tokio::test]
    async fn supervisor_trait_contract() {
        let supervisor = HealthySupervisor;
        let result = supervisor.health_check().await;
        assert!(result.is_ok());
    }

    #[test]
    fn core_error_message_is_actionable() {
        let error = CoreError::DependencyUnavailable("gh not found".to_owned());
        assert!(error.to_string().contains("gh not found"));
    }

    #[test]
    fn id_type_serialization_round_trip() {
        round_trip(ProjectId::new("proj_1"));
        round_trip(WorkItemId::new("work_item_1"));
        round_trip(WorktreeId::new("worktree_1"));
        round_trip(WorkerSessionId::new("session_1"));
        round_trip(InboxItemId::new("inbox_1"));
        round_trip(ArtifactId::new("artifact_1"));
        round_trip(TicketId::from_provider_uuid(
            TicketProvider::Linear,
            "154db568-a35f-4b72-939e-a5ae4d5ca4ae",
        ));
    }

    #[test]
    fn ticket_identity_stability_depends_on_provider_and_uuid() {
        let provider_uuid = "2fca9f3f-c769-40bc-ab5a-b1233d4be9ed";
        let original = Ticket {
            id: TicketId::from_provider_uuid(TicketProvider::Linear, provider_uuid),
            provider: TicketProvider::Linear,
            provider_ticket_uuid: provider_uuid.to_owned(),
            identifier: "AP-93".to_owned(),
            title: "Original title".to_owned(),
            state: "Todo".to_owned(),
        };
        let updated = Ticket {
            id: TicketId::from_provider_uuid(TicketProvider::Linear, provider_uuid),
            provider: TicketProvider::Linear,
            provider_ticket_uuid: provider_uuid.to_owned(),
            identifier: "AP-93".to_owned(),
            title: "Updated title".to_owned(),
            state: "In Progress".to_owned(),
        };

        assert_eq!(original.id, updated.id);
        assert_ne!(original.title, updated.title);
        assert_ne!(original.state, updated.state);
    }

    #[test]
    fn work_item_relationships_link_by_ids() {
        let work_item = WorkItem {
            id: WorkItemId::new("workitem_1"),
            project_id: ProjectId::new("project_1"),
            ticket_id: TicketId::from_provider_uuid(
                TicketProvider::Linear,
                "ad130f67-bad6-484f-bf95-073a0f838d44",
            ),
            workflow_state: WorkflowState::Implementing,
            worktree_id: Some(WorktreeId::new("worktree_1")),
            worker_session_id: Some(WorkerSessionId::new("session_1")),
            inbox_item_ids: vec![InboxItemId::new("inbox_1")],
            artifact_ids: vec![ArtifactId::new("artifact_1")],
        };

        assert_eq!(work_item.project_id.as_str(), "project_1");
        assert_eq!(
            work_item.worktree_id.expect("worktree").as_str(),
            "worktree_1"
        );
        assert_eq!(
            work_item.worker_session_id.expect("session").as_str(),
            "session_1"
        );
    }

    #[test]
    fn enum_compatibility_round_trip() {
        for variant in [
            WorkflowState::New,
            WorkflowState::Planning,
            WorkflowState::Implementing,
            WorkflowState::Testing,
            WorkflowState::PRDrafted,
            WorkflowState::AwaitingYourReview,
            WorkflowState::ReadyForReview,
            WorkflowState::InReview,
            WorkflowState::Done,
            WorkflowState::Abandoned,
        ] {
            round_trip(variant);
        }

        for variant in [
            WorkerSessionStatus::Running,
            WorkerSessionStatus::WaitingForUser,
            WorkerSessionStatus::Blocked,
            WorkerSessionStatus::Done,
            WorkerSessionStatus::Crashed,
        ] {
            round_trip(variant);
        }

        for variant in [
            InboxItemKind::NeedsDecision,
            InboxItemKind::NeedsApproval,
            InboxItemKind::Blocked,
            InboxItemKind::FYI,
            InboxItemKind::ReadyForReview,
        ] {
            round_trip(variant);
        }

        for variant in [
            ArtifactKind::Diff,
            ArtifactKind::PR,
            ArtifactKind::TestRun,
            ArtifactKind::LogSnippet,
            ArtifactKind::Link,
            ArtifactKind::Export,
        ] {
            round_trip(variant);
        }
    }

    #[test]
    fn canonical_entity_serialization_round_trip() {
        round_trip(Project {
            id: ProjectId::new("project_1"),
            name: "Orchestrator".to_owned(),
            description: Some("Main project".to_owned()),
            work_item_ids: vec![WorkItemId::new("workitem_1")],
        });

        round_trip(Ticket {
            id: TicketId::from_provider_uuid(
                TicketProvider::Linear,
                "5d9eef5a-58fb-4ddf-98fb-d86e607b1949",
            ),
            provider: TicketProvider::Linear,
            provider_ticket_uuid: "5d9eef5a-58fb-4ddf-98fb-d86e607b1949".to_owned(),
            identifier: "AP-93".to_owned(),
            title: "Define canonical domain entities".to_owned(),
            state: "In Progress".to_owned(),
        });

        round_trip(WorkItem {
            id: WorkItemId::new("workitem_1"),
            project_id: ProjectId::new("project_1"),
            ticket_id: TicketId::from_provider_uuid(
                TicketProvider::Linear,
                "4b6b95a8-f79c-497d-a802-008d5de918f9",
            ),
            workflow_state: WorkflowState::Testing,
            worktree_id: Some(WorktreeId::new("worktree_1")),
            worker_session_id: Some(WorkerSessionId::new("session_1")),
            inbox_item_ids: vec![InboxItemId::new("inbox_1")],
            artifact_ids: vec![ArtifactId::new("artifact_1")],
        });

        round_trip(Worktree {
            id: WorktreeId::new("worktree_1"),
            project_id: ProjectId::new("project_1"),
            work_item_id: WorkItemId::new("workitem_1"),
            path: "/tmp/worktree".to_owned(),
            branch: "feature/ap-93".to_owned(),
            base_branch: "main".to_owned(),
        });

        round_trip(WorkerSession {
            id: WorkerSessionId::new("session_1"),
            work_item_id: WorkItemId::new("workitem_1"),
            status: WorkerSessionStatus::Running,
            model: Some("gpt-5.2-codex".to_owned()),
            started_at: Some("2026-02-15T14:00:00Z".to_owned()),
            updated_at: Some("2026-02-15T14:05:00Z".to_owned()),
            ended_at: None,
        });

        round_trip(InboxItem {
            id: InboxItemId::new("inbox_1"),
            work_item_id: WorkItemId::new("workitem_1"),
            kind: InboxItemKind::NeedsDecision,
            title: "Choose implementation approach".to_owned(),
            body: "Select canonical id strategy".to_owned(),
        });

        round_trip(Artifact {
            id: ArtifactId::new("artifact_1"),
            work_item_id: WorkItemId::new("workitem_1"),
            kind: ArtifactKind::Diff,
            label: "domain diff".to_owned(),
            value: "https://example.com/diff".to_owned(),
        });
    }
}
