use backend_opencode::{OpenCodeBackend, OpenCodeBackendConfig};
use orchestrator_core::{BackendKind, WorkerBackend};

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
