use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Instant;

use orchestrator_worker_protocol::event::WorkerEvent;
use orchestrator_worker_protocol::ids::WorkerSessionId;
use tokio::sync::broadcast;

use crate::envelope::WorkerEventEnvelope;

pub const DEFAULT_SESSION_BUFFER_CAPACITY: usize = 64;
pub const DEFAULT_GLOBAL_BUFFER_CAPACITY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerEventBusConfig {
    pub session_buffer_capacity: usize,
    pub global_buffer_capacity: usize,
}

impl Default for WorkerEventBusConfig {
    fn default() -> Self {
        Self {
            session_buffer_capacity: DEFAULT_SESSION_BUFFER_CAPACITY,
            global_buffer_capacity: DEFAULT_GLOBAL_BUFFER_CAPACITY,
        }
    }
}

#[derive(Debug)]
pub struct WorkerEventBus {
    next_sequence: AtomicU64,
    boot_instant: Instant,
    config: WorkerEventBusConfig,
    session_senders: RwLock<HashMap<WorkerSessionId, broadcast::Sender<WorkerEventEnvelope>>>,
    global_sender: broadcast::Sender<WorkerEventEnvelope>,
}

impl Default for WorkerEventBus {
    fn default() -> Self {
        Self::new(WorkerEventBusConfig::default())
    }
}

impl WorkerEventBus {
    pub fn new(config: WorkerEventBusConfig) -> Self {
        assert!(
            config.session_buffer_capacity > 0,
            "session_buffer_capacity must be greater than 0"
        );
        assert!(
            config.global_buffer_capacity > 0,
            "global_buffer_capacity must be greater than 0"
        );

        let (global_sender, _global_receiver) = broadcast::channel(config.global_buffer_capacity);
        Self {
            next_sequence: AtomicU64::new(0),
            boot_instant: Instant::now(),
            config,
            session_senders: RwLock::new(HashMap::new()),
            global_sender,
        }
    }

    pub fn subscribe_session(
        &self,
        session_id: WorkerSessionId,
    ) -> broadcast::Receiver<WorkerEventEnvelope> {
        if let Some(sender) = self.session_sender(&session_id) {
            return sender.subscribe();
        }

        let mut session_senders = self
            .session_senders
            .write()
            .expect("worker eventbus session sender lock poisoned");
        let sender = session_senders.entry(session_id).or_insert_with(|| {
            let (sender, _receiver) = broadcast::channel(self.config.session_buffer_capacity);
            sender
        });
        sender.subscribe()
    }

    pub fn subscribe_all(&self) -> broadcast::Receiver<WorkerEventEnvelope> {
        self.global_sender.subscribe()
    }

    pub fn remove_session(&self, session_id: &WorkerSessionId) -> bool {
        let mut session_senders = self
            .session_senders
            .write()
            .expect("worker eventbus session sender lock poisoned");
        session_senders.remove(session_id).is_some()
    }

    pub fn publish(&self, session_id: WorkerSessionId, event: WorkerEvent) -> WorkerEventEnvelope {
        let envelope = WorkerEventEnvelope {
            session_id,
            sequence: self.next_sequence(),
            received_at_monotonic_nanos: self.monotonic_nanos_since_bus_bootstrap(),
            event,
        };

        let session_sender = self.session_sender(&envelope.session_id);
        let has_session_receivers = session_sender
            .as_ref()
            .is_some_and(|sender| sender.receiver_count() > 0);
        let has_global_receivers = self.global_sender.receiver_count() > 0;

        match (has_session_receivers, has_global_receivers) {
            (true, true) => {
                let _ = session_sender
                    .as_ref()
                    .expect("session sender should exist when receiver count is non-zero")
                    .send(envelope.clone());
                let _ = self.global_sender.send(envelope.clone());
            }
            (true, false) => {
                let _ = session_sender
                    .as_ref()
                    .expect("session sender should exist when receiver count is non-zero")
                    .send(envelope.clone());
            }
            (false, true) => {
                let _ = self.global_sender.send(envelope.clone());
            }
            (false, false) => {}
        }

        envelope
    }

