//! Worker runtime composition scaffold.

use orchestrator_worker_eventbus::{WorkerEventBus, WorkerEventBusPerfSnapshot};
use orchestrator_worker_lifecycle::WorkerLifecycleRegistry;
use orchestrator_worker_scheduler::WorkerScheduler;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimePerfSnapshot {
    pub eventbus: WorkerEventBusPerfSnapshot,
}

#[derive(Debug, Default)]
pub struct WorkerRuntime {
    lifecycle: WorkerLifecycleRegistry,
    eventbus: WorkerEventBus,
    scheduler: WorkerScheduler,
}

impl WorkerRuntime {
    pub fn new(
        lifecycle: WorkerLifecycleRegistry,
        eventbus: WorkerEventBus,
        scheduler: WorkerScheduler,
    ) -> Self {
        Self {
            lifecycle,
            eventbus,
            scheduler,
        }
    }

    pub fn components(&self) -> (&WorkerLifecycleRegistry, &WorkerEventBus, &WorkerScheduler) {
        (&self.lifecycle, &self.eventbus, &self.scheduler)
    }

    pub fn components_mut(
        &mut self,
    ) -> (
        &mut WorkerLifecycleRegistry,
        &mut WorkerEventBus,
        &mut WorkerScheduler,
    ) {
        (&mut self.lifecycle, &mut self.eventbus, &mut self.scheduler)
    }

    pub fn into_parts(self) -> (WorkerLifecycleRegistry, WorkerEventBus, WorkerScheduler) {
        (self.lifecycle, self.eventbus, self.scheduler)
    }

    pub fn perf_snapshot(&self) -> WorkerRuntimePerfSnapshot {
        WorkerRuntimePerfSnapshot {
            eventbus: self.eventbus.perf_snapshot(),
        }
    }
}

#[cfg(test)]
mod tests {
    use orchestrator_worker_eventbus::WorkerEventBus;
    use orchestrator_worker_lifecycle::WorkerLifecycleRegistry;
    use orchestrator_worker_scheduler::WorkerScheduler;

    use super::WorkerRuntime;

    #[test]
    fn runtime_keeps_composed_components() {
        let mut runtime = WorkerRuntime::new(
            WorkerLifecycleRegistry::default(),
            WorkerEventBus::default(),
            WorkerScheduler::default(),
        );

        let _ = runtime.components();
        let _ = runtime.components_mut();
    }

    #[test]
    fn runtime_can_be_decomposed_back_into_parts() {
        let runtime = WorkerRuntime::default();
        let _ = runtime.into_parts();
    }

    #[test]
    fn runtime_perf_snapshot_exposes_eventbus_metrics() {
        let runtime = WorkerRuntime::default();
        let snapshot = runtime.perf_snapshot();
        assert_eq!(snapshot.eventbus.events_received_total, 0);
    }
}
