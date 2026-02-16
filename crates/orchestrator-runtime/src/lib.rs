use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod pty_manager;

pub use pty_manager::{PtyManager, PtyOutputSubscription, PtySpawnSpec, TerminalSize};

macro_rules! runtime_string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime configuration error: {0}")]
    Configuration(String),
    #[error("runtime dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("runtime session not found: {0}")]
    SessionNotFound(String),
    #[error("runtime process error: {0}")]
    Process(String),
    #[error("runtime protocol error: {0}")]
    Protocol(String),
    #[error("runtime internal error: {0}")]
    Internal(String),
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;

runtime_string_id!(RuntimeSessionId);
runtime_string_id!(RuntimeArtifactId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    OpenCode,
    Codex,
    ClaudeCode,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub structured_events: bool,
    pub session_export: bool,
    pub diff_provider: bool,
    pub supports_background: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub session_id: RuntimeSessionId,
    pub workdir: PathBuf,
    pub model: Option<String>,
    pub instruction_prelude: Option<String>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandle {
    pub session_id: RuntimeSessionId,
    pub backend: BackendKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendOutputEvent {
    pub stream: BackendOutputStream,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCheckpointEvent {
    pub summary: String,
    pub detail: Option<String>,
    pub file_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendNeedsInputEvent {
    pub prompt_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub default_option: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendBlockedEvent {
    pub reason: String,
    pub hint: Option<String>,
    pub log_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendArtifactKind {
    Diff,
    PullRequest,
    TestRun,
    LogSnippet,
    Link,
    SessionExport,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendArtifactEvent {
    pub kind: BackendArtifactKind,
    pub artifact_id: Option<RuntimeArtifactId>,
    pub label: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendDoneEvent {
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCrashedEvent {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendEvent {
    Output(BackendOutputEvent),
    Checkpoint(BackendCheckpointEvent),
    NeedsInput(BackendNeedsInputEvent),
    Blocked(BackendBlockedEvent),
    Artifact(BackendArtifactEvent),
    Done(BackendDoneEvent),
    Crashed(BackendCrashedEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub lines: Vec<String>,
}

#[async_trait]
pub trait WorkerEventSubscription: Send {
    async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>>;
}

pub type WorkerEventStream = Box<dyn WorkerEventSubscription>;

#[async_trait]
pub trait SessionLifecycle: Send + Sync {
    async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle>;
    async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()>;
    async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()>;
    async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()>;
}

#[async_trait]
pub trait WorkerBackend: SessionLifecycle + Send + Sync {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> BackendCapabilities;

    async fn health_check(&self) -> RuntimeResult<()>;
    async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream>;
    async fn snapshot(&self, session: &SessionHandle) -> RuntimeResult<TerminalSnapshot>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyWorkerStream;

    #[async_trait]
    impl WorkerEventSubscription for EmptyWorkerStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            Ok(None)
        }
    }

    #[test]
    fn runtime_session_id_round_trips_as_string() {
        let session_id = RuntimeSessionId::new("sess-1");
        let serialized = serde_json::to_string(&session_id).expect("serialize session id");
        let deserialized: RuntimeSessionId =
            serde_json::from_str(&serialized).expect("deserialize session id");

        assert_eq!(serialized, "\"sess-1\"");
        assert_eq!(deserialized, session_id);
    }

    #[test]
    fn backend_kind_serialization_is_stable_for_persistence() {
        let serialized = serde_json::to_string(&BackendKind::OpenCode).expect("serialize kind");
        let parsed: BackendKind = serde_json::from_str("\"OpenCode\"").expect("parse kind");

        assert_eq!(serialized, "\"OpenCode\"");
        assert_eq!(parsed, BackendKind::OpenCode);
    }

    #[test]
    fn worker_event_stream_alias_accepts_trait_objects() {
        let _stream: WorkerEventStream = Box::new(EmptyWorkerStream);
    }
}
