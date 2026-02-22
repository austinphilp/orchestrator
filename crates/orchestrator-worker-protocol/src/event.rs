use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerOutputEvent {
    pub stream: WorkerOutputStream,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCheckpointEvent {
    pub summary: String,
    pub detail: Option<String>,
    pub file_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerNeedsInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerNeedsInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub is_other: bool,
    pub is_secret: bool,
    pub options: Option<Vec<WorkerNeedsInputOption>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerNeedsInputEvent {
    pub prompt_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub default_option: Option<String>,
    #[serde(default)]
    pub questions: Vec<WorkerNeedsInputQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerNeedsInputAnswer {
    pub question_id: String,
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBlockedEvent {
    pub reason: String,
    pub hint: Option<String>,
    pub log_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerArtifactKind {
    Diff,
    PullRequest,
    TestRun,
    LogSnippet,
    Link,
    SessionExport,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerArtifactEvent {
    pub kind: WorkerArtifactKind,
    pub artifact_id: Option<crate::ids::WorkerArtifactId>,
    pub label: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerTurnStateEvent {
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkerDoneEvent {
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCrashedEvent {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerEvent {
    Output(WorkerOutputEvent),
    Checkpoint(WorkerCheckpointEvent),
    NeedsInput(WorkerNeedsInputEvent),
    Blocked(WorkerBlockedEvent),
    Artifact(WorkerArtifactEvent),
    TurnState(WorkerTurnStateEvent),
    Done(WorkerDoneEvent),
    Crashed(WorkerCrashedEvent),
}
