use backend_codex::{CodexBackend, CodexBackendConfig};
use orchestrator_runtime::{BackendCapabilities, BackendKind, WorkerBackend};

fn assert_legacy_worker_backend<T: WorkerBackend>() {}

#[test]
fn codex_backend_still_satisfies_legacy_worker_backend_trait() {
    assert_legacy_worker_backend::<CodexBackend>();
}

#[test]
fn codex_backend_runtime_bridge_reports_kind_and_capabilities() {
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
