//! Worker eventbus scaffold.

pub mod bus;
pub mod envelope;

pub use bus::WorkerEventBus;
pub use envelope::WorkerEventEnvelope;

#[cfg(test)]
mod tests {
    use orchestrator_worker_protocol::event::WorkerEvent;
    use orchestrator_worker_protocol::ids::WorkerSessionId;

    use super::WorkerEventBus;

    #[test]
    fn publish_allocates_monotonic_sequence_numbers() {
        let mut bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");

        let first = bus.publish(session_id.clone(), WorkerEvent::Done(Default::default()));
        let second = bus.publish(session_id, WorkerEvent::Done(Default::default()));

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert!(second.received_at_monotonic_nanos >= first.received_at_monotonic_nanos);
    }
}
