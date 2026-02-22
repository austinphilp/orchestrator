use std::collections::HashMap;

use orchestrator_worker_protocol::{
    backend::WorkerBackendKind,
    error::{WorkerRuntimeError, WorkerRuntimeResult},
    ids::WorkerSessionId,
    session::WorkerSessionHandle,
};

use crate::state::{WorkerSessionState, WorkerSessionVisibility};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLifecycleSnapshot {
    pub backend: Option<WorkerBackendKind>,
    pub state: WorkerSessionState,
    pub visibility: WorkerSessionVisibility,
}

impl Default for WorkerLifecycleSnapshot {
    fn default() -> Self {
        Self {
            backend: None,
            state: WorkerSessionState::Starting,
            visibility: WorkerSessionVisibility::Background,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLifecycleSummary {
    pub session_id: WorkerSessionId,
    pub backend: Option<WorkerBackendKind>,
    pub state: WorkerSessionState,
    pub visibility: WorkerSessionVisibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTerminalTransition {
    pub session_id: WorkerSessionId,
    pub previous_state: WorkerSessionState,
    pub resulting_state: WorkerSessionState,
    pub released_handle: Option<WorkerSessionHandle>,
    pub was_focused: bool,
}

#[derive(Debug, Clone)]
struct WorkerLifecycleSession {
    snapshot: WorkerLifecycleSnapshot,
    handle: Option<WorkerSessionHandle>,
}

impl WorkerLifecycleSession {
    fn starting() -> Self {
        Self {
            snapshot: WorkerLifecycleSnapshot::default(),
            handle: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct WorkerLifecycleRegistry {
    sessions: HashMap<WorkerSessionId, WorkerLifecycleSession>,
}

impl WorkerLifecycleRegistry {
    pub fn reserve_session(&mut self, session_id: WorkerSessionId) -> WorkerRuntimeResult<()> {
        if self.sessions.contains_key(&session_id) {
            return Err(WorkerRuntimeError::Configuration(format!(
                "worker session already exists: {}",
                session_id.as_str()
            )));
        }

        self.sessions
            .insert(session_id, WorkerLifecycleSession::starting());
        Ok(())
    }

    pub fn mark_spawned(
        &mut self,
        reserved_session_id: &WorkerSessionId,
        handle: WorkerSessionHandle,
    ) -> WorkerRuntimeResult<()> {
        if handle.session_id != *reserved_session_id {
            self.sessions.remove(reserved_session_id);
            return Err(WorkerRuntimeError::Internal(format!(
                "backend returned mismatched session id: expected {}, got {}",
                reserved_session_id.as_str(),
                handle.session_id.as_str()
            )));
        }

        let session = self.sessions.get_mut(reserved_session_id).ok_or_else(|| {
            WorkerRuntimeError::Internal(format!(
                "worker session reservation missing during install: {}",
                reserved_session_id.as_str()
            ))
        })?;
        if session.snapshot.state != WorkerSessionState::Starting {
            return Err(WorkerRuntimeError::Process(format!(
                "worker session is not starting: {} ({:?})",
                reserved_session_id.as_str(),
                session.snapshot.state
            )));
        }

        session.snapshot.state = WorkerSessionState::Running;
        session.snapshot.backend = Some(handle.backend.clone());
        session.snapshot.visibility = WorkerSessionVisibility::Background;
        session.handle = Some(handle);
        Ok(())
    }

    pub fn rollback_spawn(&mut self, session_id: &WorkerSessionId) -> WorkerRuntimeResult<()> {
        match self.sessions.get(session_id) {
            Some(session) if session.snapshot.state == WorkerSessionState::Starting => {}
            Some(session) => {
                return Err(WorkerRuntimeError::Process(format!(
                    "worker session is not starting: {} ({:?})",
                    session_id.as_str(),
                    session.snapshot.state
                )))
            }
            None => {
                return Err(WorkerRuntimeError::SessionNotFound(
                    session_id.as_str().to_owned(),
                ))
            }
        }

        self.sessions.remove(session_id);
        Ok(())
    }

    pub fn mark_done(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerTerminalTransition> {
        self.transition_terminal(session_id, WorkerSessionState::Done)
    }

    pub fn mark_crashed(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerTerminalTransition> {
        self.transition_terminal(session_id, WorkerSessionState::Crashed)
    }

    pub fn mark_killed(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerTerminalTransition> {
        self.transition_terminal(session_id, WorkerSessionState::Killed)
    }

    pub fn transition_terminal(
        &mut self,
        session_id: &WorkerSessionId,
        terminal_state: WorkerSessionState,
    ) -> WorkerRuntimeResult<WorkerTerminalTransition> {
        if !terminal_state.is_terminal() {
            return Err(WorkerRuntimeError::Configuration(format!(
                "worker terminal transition requires terminal state: {:?}",
                terminal_state
            )));
        }

        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| WorkerRuntimeError::SessionNotFound(session_id.as_str().to_owned()))?;

        let previous_state = session.snapshot.state;
        let resulting_state = match (previous_state, terminal_state) {
            (WorkerSessionState::Starting, _) => {
                return Err(WorkerRuntimeError::Process(format!(
                    "worker session is still starting: {}",
                    session_id.as_str()
                )))
            }
            (WorkerSessionState::Running, next) => next,
            // Terminal state updates are intentionally sticky. Backend streams can emit
            // duplicate or out-of-order terminal signals; we preserve the first terminal
            // transition except that `Killed` remains dominant once set.
            (WorkerSessionState::Done, _) => WorkerSessionState::Done,
            (WorkerSessionState::Crashed, _) => WorkerSessionState::Crashed,
            (WorkerSessionState::Killed, _) => WorkerSessionState::Killed,
        };

        let was_focused = session.snapshot.visibility == WorkerSessionVisibility::Focused;
        session.snapshot.state = resulting_state;
        session.snapshot.visibility = WorkerSessionVisibility::Background;
        let released_handle = session.handle.take();

        Ok(WorkerTerminalTransition {
            session_id: session_id.clone(),
            previous_state,
            resulting_state,
            released_handle,
            was_focused,
        })
    }

    pub fn running_session_handle(
        &self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerSessionHandle> {
        match self.sessions.get(session_id) {
            Some(session) if session.snapshot.state == WorkerSessionState::Starting => {
                Err(WorkerRuntimeError::Process(format!(
                    "worker session is still starting: {}",
                    session_id.as_str()
                )))
            }
            Some(session) if session.snapshot.state == WorkerSessionState::Running => {
                session.handle.clone().ok_or_else(|| {
                    WorkerRuntimeError::Internal(format!(
                        "worker session handle missing while running: {}",
                        session_id.as_str()
                    ))
                })
            }
            Some(session) => Err(WorkerRuntimeError::Process(format!(
                "worker session is not running: {} ({:?})",
                session_id.as_str(),
                session.snapshot.state
            ))),
            None => Err(WorkerRuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            )),
        }
    }

    pub fn status(&self, session_id: &WorkerSessionId) -> WorkerRuntimeResult<WorkerSessionState> {
        self.sessions
            .get(session_id)
            .map(|session| session.snapshot.state)
            .ok_or_else(|| WorkerRuntimeError::SessionNotFound(session_id.as_str().to_owned()))
    }

    pub fn snapshot(&self, session_id: &WorkerSessionId) -> Option<WorkerLifecycleSnapshot> {
        self.sessions
            .get(session_id)
            .map(|session| session.snapshot.clone())
    }

    pub fn set_visibility(
        &mut self,
        session_id: &WorkerSessionId,
        visibility: WorkerSessionVisibility,
    ) -> WorkerRuntimeResult<()> {
        if visibility == WorkerSessionVisibility::Focused {
            return self.focus_session(Some(session_id));
        }

        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| WorkerRuntimeError::SessionNotFound(session_id.as_str().to_owned()))?;
        if session.snapshot.state == WorkerSessionState::Starting {
            return Err(WorkerRuntimeError::Process(format!(
                "worker session is still starting: {}",
                session_id.as_str()
            )));
        }
        session.snapshot.visibility = visibility;
        Ok(())
    }

    pub fn focus_session(&mut self, focused: Option<&WorkerSessionId>) -> WorkerRuntimeResult<()> {
        if let Some(target) = focused {
            let session = self
                .sessions
                .get(target)
                .ok_or_else(|| WorkerRuntimeError::SessionNotFound(target.as_str().to_owned()))?;
            if session.snapshot.state != WorkerSessionState::Running {
                return Err(WorkerRuntimeError::Process(format!(
                    "worker session is not running: {} ({:?})",
                    target.as_str(),
                    session.snapshot.state
                )));
            }
        }

        for session in self.sessions.values_mut() {
            session.snapshot.visibility = WorkerSessionVisibility::Background;
        }
        if let Some(target) = focused {
            if let Some(session) = self.sessions.get_mut(target) {
                session.snapshot.visibility = WorkerSessionVisibility::Focused;
            }
        }
        Ok(())
    }

    pub fn list_sessions(&self) -> Vec<WorkerLifecycleSummary> {
        let mut sessions = self
            .sessions
            .iter()
            .map(|(session_id, session)| WorkerLifecycleSummary {
                session_id: session_id.clone(),
                backend: session.snapshot.backend.clone(),
                state: session.snapshot.state,
                visibility: session.snapshot.visibility,
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
        sessions
    }

    pub fn remove_session(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerLifecycleSnapshot> {
        let snapshot = self
            .sessions
            .get(session_id)
            .map(|session| session.snapshot.clone())
            .ok_or_else(|| WorkerRuntimeError::SessionNotFound(session_id.as_str().to_owned()))?;

        match snapshot.state {
            WorkerSessionState::Done | WorkerSessionState::Crashed | WorkerSessionState::Killed => {
            }
            WorkerSessionState::Starting => {
                return Err(WorkerRuntimeError::Process(format!(
                    "worker session is still starting: {}",
                    session_id.as_str()
                )))
            }
            WorkerSessionState::Running => {
                return Err(WorkerRuntimeError::Process(format!(
                    "worker session is still running: {}",
                    session_id.as_str()
                )))
            }
        }

        let removed = self.sessions.remove(session_id).ok_or_else(|| {
            WorkerRuntimeError::Internal(format!(
                "worker session vanished during removal: {}",
                session_id.as_str()
            ))
        })?;
        Ok(removed.snapshot)
    }

    pub fn list_session_ids(&self) -> Vec<WorkerSessionId> {
        let mut sessions = self.sessions.keys().cloned().collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        sessions
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}
