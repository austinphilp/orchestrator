use orchestrator_worker_protocol::{WorkerEvent, WorkerSessionHandle, WorkerSpawnRequest};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessProviderKind {
    OpenCode,
    Codex,
}

impl HarnessProviderKind {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::OpenCode => "harness.opencode",
            Self::Codex => "harness.codex",
        }
    }

    pub fn from_key(provider_key: &str) -> Option<Self> {
        match provider_key {
            "harness.opencode" => Some(Self::OpenCode),
            "harness.codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessProviderSessionContract {
    pub spawn_request: WorkerSpawnRequest,
    pub session_handle: WorkerSessionHandle,
    pub bootstrap_events: Vec<WorkerEvent>,
}

pub trait HarnessProvider: Send + Sync {
    fn kind(&self) -> HarnessProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HarnessProviderError {
    #[error("unknown harness provider key: {0}")]
    UnknownProviderKey(String),
}
