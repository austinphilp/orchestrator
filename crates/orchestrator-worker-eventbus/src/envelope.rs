use orchestrator_worker_protocol::event::WorkerEvent;
use orchestrator_worker_protocol::ids::WorkerSessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerEventEnvelope {
    pub session_id: WorkerSessionId,
    pub sequence: u64,
    pub received_at_monotonic_nanos: u64,
    pub event: WorkerEvent,
}
