use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use orchestrator_worker_protocol::event::{WorkerEvent, WorkerOutputEvent, WorkerOutputStream};
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkerEventBusPerfSnapshot {
    pub captured_at_monotonic_nanos: u64,
    pub events_received_total: u64,
    pub events_published_total: u64,
    pub events_forwarded_session_total: u64,
    pub events_forwarded_global_total: u64,
    pub event_clone_ops_total: u64,
    pub event_dispatch_nanos_total: u64,
    pub queue_pressure_dropped_events_total: u64,
    pub output_truncation_markers_total: u64,
    pub subscribe_session_calls_total: u64,
    pub subscribe_global_calls_total: u64,
    pub subscriber_lagged_errors_session_total: u64,
    pub subscriber_lagged_errors_global_total: u64,
    pub subscriber_lagged_events_session_total: u64,
    pub subscriber_lagged_events_global_total: u64,
    pub subscriber_closed_session_total: u64,
    pub subscriber_closed_global_total: u64,
    pub session_receiver_count_last: u64,
    pub global_receiver_count_last: u64,
}

#[derive(Debug, Default)]
struct WorkerEventBusPerfCounters {
    events_received_total: AtomicU64,
    events_published_total: AtomicU64,
    events_forwarded_session_total: AtomicU64,
    events_forwarded_global_total: AtomicU64,
    event_clone_ops_total: AtomicU64,
    event_dispatch_nanos_total: AtomicU64,
    queue_pressure_dropped_events_total: AtomicU64,
    output_truncation_markers_total: AtomicU64,
    subscribe_session_calls_total: AtomicU64,
    subscribe_global_calls_total: AtomicU64,
    subscriber_lagged_errors_session_total: AtomicU64,
    subscriber_lagged_errors_global_total: AtomicU64,
    subscriber_lagged_events_session_total: AtomicU64,
    subscriber_lagged_events_global_total: AtomicU64,
    subscriber_closed_session_total: AtomicU64,
    subscriber_closed_global_total: AtomicU64,
    session_receiver_count_last: AtomicU64,
    global_receiver_count_last: AtomicU64,
}

