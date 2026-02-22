use backend_opencode::OpenCodeBackend;
use orchestrator_runtime::WorkerBackend;

fn assert_legacy_worker_backend<T: WorkerBackend>() {}

#[test]
fn opencode_backend_still_satisfies_legacy_worker_backend_trait() {
    assert_legacy_worker_backend::<OpenCodeBackend>();
}
