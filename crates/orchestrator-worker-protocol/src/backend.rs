use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{WorkerRuntimeError, WorkerRuntimeResult};
use crate::event::{WorkerEvent, WorkerNeedsInputAnswer};
use crate::session::{WorkerSessionHandle, WorkerSpawnRequest};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerBackendKind {
    OpenCode,
    Codex,
    ClaudeCode,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkerBackendCapabilities {
    pub structured_events: bool,
    pub session_export: bool,
    pub diff_provider: bool,
    pub supports_background: bool,
}

#[async_trait]
pub trait WorkerEventSubscription: Send {
    async fn next_event(&mut self) -> WorkerRuntimeResult<Option<WorkerEvent>>;
}

pub type WorkerEventStream = Box<dyn WorkerEventSubscription>;

#[async_trait]
pub trait WorkerSessionControl: Send + Sync {
    async fn spawn(&self, spec: WorkerSpawnRequest) -> WorkerRuntimeResult<WorkerSessionHandle>;
    async fn kill(&self, session: &WorkerSessionHandle) -> WorkerRuntimeResult<()>;
    async fn send_input(
        &self,
        session: &WorkerSessionHandle,
        input: &[u8],
    ) -> WorkerRuntimeResult<()>;
    async fn respond_to_needs_input(
        &self,
        session: &WorkerSessionHandle,
        prompt_id: &str,
        answers: &[WorkerNeedsInputAnswer],
    ) -> WorkerRuntimeResult<()> {
        let _ = (session, prompt_id, answers);
        Err(WorkerRuntimeError::Protocol(
            "needs-input responses are not supported by this backend".to_owned(),
        ))
    }
    async fn resize(
        &self,
        session: &WorkerSessionHandle,
        cols: u16,
        rows: u16,
    ) -> WorkerRuntimeResult<()>;
}

#[async_trait]
pub trait WorkerSessionStreamSource: Send + Sync {
    async fn subscribe(
        &self,
        session: &WorkerSessionHandle,
    ) -> WorkerRuntimeResult<WorkerEventStream>;
    async fn harness_session_id(
        &self,
        _session: &WorkerSessionHandle,
    ) -> WorkerRuntimeResult<Option<String>> {
        Ok(None)
    }
}

#[async_trait]
pub trait WorkerBackendInfo: Send + Sync {
    fn kind(&self) -> WorkerBackendKind;
    fn capabilities(&self) -> WorkerBackendCapabilities;
    async fn health_check(&self) -> WorkerRuntimeResult<()>;
}
