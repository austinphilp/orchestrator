pub use orchestrator_worker_protocol::{
    WorkerBackendCapabilities as HarnessBackendCapabilities,
    WorkerBackendInfo as HarnessBackendInfo, WorkerBackendKind as HarnessBackendKind,
    WorkerEvent as HarnessEvent, WorkerEventStream as HarnessEventStream,
    WorkerEventSubscription as HarnessEventSubscription, WorkerRuntimeError as HarnessRuntimeError,
    WorkerRuntimeResult as HarnessRuntimeResult, WorkerSessionControl as HarnessSessionControl,
    WorkerSessionHandle as HarnessSessionHandle, WorkerSessionId as HarnessSessionId,
    WorkerSessionStreamSource as HarnessSessionStreamSource,
    WorkerSpawnRequest as HarnessSpawnRequest,
};
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
    pub spawn_request: HarnessSpawnRequest,
    pub session_handle: HarnessSessionHandle,
    pub bootstrap_events: Vec<HarnessEvent>,
}

pub trait HarnessProvider: Send + Sync {
    fn kind(&self) -> HarnessProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

pub trait HarnessRuntimeProvider:
    HarnessProvider
    + HarnessSessionControl
    + HarnessSessionStreamSource
    + HarnessBackendInfo
    + Send
    + Sync
{
}

impl<T> HarnessRuntimeProvider for T where
    T: HarnessProvider
        + HarnessSessionControl
        + HarnessSessionStreamSource
        + HarnessBackendInfo
        + Send
        + Sync
{
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HarnessProviderError {
    #[error("unknown harness provider key: {0}")]
    UnknownProviderKey(String),
}
