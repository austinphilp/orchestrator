use backend_opencode::{
    OpenCodeBackend, OpenCodeBackendConfig, OpenCodeHarnessProvider, OpenCodeHarnessProviderConfig,
};
use orchestrator_core::{BackendKind, WorkerBackend};
use orchestrator_harness::{
    OpenCodeBackend as HarnessOpenCodeBackend,
    OpenCodeBackendConfig as HarnessOpenCodeBackendConfig,
};

fn assert_core_worker_backend<T: WorkerBackend>() {}

#[test]
fn opencode_backend_satisfies_core_worker_backend_trait() {
    assert_core_worker_backend::<OpenCodeBackend>();
}

#[test]
fn opencode_backend_reports_expected_kind() {
    let backend = OpenCodeBackend::new(OpenCodeBackendConfig::default());
    let runtime_backend: &dyn WorkerBackend = &backend;

    assert_eq!(runtime_backend.kind(), BackendKind::OpenCode);
}

#[test]
fn reexports_match_harness_opencode_types() {
    let backend: HarnessOpenCodeBackend = OpenCodeBackend::new(OpenCodeBackendConfig::default());
    let provider: OpenCodeHarnessProvider = backend;
    let _config: HarnessOpenCodeBackendConfig = OpenCodeHarnessProviderConfig::default();
    let runtime_backend: &dyn WorkerBackend = &provider;
    assert_eq!(runtime_backend.kind(), BackendKind::OpenCode);
}
