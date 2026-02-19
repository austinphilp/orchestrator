use serde::{Deserialize, Serialize};

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
    Merging,
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
