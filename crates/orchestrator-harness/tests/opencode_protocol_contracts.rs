use orchestrator_harness::{
    CodexBackend, CodexBackendConfig, OpenCodeBackend, OpenCodeBackendConfig,
};
use orchestrator_worker_protocol::{
    WorkerBackendInfo, WorkerBackendKind as BackendKind, WorkerSessionControl,
    WorkerSessionStreamSource,
};

fn assert_worker_protocol_backend<T>()
where
    T: WorkerSessionControl + WorkerSessionStreamSource + WorkerBackendInfo,
{
}

#[test]
fn opencode_provider_satisfies_worker_protocol_trait_set() {
    assert_worker_protocol_backend::<OpenCodeBackend>();
}

#[test]
fn codex_provider_satisfies_worker_protocol_trait_set() {
    assert_worker_protocol_backend::<CodexBackend>();
}

#[test]
fn opencode_provider_reports_expected_kind() {
    let backend = OpenCodeBackend::new(OpenCodeBackendConfig::default());
    assert_eq!(WorkerBackendInfo::kind(&backend), BackendKind::OpenCode);
}

#[test]
fn codex_provider_reports_expected_kind() {
    let backend = CodexBackend::new(CodexBackendConfig::default());
    assert_eq!(WorkerBackendInfo::kind(&backend), BackendKind::Codex);
}
