//! Worker lifecycle state machine and session registry.

pub mod registry;
pub mod state;

pub use registry::{
    WorkerLifecycleRegistry, WorkerLifecycleSnapshot, WorkerLifecycleSummary,
    WorkerTerminalTransition,
};
pub use state::{WorkerSessionState, WorkerSessionVisibility};

#[cfg(test)]
mod tests {
    use orchestrator_worker_protocol::{
        backend::WorkerBackendKind, error::WorkerRuntimeError, ids::WorkerSessionId,
        session::WorkerSessionHandle,
    };

    use super::{WorkerLifecycleRegistry, WorkerSessionState, WorkerSessionVisibility};

    fn session_handle(
        session_id: &WorkerSessionId,
        backend: WorkerBackendKind,
    ) -> WorkerSessionHandle {
        WorkerSessionHandle {
            session_id: session_id.clone(),
            backend,
        }
    }

    #[test]
    fn lifecycle_state_defaults_to_starting_and_background() {
        assert_eq!(WorkerSessionState::default(), WorkerSessionState::Starting);
        assert_eq!(
            WorkerSessionVisibility::default(),
            WorkerSessionVisibility::Background
        );
    }

    #[test]
    fn reserve_rejects_duplicate_session_ids() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-dup");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session once");
        let duplicate = registry.reserve_session(session_id.clone());

        assert!(matches!(
            duplicate,
            Err(WorkerRuntimeError::Configuration(_))
        ));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn spawn_success_transitions_starting_to_running() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-running");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");
        registry
            .mark_spawned(
                &session_id,
                session_handle(&session_id, WorkerBackendKind::OpenCode),
            )
            .expect("install spawned session");

        assert_eq!(
            registry.status(&session_id).expect("session status"),
            WorkerSessionState::Running
        );

        let snapshot = registry.snapshot(&session_id).expect("snapshot");
        assert_eq!(snapshot.backend, Some(WorkerBackendKind::OpenCode));
        assert_eq!(snapshot.visibility, WorkerSessionVisibility::Background);
        assert_eq!(
            registry
                .running_session_handle(&session_id)
                .expect("running session handle")
                .session_id,
            session_id
        );

        let sessions = registry.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
        assert_eq!(sessions[0].state, WorkerSessionState::Running);
    }

    #[test]
    fn spawn_failure_rollback_removes_reserved_session() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-rollback");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");
        registry
            .rollback_spawn(&session_id)
            .expect("rollback spawn reservation");

        assert!(matches!(
            registry.status(&session_id),
            Err(WorkerRuntimeError::SessionNotFound(_))
        ));
        assert!(registry.is_empty());
    }

    #[test]
    fn spawn_mismatched_handle_rolls_back_reservation() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-expected");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");

        let mismatch = registry.mark_spawned(
            &session_id,
            session_handle(
                &WorkerSessionId::new("sess-other"),
                WorkerBackendKind::Codex,
            ),
        );

        assert!(matches!(mismatch, Err(WorkerRuntimeError::Internal(_))));
        assert!(matches!(
            registry.status(&session_id),
            Err(WorkerRuntimeError::SessionNotFound(_))
        ));
        assert!(registry.is_empty());
    }

    #[test]
    fn terminal_transition_releases_handle_and_clears_focus() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-done");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");
        registry
            .mark_spawned(
                &session_id,
                session_handle(&session_id, WorkerBackendKind::OpenCode),
            )
            .expect("install spawned session");
        registry
            .focus_session(Some(&session_id))
            .expect("focus session");

        let transition = registry.mark_done(&session_id).expect("mark done");
        assert_eq!(transition.previous_state, WorkerSessionState::Running);
        assert_eq!(transition.resulting_state, WorkerSessionState::Done);
        assert!(transition.released_handle.is_some());
        assert!(transition.was_focused);

        let snapshot = registry.snapshot(&session_id).expect("snapshot");
        assert_eq!(snapshot.state, WorkerSessionState::Done);
        assert_eq!(snapshot.visibility, WorkerSessionVisibility::Background);
        assert!(matches!(
            registry.running_session_handle(&session_id),
            Err(WorkerRuntimeError::Process(_))
        ));
    }

    #[test]
    fn killed_state_wins_against_late_done_event() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-killed");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");
        registry
            .mark_spawned(
                &session_id,
                session_handle(&session_id, WorkerBackendKind::OpenCode),
            )
            .expect("install spawned session");

        let killed = registry.mark_killed(&session_id).expect("mark killed");
        assert_eq!(killed.resulting_state, WorkerSessionState::Killed);
        assert!(killed.released_handle.is_some());

        let late_done = registry.mark_done(&session_id).expect("late done");
        assert_eq!(late_done.previous_state, WorkerSessionState::Killed);
        assert_eq!(late_done.resulting_state, WorkerSessionState::Killed);
        assert!(late_done.released_handle.is_none());

        assert_eq!(
            registry
                .status(&session_id)
                .expect("status after late done"),
            WorkerSessionState::Killed
        );
    }

    #[test]
    fn first_terminal_state_is_sticky_for_duplicate_terminal_events() {
        let mut done_registry = WorkerLifecycleRegistry::default();
        let done_session = WorkerSessionId::new("sess-done-sticky");

        done_registry
            .reserve_session(done_session.clone())
            .expect("reserve done session");
        done_registry
            .mark_spawned(
                &done_session,
                session_handle(&done_session, WorkerBackendKind::OpenCode),
            )
            .expect("spawn done session");
        done_registry
            .mark_done(&done_session)
            .expect("mark done first");
        let late_crash = done_registry
            .mark_crashed(&done_session)
            .expect("late crash should not fail");
        assert_eq!(late_crash.resulting_state, WorkerSessionState::Done);

        let mut crashed_registry = WorkerLifecycleRegistry::default();
        let crashed_session = WorkerSessionId::new("sess-crashed-sticky");
        crashed_registry
            .reserve_session(crashed_session.clone())
            .expect("reserve crashed session");
        crashed_registry
            .mark_spawned(
                &crashed_session,
                session_handle(&crashed_session, WorkerBackendKind::Codex),
            )
            .expect("spawn crashed session");
        crashed_registry
            .mark_crashed(&crashed_session)
            .expect("mark crashed first");
        let late_done = crashed_registry
            .mark_done(&crashed_session)
            .expect("late done should not fail");
        assert_eq!(late_done.resulting_state, WorkerSessionState::Crashed);
    }

    #[test]
    fn focus_switches_other_sessions_to_background() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_a = WorkerSessionId::new("sess-a");
        let session_b = WorkerSessionId::new("sess-b");

        registry
            .reserve_session(session_a.clone())
            .expect("reserve a");
        registry
            .mark_spawned(
                &session_a,
                session_handle(&session_a, WorkerBackendKind::OpenCode),
            )
            .expect("spawn a");
        registry
            .reserve_session(session_b.clone())
            .expect("reserve b");
        registry
            .mark_spawned(
                &session_b,
                session_handle(&session_b, WorkerBackendKind::Codex),
            )
            .expect("spawn b");

        registry
            .focus_session(Some(&session_a))
            .expect("focus session a");

        let session_a_visibility = registry
            .snapshot(&session_a)
            .expect("session a snapshot")
            .visibility;
        let session_b_visibility = registry
            .snapshot(&session_b)
            .expect("session b snapshot")
            .visibility;

        assert_eq!(session_a_visibility, WorkerSessionVisibility::Focused);
        assert_eq!(session_b_visibility, WorkerSessionVisibility::Background);

        registry.focus_session(None).expect("clear focus");
        assert_eq!(
            registry
                .snapshot(&session_a)
                .expect("a snapshot")
                .visibility,
            WorkerSessionVisibility::Background
        );
        assert_eq!(
            registry
                .snapshot(&session_b)
                .expect("b snapshot")
                .visibility,
            WorkerSessionVisibility::Background
        );
    }

    #[test]
    fn focus_rejects_non_running_session() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-non-running");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");
        registry
            .mark_spawned(
                &session_id,
                session_handle(&session_id, WorkerBackendKind::OpenCode),
            )
            .expect("spawn session");
        registry.mark_done(&session_id).expect("transition to done");

        assert!(matches!(
            registry.focus_session(Some(&session_id)),
            Err(WorkerRuntimeError::Process(_))
        ));
    }

    #[test]
    fn set_visibility_to_focused_uses_single_focus_invariant() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_a = WorkerSessionId::new("sess-set-focus-a");
        let session_b = WorkerSessionId::new("sess-set-focus-b");

        registry
            .reserve_session(session_a.clone())
            .expect("reserve a");
        registry
            .mark_spawned(
                &session_a,
                session_handle(&session_a, WorkerBackendKind::OpenCode),
            )
            .expect("spawn a");
        registry
            .reserve_session(session_b.clone())
            .expect("reserve b");
        registry
            .mark_spawned(
                &session_b,
                session_handle(&session_b, WorkerBackendKind::Codex),
            )
            .expect("spawn b");

        registry
            .set_visibility(&session_a, WorkerSessionVisibility::Focused)
            .expect("focus a");
        registry
            .set_visibility(&session_b, WorkerSessionVisibility::Focused)
            .expect("focus b");

        assert_eq!(
            registry
                .snapshot(&session_a)
                .expect("snapshot a")
                .visibility,
            WorkerSessionVisibility::Background
        );
        assert_eq!(
            registry
                .snapshot(&session_b)
                .expect("snapshot b")
                .visibility,
            WorkerSessionVisibility::Focused
        );
    }

    #[test]
    fn remove_session_requires_terminal_state() {
        let mut registry = WorkerLifecycleRegistry::default();
        let session_id = WorkerSessionId::new("sess-remove");

        registry
            .reserve_session(session_id.clone())
            .expect("reserve session");
        assert!(matches!(
            registry.remove_session(&session_id),
            Err(WorkerRuntimeError::Process(_))
        ));

        registry
            .mark_spawned(
                &session_id,
                session_handle(&session_id, WorkerBackendKind::OpenCode),
            )
            .expect("spawn session");
        assert!(matches!(
            registry.remove_session(&session_id),
            Err(WorkerRuntimeError::Process(_))
        ));

        registry.mark_done(&session_id).expect("mark done");
        let removed = registry
            .remove_session(&session_id)
            .expect("remove done session");
        assert_eq!(removed.state, WorkerSessionState::Done);
        assert!(matches!(
            registry.remove_session(&session_id),
            Err(WorkerRuntimeError::SessionNotFound(_))
        ));
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
