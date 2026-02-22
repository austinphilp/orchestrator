use backend_codex::{CodexBackend, CodexBackendConfig};
use orchestrator_core::{BackendCapabilities, BackendKind, WorkerBackend};

fn assert_core_worker_backend<T: WorkerBackend>() {}

#[test]
fn codex_backend_satisfies_core_worker_backend_trait() {
    assert_core_worker_backend::<CodexBackend>();
}

#[test]
fn codex_backend_reports_expected_kind_and_capabilities() {
    let backend = CodexBackend::new(CodexBackendConfig::default());
    let runtime_backend: &dyn WorkerBackend = &backend;

    assert_eq!(runtime_backend.kind(), BackendKind::Codex);
    assert_eq!(
        runtime_backend.capabilities(),
        BackendCapabilities {
            structured_events: true,
            ..BackendCapabilities::default()
        }
    );
}
