use std::collections::hash_map::Entry;
use std::collections::HashMap;

use orchestrator_worker_protocol::ids::WorkerSessionId;

use crate::state::{WorkerSessionState, WorkerSessionVisibility};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLifecycleSnapshot {
    pub state: WorkerSessionState,
    pub visibility: WorkerSessionVisibility,
}

impl Default for WorkerLifecycleSnapshot {
    fn default() -> Self {
        Self {
            state: WorkerSessionState::Starting,
            visibility: WorkerSessionVisibility::Focused,
        }
    }
}

#[derive(Debug, Default)]
pub struct WorkerLifecycleRegistry {
    sessions: HashMap<WorkerSessionId, WorkerLifecycleSnapshot>,
}

impl WorkerLifecycleRegistry {
    pub fn reserve_session(&mut self, session_id: WorkerSessionId) -> bool {
        match self.sessions.entry(session_id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(WorkerLifecycleSnapshot::default());
                true
            }
        }
    }

    pub fn snapshot(&self, session_id: &WorkerSessionId) -> Option<&WorkerLifecycleSnapshot> {
        self.sessions.get(session_id)
    }

    pub fn set_state(&mut self, session_id: &WorkerSessionId, state: WorkerSessionState) -> bool {
        let Some(snapshot) = self.sessions.get_mut(session_id) else {
            return false;
        };
        snapshot.state = state;
        true
    }

    pub fn set_visibility(
        &mut self,
        session_id: &WorkerSessionId,
        visibility: WorkerSessionVisibility,
    ) -> bool {
        let Some(snapshot) = self.sessions.get_mut(session_id) else {
            return false;
        };
        snapshot.visibility = visibility;
        true
    }

    pub fn remove_session(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> Option<WorkerLifecycleSnapshot> {
        self.sessions.remove(session_id)
    }

    pub fn list_sessions(&self) -> Vec<WorkerSessionId> {
        self.sessions.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}
