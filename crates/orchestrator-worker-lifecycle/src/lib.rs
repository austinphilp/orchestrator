//! Worker lifecycle registry scaffold.

pub mod registry;
pub mod state;

pub use registry::WorkerLifecycleRegistry;
pub use state::{WorkerSessionState, WorkerSessionVisibility};

#[cfg(test)]
mod tests {
    use orchestrator_worker_protocol::ids::WorkerSessionId;

    use super::{WorkerLifecycleRegistry, WorkerSessionState, WorkerSessionVisibility};

    #[test]
    fn lifecycle_state_defaults_to_starting() {
        assert_eq!(WorkerSessionState::default(), WorkerSessionState::Starting);
        assert_eq!(
            WorkerSessionVisibility::default(),
            WorkerSessionVisibility::Focused
        );
    }

    #[test]
    fn reserve_rejects_duplicate_session_ids() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-1");

        assert!(registry.reserve_session(session_id.clone()));
        assert!(!registry.reserve_session(session_id));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn state_reports_terminal_values() {
        assert!(!WorkerSessionState::Starting.is_terminal());
        assert!(!WorkerSessionState::Running.is_terminal());
        assert!(WorkerSessionState::Done.is_terminal());
        assert!(WorkerSessionState::Crashed.is_terminal());
        assert!(WorkerSessionState::Killed.is_terminal());
    }
}
