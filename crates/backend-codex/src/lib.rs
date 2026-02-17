use backend_opencode::OpenCodeBackend;
use orchestrator_runtime::{
    BackendCapabilities, BackendKind, RuntimeResult, SessionHandle, SpawnSpec,
    TerminalSnapshot, WorkerBackend, WorkerEventStream, SessionLifecycle,
};
use std::path::PathBuf;

const DEFAULT_CODEX_BINARY: &str = "codex";
const ENV_CODEX_BIN: &str = "ORCHESTRATOR_CODEX_BIN";

#[derive(Debug, Clone)]
pub struct CodexBackendConfig {
    pub binary: PathBuf,
}

impl Default for CodexBackendConfig {
    fn default() -> Self {
        Self {
            binary: std::env::var_os(ENV_CODEX_BIN)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CODEX_BINARY)),
        }
    }
}

#[derive(Clone)]
pub struct CodexBackend {
    inner: OpenCodeBackend,
}

impl CodexBackend {
    pub fn new(config: CodexBackendConfig) -> Self {
        let mut base = backend_opencode::OpenCodeBackendConfig::default();
        base.binary = config.binary;
        Self {
            inner: OpenCodeBackend::new(base),
        }
    }

    pub fn from_env() -> Self {
        Self::new(CodexBackendConfig::default())
    }
}

#[async_trait::async_trait]
impl SessionLifecycle for CodexBackend {
    async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<orchestrator_runtime::SessionHandle> {
        self.inner.spawn(spec).await
    }

    async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
        self.inner.kill(session).await
    }

    async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
        self.inner.send_input(session, input).await
    }

    async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()> {
        self.inner.resize(session, cols, rows).await
    }
}

#[async_trait::async_trait]
impl WorkerBackend for CodexBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    async fn health_check(&self) -> RuntimeResult<()> {
        self.inner.health_check().await
    }

    async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
        self.inner.subscribe(session).await
    }

    async fn snapshot(&self, session: &SessionHandle) -> RuntimeResult<TerminalSnapshot> {
        self.inner.snapshot(session).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_codex_binary_from_env_or_codex() {
        let config = CodexBackendConfig::default();
        assert!(!config.binary.as_os_str().is_empty());
    }

    #[test]
    fn backend_reports_codex_kind() {
        let backend = CodexBackend::from_env();
        assert_eq!(backend.kind(), BackendKind::Codex);
    }
}
