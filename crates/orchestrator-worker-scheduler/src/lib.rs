//! Shared scheduler scaffold for worker runtime actions.

pub mod task;

pub use task::{WorkerScheduler, WorkerSchedulerTask};

#[cfg(test)]
mod tests {
    use orchestrator_worker_protocol::ids::WorkerSessionId;

    use super::{WorkerScheduler, WorkerSchedulerTask};

    #[test]
    fn register_and_cancel_task_round_trips() {
        let mut scheduler = WorkerScheduler::default();
        let task = WorkerSchedulerTask::checkpoint(WorkerSessionId::new("sess-1"), 30);

        let task_id = scheduler.register(task);
        assert_eq!(scheduler.task_count(), 1);
        assert_eq!(
            scheduler.task(task_id).map(|task| task.interval_seconds),
            Some(30)
        );
        assert!(scheduler.cancel(task_id));
        assert!(!scheduler.cancel(task_id));
        assert_eq!(scheduler.task_count(), 0);
    }
}
