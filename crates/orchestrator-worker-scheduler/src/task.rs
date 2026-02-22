use std::collections::HashMap;

use orchestrator_worker_protocol::ids::WorkerSessionId;

pub type WorkerSchedulerTaskId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSchedulerAction {
    Checkpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSchedulerTask {
    pub session_id: WorkerSessionId,
    pub interval_seconds: u64,
    pub action: WorkerSchedulerAction,
}

impl WorkerSchedulerTask {
    pub fn checkpoint(session_id: WorkerSessionId, interval_seconds: u64) -> Self {
        Self {
            session_id,
            interval_seconds,
            action: WorkerSchedulerAction::Checkpoint,
        }
    }
}

#[derive(Debug, Default)]
pub struct WorkerScheduler {
    next_task_id: WorkerSchedulerTaskId,
    tasks: HashMap<WorkerSchedulerTaskId, WorkerSchedulerTask>,
}

impl WorkerScheduler {
    pub fn register(&mut self, task: WorkerSchedulerTask) -> WorkerSchedulerTaskId {
        self.next_task_id = self
            .next_task_id
            .checked_add(1)
            .expect("worker scheduler task id space exhausted");
        self.tasks.insert(self.next_task_id, task);
        self.next_task_id
    }

    pub fn cancel(&mut self, task_id: WorkerSchedulerTaskId) -> bool {
        self.tasks.remove(&task_id).is_some()
    }

    pub fn task(&self, task_id: WorkerSchedulerTaskId) -> Option<&WorkerSchedulerTask> {
        self.tasks.get(&task_id)
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

#[cfg(test)]
mod tests {
    use orchestrator_worker_protocol::ids::WorkerSessionId;

    use super::{WorkerScheduler, WorkerSchedulerTask};

    #[test]
    #[should_panic(expected = "worker scheduler task id space exhausted")]
    fn register_panics_when_task_id_space_is_exhausted() {
        let mut scheduler = WorkerScheduler {
            next_task_id: u64::MAX,
            tasks: Default::default(),
        };

        let _ = scheduler.register(WorkerSchedulerTask::checkpoint(
            WorkerSessionId::new("sess-overflow"),
            60,
        ));
    }
}