    fn session_sender(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<broadcast::Sender<WorkerEventEnvelope>> {
        let session_senders = self
            .session_senders
            .read()
            .expect("worker eventbus session sender lock poisoned");
        session_senders.get(session_id).cloned()
    }

    fn next_sequence(&self) -> u64 {
        let mut current = self.next_sequence.load(Ordering::Relaxed);
        loop {
            let next = current
                .checked_add(1)
                .expect("worker event sequence exhausted");
            match self.next_sequence.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }

    fn monotonic_nanos_since_bus_bootstrap(&self) -> u64 {
        let nanos = self.boot_instant.elapsed().as_nanos();
        u64::try_from(nanos).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use orchestrator_worker_protocol::event::WorkerEvent;
    use orchestrator_worker_protocol::ids::WorkerSessionId;
    use tokio::sync::broadcast::error::RecvError;
    use tokio::time::timeout;

    use super::WorkerEventBus;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    #[test]
    #[should_panic(expected = "worker event sequence exhausted")]
    fn publish_panics_when_sequence_space_is_exhausted() {
        let bus = WorkerEventBus::default();
        bus.next_sequence
            .store(u64::MAX, std::sync::atomic::Ordering::Relaxed);

        let _ = bus.publish(
            "sess-overflow".into(),
            WorkerEvent::Done(Default::default()),
        );
    }

    #[test]
    fn publish_allocates_monotonic_sequence_numbers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");

        let first = bus.publish(session_id.clone(), WorkerEvent::Done(Default::default()));
        let second = bus.publish(session_id, WorkerEvent::Done(Default::default()));

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert!(second.received_at_monotonic_nanos >= first.received_at_monotonic_nanos);
    }

    #[tokio::test]
    async fn publish_fans_out_to_session_and_global_subscribers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());
        let mut global_subscriber = bus.subscribe_all();

        let published = bus.publish(session_id, WorkerEvent::Done(Default::default()));

        let session_envelope = timeout(TEST_TIMEOUT, session_subscriber.recv())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        let global_envelope = timeout(TEST_TIMEOUT, global_subscriber.recv())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");

        assert_eq!(session_envelope, published);
        assert_eq!(global_envelope, published);
    }

    #[tokio::test]
    async fn session_subscriptions_only_receive_matching_session_events() {
        let bus = WorkerEventBus::default();
        let session_a = WorkerSessionId::new("sess-a");
        let session_b = WorkerSessionId::new("sess-b");
        let mut subscriber_a = bus.subscribe_session(session_a.clone());
        let mut subscriber_b = bus.subscribe_session(session_b.clone());

        let event_a = bus.publish(session_a.clone(), WorkerEvent::Done(Default::default()));
        let event_b = bus.publish(session_b.clone(), WorkerEvent::Done(Default::default()));

        let received_a = timeout(TEST_TIMEOUT, subscriber_a.recv())
            .await
            .expect("session a recv timed out")
            .expect("session a recv should succeed");
        let received_b = timeout(TEST_TIMEOUT, subscriber_b.recv())
            .await
            .expect("session b recv timed out")
            .expect("session b recv should succeed");

        assert_eq!(received_a, event_a);
        assert_eq!(received_b, event_b);
    }

    #[tokio::test]
    async fn global_subscription_receives_events_from_all_sessions() {
        let bus = WorkerEventBus::default();
        let mut global_subscriber = bus.subscribe_all();
        let session_a = WorkerSessionId::new("sess-a");
        let session_b = WorkerSessionId::new("sess-b");

        let event_a = bus.publish(session_a.clone(), WorkerEvent::Done(Default::default()));
        let event_b = bus.publish(session_b.clone(), WorkerEvent::Done(Default::default()));

        let received_a = timeout(TEST_TIMEOUT, global_subscriber.recv())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");
        let received_b = timeout(TEST_TIMEOUT, global_subscriber.recv())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");

        assert_eq!(received_a, event_a);
        assert_eq!(received_b, event_b);
    }

    #[tokio::test]
    async fn bounded_queue_reports_lag_for_slow_global_subscriber() {
        let bus = WorkerEventBus::new(super::WorkerEventBusConfig {
            session_buffer_capacity: 1,
            global_buffer_capacity: 1,
        });
        let session_id = WorkerSessionId::new("sess-a");
        let mut global_subscriber = bus.subscribe_all();

        for _ in 0..8 {
            let _ = bus.publish(session_id.clone(), WorkerEvent::Done(Default::default()));
        }

        let lagged = timeout(TEST_TIMEOUT, global_subscriber.recv())
            .await
            .expect("global recv timed out")
            .expect_err("expected lagged receiver due bounded buffer");

        match lagged {
            RecvError::Lagged(skipped) => assert!(skipped >= 1),
            RecvError::Closed => panic!("global channel unexpectedly closed"),
        }
    }

    #[tokio::test]
    async fn remove_session_closes_existing_session_subscribers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());

        assert!(bus.remove_session(&session_id));
        assert!(!bus.remove_session(&session_id));

        let closed = timeout(TEST_TIMEOUT, session_subscriber.recv())
            .await
            .expect("session recv timed out")
            .expect_err("session subscription should close after remove_session");
        assert!(matches!(closed, RecvError::Closed));
    }

    #[tokio::test]
    async fn subscribe_session_after_remove_recreates_session_channel() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let _subscriber = bus.subscribe_session(session_id.clone());

        assert!(bus.remove_session(&session_id));

        let mut refreshed_subscriber = bus.subscribe_session(session_id.clone());
        let published = bus.publish(session_id, WorkerEvent::Done(Default::default()));
        let received = timeout(TEST_TIMEOUT, refreshed_subscriber.recv())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        assert_eq!(received, published);
    }
}
