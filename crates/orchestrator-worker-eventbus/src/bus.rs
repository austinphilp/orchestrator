use std::sync::OnceLock;
use std::time::Instant;

use orchestrator_worker_protocol::event::WorkerEvent;
use orchestrator_worker_protocol::ids::WorkerSessionId;

use crate::envelope::WorkerEventEnvelope;

#[derive(Debug, Default)]
pub struct WorkerEventBus {
    next_sequence: u64,
}

impl WorkerEventBus {
    pub fn publish(
        &mut self,
        session_id: WorkerSessionId,
        event: WorkerEvent,
    ) -> WorkerEventEnvelope {
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("worker event sequence exhausted");
        WorkerEventEnvelope {
            session_id,
            sequence: self.next_sequence,
            received_at_monotonic_nanos: monotonic_nanos_since_bus_bootstrap(),
            event,
        }
    }
}

fn monotonic_nanos_since_bus_bootstrap() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let nanos = START.get_or_init(Instant::now).elapsed().as_nanos();
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::WorkerEventBus;

    #[test]
    #[should_panic(expected = "worker event sequence exhausted")]
    fn publish_panics_when_sequence_space_is_exhausted() {
        let mut bus = WorkerEventBus {
            next_sequence: u64::MAX,
        };

        let _ = bus.publish(
            "sess-overflow".into(),
            orchestrator_worker_protocol::event::WorkerEvent::Done(Default::default()),
        );
    }
}