impl WorkerEventBusPerfCounters {
    fn snapshot(&self, captured_at_monotonic_nanos: u64) -> WorkerEventBusPerfSnapshot {
        WorkerEventBusPerfSnapshot {
            captured_at_monotonic_nanos,
            events_received_total: self.events_received_total.load(Ordering::Relaxed),
            events_published_total: self.events_published_total.load(Ordering::Relaxed),
            events_forwarded_session_total: self
                .events_forwarded_session_total
                .load(Ordering::Relaxed),
            events_forwarded_global_total: self
                .events_forwarded_global_total
                .load(Ordering::Relaxed),
            event_clone_ops_total: self.event_clone_ops_total.load(Ordering::Relaxed),
            event_dispatch_nanos_total: self.event_dispatch_nanos_total.load(Ordering::Relaxed),
            queue_pressure_dropped_events_total: self
                .queue_pressure_dropped_events_total
                .load(Ordering::Relaxed),
            output_truncation_markers_total: self
                .output_truncation_markers_total
                .load(Ordering::Relaxed),
            subscribe_session_calls_total: self
                .subscribe_session_calls_total
                .load(Ordering::Relaxed),
            subscribe_global_calls_total: self.subscribe_global_calls_total.load(Ordering::Relaxed),
            subscriber_lagged_errors_session_total: self
                .subscriber_lagged_errors_session_total
                .load(Ordering::Relaxed),
            subscriber_lagged_errors_global_total: self
                .subscriber_lagged_errors_global_total
                .load(Ordering::Relaxed),
            subscriber_lagged_events_session_total: self
                .subscriber_lagged_events_session_total
                .load(Ordering::Relaxed),
            subscriber_lagged_events_global_total: self
                .subscriber_lagged_events_global_total
                .load(Ordering::Relaxed),
            subscriber_closed_session_total: self
                .subscriber_closed_session_total
                .load(Ordering::Relaxed),
            subscriber_closed_global_total: self
                .subscriber_closed_global_total
                .load(Ordering::Relaxed),
            session_receiver_count_last: self.session_receiver_count_last.load(Ordering::Relaxed),
            global_receiver_count_last: self.global_receiver_count_last.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct WorkerSessionEventSubscription {
    session_id: WorkerSessionId,
    receiver: broadcast::Receiver<WorkerEventEnvelope>,
    perf: Arc<WorkerEventBusPerfCounters>,
    boot_instant: Instant,
    last_observed_sequence: u64,
}

impl WorkerSessionEventSubscription {
    pub async fn next_event(&mut self) -> Option<WorkerEventEnvelope> {
        loop {
            match self.receiver.recv().await {
                Ok(envelope) => {
                    self.last_observed_sequence =
                        self.last_observed_sequence.max(envelope.sequence);
                    return Some(envelope);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    self.perf
                        .subscriber_closed_session_total
                        .fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.perf
                        .subscriber_lagged_errors_session_total
                        .fetch_add(1, Ordering::Relaxed);
                    self.perf
                        .subscriber_lagged_events_session_total
                        .fetch_add(skipped, Ordering::Relaxed);
                    self.perf
                        .queue_pressure_dropped_events_total
                        .fetch_add(skipped, Ordering::Relaxed);
                    self.perf
                        .output_truncation_markers_total
                        .fetch_add(1, Ordering::Relaxed);

                    let marker_sequence =
                        self.last_observed_sequence.saturating_add(skipped.max(1));
                    self.last_observed_sequence = marker_sequence;

                    return Some(WorkerEventEnvelope {
                        session_id: self.session_id.clone(),
                        sequence: marker_sequence,
                        received_at_monotonic_nanos: monotonic_nanos_since(self.boot_instant),
                        event: WorkerEvent::Output(WorkerOutputEvent {
                            stream: WorkerOutputStream::Stderr,
                            bytes: truncation_marker_bytes(skipped),
                        }),
                    });
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkerGlobalEventSubscription {
    receiver: broadcast::Receiver<WorkerEventEnvelope>,
    perf: Arc<WorkerEventBusPerfCounters>,
}

impl WorkerGlobalEventSubscription {
    pub async fn next_event(&mut self) -> Option<WorkerEventEnvelope> {
        loop {
            match self.receiver.recv().await {
                Ok(envelope) => return Some(envelope),
                Err(broadcast::error::RecvError::Closed) => {
                    self.perf
                        .subscriber_closed_global_total
                        .fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.perf
                        .subscriber_lagged_errors_global_total
                        .fetch_add(1, Ordering::Relaxed);
                    self.perf
                        .subscriber_lagged_events_global_total
                        .fetch_add(skipped, Ordering::Relaxed);
                    self.perf
                        .queue_pressure_dropped_events_total
                        .fetch_add(skipped, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkerEventBus {
    next_sequence: AtomicU64,
    boot_instant: Instant,
    config: WorkerEventBusConfig,
    perf: Arc<WorkerEventBusPerfCounters>,
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
            perf: Arc::new(WorkerEventBusPerfCounters::default()),
            session_senders: RwLock::new(HashMap::new()),
            global_sender,
        }
    }

    pub fn subscribe_session(&self, session_id: WorkerSessionId) -> WorkerSessionEventSubscription {
        self.perf
            .subscribe_session_calls_total
            .fetch_add(1, Ordering::Relaxed);

        let receiver = if let Some(sender) = self.session_sender(&session_id) {
            sender.subscribe()
        } else {
            let mut session_senders = self
                .session_senders
                .write()
                .expect("worker eventbus session sender lock poisoned");
            let sender = session_senders
                .entry(session_id.clone())
                .or_insert_with(|| {
                    let (sender, _receiver) =
                        broadcast::channel(self.config.session_buffer_capacity);
                    sender
                });
            sender.subscribe()
        };

        WorkerSessionEventSubscription {
            session_id,
            receiver,
            perf: Arc::clone(&self.perf),
            boot_instant: self.boot_instant,
            last_observed_sequence: 0,
        }
    }

    pub fn subscribe_all(&self) -> WorkerGlobalEventSubscription {
        self.perf
            .subscribe_global_calls_total
            .fetch_add(1, Ordering::Relaxed);

        WorkerGlobalEventSubscription {
            receiver: self.global_sender.subscribe(),
            perf: Arc::clone(&self.perf),
        }
    }

    pub fn remove_session(&self, session_id: &WorkerSessionId) -> bool {
        let mut session_senders = self
            .session_senders
            .write()
            .expect("worker eventbus session sender lock poisoned");
        session_senders.remove(session_id).is_some()
    }

    pub fn publish(&self, session_id: WorkerSessionId, event: WorkerEvent) {
        self.perf
            .events_received_total
            .fetch_add(1, Ordering::Relaxed);
        self.perf
            .events_published_total
            .fetch_add(1, Ordering::Relaxed);

        let dispatch_started = Instant::now();

        let envelope = WorkerEventEnvelope {
            session_id,
            sequence: self.next_sequence(),
            received_at_monotonic_nanos: self.monotonic_nanos_since_bus_bootstrap(),
            event,
        };

        let session_sender = self.session_sender(&envelope.session_id);
        let session_receivers = session_sender
            .as_ref()
            .map(|sender| sender.receiver_count() as u64)
            .unwrap_or(0);
        let global_receivers = self.global_sender.receiver_count() as u64;

        self.perf
            .session_receiver_count_last
            .store(session_receivers, Ordering::Relaxed);
        self.perf
            .global_receiver_count_last
            .store(global_receivers, Ordering::Relaxed);

        match (session_receivers > 0, global_receivers > 0) {
            (true, true) => {
                self.perf
                    .event_clone_ops_total
                    .fetch_add(1, Ordering::Relaxed);

                if let Some(sender) = session_sender {
                    let _ = sender.send(envelope.clone());
                    self.perf
                        .events_forwarded_session_total
                        .fetch_add(1, Ordering::Relaxed);
                }

                let _ = self.global_sender.send(envelope);
                self.perf
                    .events_forwarded_global_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            (true, false) => {
                if let Some(sender) = session_sender {
                    let _ = sender.send(envelope);
                    self.perf
                        .events_forwarded_session_total
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            (false, true) => {
                let _ = self.global_sender.send(envelope);
                self.perf
                    .events_forwarded_global_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            (false, false) => {}
        }

        self.perf.event_dispatch_nanos_total.fetch_add(
            saturating_nanos_u64(dispatch_started.elapsed()),
            Ordering::Relaxed,
        );
    }

    pub fn perf_snapshot(&self) -> WorkerEventBusPerfSnapshot {
        self.perf
            .snapshot(self.monotonic_nanos_since_bus_bootstrap())
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
        monotonic_nanos_since(self.boot_instant)
    }
}

fn truncation_marker_bytes(skipped: u64) -> Vec<u8> {
    format!("[orchestrator] output truncated: dropped {skipped} events due stream pressure\n")
        .into_bytes()
}

fn monotonic_nanos_since(instant: Instant) -> u64 {
    let nanos = instant.elapsed().as_nanos();
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

fn saturating_nanos_u64(duration: Duration) -> u64 {
    duration
        .as_nanos()
        .min(u128::from(u64::MAX))
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use orchestrator_worker_protocol::event::{WorkerEvent, WorkerOutputEvent, WorkerOutputStream};
    use orchestrator_worker_protocol::ids::WorkerSessionId;
    use tokio::time::timeout;

    use super::{WorkerEventBus, WorkerEventBusConfig};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    #[test]
    #[should_panic(expected = "worker event sequence exhausted")]
    fn publish_panics_when_sequence_space_is_exhausted() {
        let bus = WorkerEventBus::default();
        bus.next_sequence
            .store(u64::MAX, std::sync::atomic::Ordering::Relaxed);

        bus.publish(
            "sess-overflow".into(),
            WorkerEvent::Done(Default::default()),
        );
    }

    #[tokio::test]
    async fn publish_allocates_monotonic_sequence_numbers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());

        bus.publish(session_id.clone(), WorkerEvent::Done(Default::default()));
        bus.publish(session_id, WorkerEvent::Done(Default::default()));

        let first = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        let second = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");

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

        bus.publish(session_id, WorkerEvent::Done(Default::default()));

        let session_envelope = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        let global_envelope = timeout(TEST_TIMEOUT, global_subscriber.next_event())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");

        assert_eq!(session_envelope.sequence, global_envelope.sequence);
        assert_eq!(session_envelope.session_id, global_envelope.session_id);
        assert_eq!(session_envelope.event, global_envelope.event);
    }

    #[tokio::test]
    async fn session_subscriptions_only_receive_matching_session_events() {
        let bus = WorkerEventBus::default();
        let session_a = WorkerSessionId::new("sess-a");
        let session_b = WorkerSessionId::new("sess-b");
        let mut subscriber_a = bus.subscribe_session(session_a.clone());
        let mut subscriber_b = bus.subscribe_session(session_b.clone());

        bus.publish(session_a.clone(), WorkerEvent::Done(Default::default()));
        bus.publish(session_b.clone(), WorkerEvent::Done(Default::default()));

        let received_a = timeout(TEST_TIMEOUT, subscriber_a.next_event())
            .await
            .expect("session a recv timed out")
            .expect("session a recv should succeed");
        let received_b = timeout(TEST_TIMEOUT, subscriber_b.next_event())
            .await
            .expect("session b recv timed out")
            .expect("session b recv should succeed");

        assert_eq!(received_a.session_id, session_a);
        assert_eq!(received_b.session_id, session_b);
    }

    #[tokio::test]
    async fn global_subscription_receives_events_from_all_sessions() {
        let bus = WorkerEventBus::default();
        let mut global_subscriber = bus.subscribe_all();
        let session_a = WorkerSessionId::new("sess-a");
        let session_b = WorkerSessionId::new("sess-b");

        bus.publish(session_a.clone(), WorkerEvent::Done(Default::default()));
        bus.publish(session_b.clone(), WorkerEvent::Done(Default::default()));

        let received_a = timeout(TEST_TIMEOUT, global_subscriber.next_event())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");
        let received_b = timeout(TEST_TIMEOUT, global_subscriber.next_event())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");

        assert_eq!(received_a.session_id, session_a);
        assert_eq!(received_b.session_id, session_b);
    }

    #[tokio::test]
    async fn session_lag_emits_truncation_marker_and_updates_counters() {
        let bus = WorkerEventBus::new(WorkerEventBusConfig {
            session_buffer_capacity: 1,
            global_buffer_capacity: 1,
        });
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());

        for chunk in ["one", "two", "three"] {
            bus.publish(
                session_id.clone(),
                WorkerEvent::Output(WorkerOutputEvent {
                    stream: WorkerOutputStream::Stdout,
                    bytes: chunk.as_bytes().to_vec(),
                }),
            );
        }

        let marker_event = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");

        match marker_event.event {
            WorkerEvent::Output(output) => {
                assert_eq!(output.stream, WorkerOutputStream::Stderr);
                assert!(String::from_utf8_lossy(&output.bytes).contains("output truncated"));
                assert!(output.bytes.ends_with(b"\n"));
                assert!(!output.bytes.ends_with(b"\\n"));
            }
            other => panic!("expected truncation marker output event, got {other:?}"),
        }

        assert_eq!(marker_event.sequence, 2);
        let resumed_event = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        assert_eq!(resumed_event.sequence, marker_event.sequence + 1);
        match resumed_event.event {
            WorkerEvent::Output(output) => {
                assert_eq!(output.stream, WorkerOutputStream::Stdout);
                assert_eq!(output.bytes, b"three");
            }
            other => panic!("expected resumed stdout output event, got {other:?}"),
        }

        let snapshot = bus.perf_snapshot();
        assert_eq!(snapshot.subscriber_lagged_errors_session_total, 1);
        assert_eq!(snapshot.subscriber_lagged_events_session_total, 2);
        assert_eq!(snapshot.queue_pressure_dropped_events_total, 2);
        assert_eq!(snapshot.output_truncation_markers_total, 1);
    }

    #[tokio::test]
    async fn global_lag_skips_dropped_events_and_updates_counters_without_marker() {
        let bus = WorkerEventBus::new(WorkerEventBusConfig {
            session_buffer_capacity: 1,
            global_buffer_capacity: 1,
        });
        let session_id = WorkerSessionId::new("sess-a");
        let mut global_subscriber = bus.subscribe_all();

        for idx in 0..4 {
            bus.publish(
                session_id.clone(),
                WorkerEvent::Output(WorkerOutputEvent {
                    stream: WorkerOutputStream::Stdout,
                    bytes: format!("event-{idx}").into_bytes(),
                }),
            );
        }

        let received = timeout(TEST_TIMEOUT, global_subscriber.next_event())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");
        match received.event {
            WorkerEvent::Output(output) => {
                assert_eq!(output.stream, WorkerOutputStream::Stdout);
                assert!(String::from_utf8_lossy(&output.bytes).starts_with("event-"));
            }
            other => panic!("expected stdout output event, got {other:?}"),
        }

        let snapshot = bus.perf_snapshot();
        assert_eq!(snapshot.subscriber_lagged_errors_global_total, 1);
        assert_eq!(snapshot.subscriber_lagged_events_global_total, 3);
        assert_eq!(snapshot.queue_pressure_dropped_events_total, 3);
        assert_eq!(snapshot.output_truncation_markers_total, 0);
    }

    #[tokio::test]
    async fn clone_minimization_skips_clone_for_global_only_subscribers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut global_subscriber = bus.subscribe_all();

        bus.publish(session_id, WorkerEvent::Done(Default::default()));

        let _ = timeout(TEST_TIMEOUT, global_subscriber.next_event())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");

        let snapshot = bus.perf_snapshot();
        assert_eq!(snapshot.event_clone_ops_total, 0);
        assert_eq!(snapshot.events_forwarded_session_total, 0);
        assert_eq!(snapshot.events_forwarded_global_total, 1);
    }

    #[tokio::test]
    async fn clone_minimization_skips_clone_for_session_only_subscribers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());

        bus.publish(session_id, WorkerEvent::Done(Default::default()));

        let _ = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");

        let snapshot = bus.perf_snapshot();
        assert_eq!(snapshot.event_clone_ops_total, 0);
        assert_eq!(snapshot.events_forwarded_session_total, 1);
        assert_eq!(snapshot.events_forwarded_global_total, 0);
    }

    #[tokio::test]
    async fn clone_minimization_clones_once_when_both_subscribers_exist() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());
        let mut global_subscriber = bus.subscribe_all();

        bus.publish(session_id, WorkerEvent::Done(Default::default()));

        let _ = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        let _ = timeout(TEST_TIMEOUT, global_subscriber.next_event())
            .await
            .expect("global recv timed out")
            .expect("global recv should succeed");

        let snapshot = bus.perf_snapshot();
        assert_eq!(snapshot.event_clone_ops_total, 1);
        assert_eq!(snapshot.events_forwarded_session_total, 1);
        assert_eq!(snapshot.events_forwarded_global_total, 1);
    }

    #[tokio::test]
    async fn remove_session_closes_existing_session_subscribers() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let mut session_subscriber = bus.subscribe_session(session_id.clone());

        assert!(bus.remove_session(&session_id));
        assert!(!bus.remove_session(&session_id));

        let closed = timeout(TEST_TIMEOUT, session_subscriber.next_event())
            .await
            .expect("session recv timed out");
        assert!(closed.is_none());
    }

    #[tokio::test]
    async fn subscribe_session_after_remove_recreates_session_channel() {
        let bus = WorkerEventBus::default();
        let session_id = WorkerSessionId::new("sess-a");
        let _subscriber = bus.subscribe_session(session_id.clone());

        assert!(bus.remove_session(&session_id));

        let mut refreshed_subscriber = bus.subscribe_session(session_id.clone());
        bus.publish(session_id.clone(), WorkerEvent::Done(Default::default()));
        let received = timeout(TEST_TIMEOUT, refreshed_subscriber.next_event())
            .await
            .expect("session recv timed out")
            .expect("session recv should succeed");
        assert_eq!(received.session_id, session_id);
        assert!(matches!(received.event, WorkerEvent::Done(_)));
    }
}
