use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;

use crate::{
    BackendCapabilities, BackendCrashedEvent, BackendDoneEvent, BackendEvent, BackendKind,
    RuntimeError, RuntimeResult, RuntimeSessionId, SessionHandle, SpawnSpec, WorkerBackend,
    WorkerEventStream,
};

const DEFAULT_SESSION_EVENT_BUFFER: usize = 256;
const DEFAULT_GLOBAL_EVENT_BUFFER: usize = 1_024;
const DEFAULT_CHECKPOINT_PROMPT_INTERVAL_SECS: u64 = 120;
const DEFAULT_CHECKPOINT_PROMPT_MESSAGE: &str = "Emit a checkpoint now.";
const DEFAULT_PERF_METRICS_ENABLED: bool = true;

#[derive(Debug, Clone)]
pub struct WorkerManagerConfig {
    pub session_event_buffer: usize,
    pub global_event_buffer: usize,
    pub checkpoint_prompt_interval: Option<Duration>,
    pub checkpoint_prompt_message: String,
    pub perf_metrics_enabled: bool,
}

impl Default for WorkerManagerConfig {
    fn default() -> Self {
        Self {
            session_event_buffer: DEFAULT_SESSION_EVENT_BUFFER,
            global_event_buffer: DEFAULT_GLOBAL_EVENT_BUFFER,
            checkpoint_prompt_interval: Some(Duration::from_secs(
                DEFAULT_CHECKPOINT_PROMPT_INTERVAL_SECS,
            )),
            checkpoint_prompt_message: DEFAULT_CHECKPOINT_PROMPT_MESSAGE.to_owned(),
            perf_metrics_enabled: DEFAULT_PERF_METRICS_ENABLED,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionVisibility {
    Focused,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedSessionStatus {
    Starting,
    Running,
    Done,
    Crashed,
    Killed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSessionSummary {
    pub session_id: RuntimeSessionId,
    pub backend: BackendKind,
    pub status: ManagedSessionStatus,
    pub visibility: SessionVisibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerManagerEvent {
    pub session_id: RuntimeSessionId,
    pub event: BackendEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkerManagerPerfTotals {
    pub checkpoint_ticks_total: u64,
    pub checkpoint_skips_focused_total: u64,
    pub checkpoint_send_attempts_total: u64,
    pub checkpoint_send_failures_total: u64,
    pub checkpoint_loop_read_lock_wait_nanos_total: u64,
    pub checkpoint_send_nanos_total: u64,
    pub events_received_total: u64,
    pub events_forwarded_session_total: u64,
    pub events_forwarded_global_total: u64,
    pub event_clone_ops_total: u64,
    pub event_dispatch_nanos_total: u64,
    pub done_events_total: u64,
    pub crashed_events_total: u64,
    pub session_receiver_count_last: u64,
    pub global_receiver_count_last: u64,
    pub subscribe_session_calls_total: u64,
    pub subscribe_global_calls_total: u64,
    pub subscribe_session_failures_total: u64,
    pub subscriber_lagged_errors_session_total: u64,
    pub subscriber_lagged_errors_global_total: u64,
    pub subscriber_closed_session_total: u64,
    pub subscriber_closed_global_total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerManagerPerfSessionSnapshot {
    pub session_id: RuntimeSessionId,
    pub checkpoint_ticks_total: u64,
    pub checkpoint_skips_focused_total: u64,
    pub checkpoint_send_attempts_total: u64,
    pub checkpoint_send_failures_total: u64,
    pub checkpoint_loop_read_lock_wait_nanos_total: u64,
    pub checkpoint_send_nanos_total: u64,
    pub events_received_total: u64,
    pub events_forwarded_session_total: u64,
    pub events_forwarded_global_total: u64,
    pub event_clone_ops_total: u64,
    pub event_dispatch_nanos_total: u64,
    pub done_events_total: u64,
    pub crashed_events_total: u64,
    pub session_receiver_count_last: u64,
    pub global_receiver_count_last: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerManagerPerfSnapshot {
    pub metrics_enabled: bool,
    pub captured_at_monotonic_nanos: u64,
    pub session_count: usize,
    pub totals: WorkerManagerPerfTotals,
    pub sessions: Vec<WorkerManagerPerfSessionSnapshot>,
}

#[derive(Debug, Default)]
struct SessionPerfCounters {
    checkpoint_ticks_total: AtomicU64,
    checkpoint_skips_focused_total: AtomicU64,
    checkpoint_send_attempts_total: AtomicU64,
    checkpoint_send_failures_total: AtomicU64,
    checkpoint_loop_read_lock_wait_nanos_total: AtomicU64,
    checkpoint_send_nanos_total: AtomicU64,
    events_received_total: AtomicU64,
    events_forwarded_session_total: AtomicU64,
    events_forwarded_global_total: AtomicU64,
    event_clone_ops_total: AtomicU64,
    event_dispatch_nanos_total: AtomicU64,
    done_events_total: AtomicU64,
    crashed_events_total: AtomicU64,
    session_receiver_count_last: AtomicU64,
    global_receiver_count_last: AtomicU64,
}

impl SessionPerfCounters {
    fn snapshot(&self, session_id: RuntimeSessionId) -> WorkerManagerPerfSessionSnapshot {
        WorkerManagerPerfSessionSnapshot {
            session_id,
            checkpoint_ticks_total: self.checkpoint_ticks_total.load(Ordering::Relaxed),
            checkpoint_skips_focused_total: self
                .checkpoint_skips_focused_total
                .load(Ordering::Relaxed),
            checkpoint_send_attempts_total: self
                .checkpoint_send_attempts_total
                .load(Ordering::Relaxed),
            checkpoint_send_failures_total: self
                .checkpoint_send_failures_total
                .load(Ordering::Relaxed),
            checkpoint_loop_read_lock_wait_nanos_total: self
                .checkpoint_loop_read_lock_wait_nanos_total
                .load(Ordering::Relaxed),
            checkpoint_send_nanos_total: self.checkpoint_send_nanos_total.load(Ordering::Relaxed),
            events_received_total: self.events_received_total.load(Ordering::Relaxed),
            events_forwarded_session_total: self
                .events_forwarded_session_total
                .load(Ordering::Relaxed),
            events_forwarded_global_total: self
                .events_forwarded_global_total
                .load(Ordering::Relaxed),
            event_clone_ops_total: self.event_clone_ops_total.load(Ordering::Relaxed),
            event_dispatch_nanos_total: self.event_dispatch_nanos_total.load(Ordering::Relaxed),
            done_events_total: self.done_events_total.load(Ordering::Relaxed),
            crashed_events_total: self.crashed_events_total.load(Ordering::Relaxed),
            session_receiver_count_last: self.session_receiver_count_last.load(Ordering::Relaxed),
            global_receiver_count_last: self.global_receiver_count_last.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
struct WorkerManagerPerfCounters {
    enabled: bool,
    started_at: std::time::Instant,
    totals: SessionPerfCounters,
    subscribe_session_calls_total: AtomicU64,
    subscribe_global_calls_total: AtomicU64,
    subscribe_session_failures_total: AtomicU64,
    subscriber_lagged_errors_session_total: AtomicU64,
    subscriber_lagged_errors_global_total: AtomicU64,
    subscriber_closed_session_total: AtomicU64,
    subscriber_closed_global_total: AtomicU64,
}

impl WorkerManagerPerfCounters {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started_at: std::time::Instant::now(),
            totals: SessionPerfCounters::default(),
            subscribe_session_calls_total: AtomicU64::new(0),
            subscribe_global_calls_total: AtomicU64::new(0),
            subscribe_session_failures_total: AtomicU64::new(0),
            subscriber_lagged_errors_session_total: AtomicU64::new(0),
            subscriber_lagged_errors_global_total: AtomicU64::new(0),
            subscriber_closed_session_total: AtomicU64::new(0),
            subscriber_closed_global_total: AtomicU64::new(0),
        }
    }

    fn maybe_add(counter: &AtomicU64, enabled: bool, amount: u64) {
        if enabled && amount != 0 {
            counter.fetch_add(amount, Ordering::Relaxed);
        }
    }

    fn maybe_inc(counter: &AtomicU64, enabled: bool) {
        if enabled {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn new_session(&self) -> Arc<SessionPerfCounters> {
        Arc::new(SessionPerfCounters::default())
    }

    fn capture_totals(&self) -> WorkerManagerPerfTotals {
        WorkerManagerPerfTotals {
            checkpoint_ticks_total: self.totals.checkpoint_ticks_total.load(Ordering::Relaxed),
            checkpoint_skips_focused_total: self
                .totals
                .checkpoint_skips_focused_total
                .load(Ordering::Relaxed),
            checkpoint_send_attempts_total: self
                .totals
                .checkpoint_send_attempts_total
                .load(Ordering::Relaxed),
            checkpoint_send_failures_total: self
                .totals
                .checkpoint_send_failures_total
                .load(Ordering::Relaxed),
            checkpoint_loop_read_lock_wait_nanos_total: self
                .totals
                .checkpoint_loop_read_lock_wait_nanos_total
                .load(Ordering::Relaxed),
            checkpoint_send_nanos_total: self
                .totals
                .checkpoint_send_nanos_total
                .load(Ordering::Relaxed),
            events_received_total: self.totals.events_received_total.load(Ordering::Relaxed),
            events_forwarded_session_total: self
                .totals
                .events_forwarded_session_total
                .load(Ordering::Relaxed),
            events_forwarded_global_total: self
                .totals
                .events_forwarded_global_total
                .load(Ordering::Relaxed),
            event_clone_ops_total: self.totals.event_clone_ops_total.load(Ordering::Relaxed),
            event_dispatch_nanos_total: self
                .totals
                .event_dispatch_nanos_total
                .load(Ordering::Relaxed),
            done_events_total: self.totals.done_events_total.load(Ordering::Relaxed),
            crashed_events_total: self.totals.crashed_events_total.load(Ordering::Relaxed),
            session_receiver_count_last: self
                .totals
                .session_receiver_count_last
                .load(Ordering::Relaxed),
            global_receiver_count_last: self
                .totals
                .global_receiver_count_last
                .load(Ordering::Relaxed),
            subscribe_session_calls_total: self
                .subscribe_session_calls_total
                .load(Ordering::Relaxed),
            subscribe_global_calls_total: self.subscribe_global_calls_total.load(Ordering::Relaxed),
            subscribe_session_failures_total: self
                .subscribe_session_failures_total
                .load(Ordering::Relaxed),
            subscriber_lagged_errors_session_total: self
                .subscriber_lagged_errors_session_total
                .load(Ordering::Relaxed),
            subscriber_lagged_errors_global_total: self
                .subscriber_lagged_errors_global_total
                .load(Ordering::Relaxed),
            subscriber_closed_session_total: self
                .subscriber_closed_session_total
                .load(Ordering::Relaxed),
            subscriber_closed_global_total: self
                .subscriber_closed_global_total
                .load(Ordering::Relaxed),
        }
    }
}

pub struct SessionEventSubscription {
    receiver: broadcast::Receiver<BackendEvent>,
    perf: Arc<WorkerManagerPerfCounters>,
}

pub struct WorkerManagerEventSubscription {
    receiver: broadcast::Receiver<WorkerManagerEvent>,
    perf: Arc<WorkerManagerPerfCounters>,
}

struct ManagedSession {
    handle: SessionHandle,
    status: ManagedSessionStatus,
    visibility: SessionVisibility,
    perf: Arc<SessionPerfCounters>,
    event_tx: Option<broadcast::Sender<BackendEvent>>,
    event_task: Option<JoinHandle<()>>,
    checkpoint_task: Option<JoinHandle<()>>,
}

enum SessionEntry {
    Starting,
    Managed(ManagedSession),
}

#[derive(Clone)]
pub struct WorkerManager {
    backend: Arc<dyn WorkerBackend>,
    sessions: Arc<RwLock<HashMap<RuntimeSessionId, SessionEntry>>>,
    global_event_tx: broadcast::Sender<WorkerManagerEvent>,
    config: WorkerManagerConfig,
    perf: Arc<WorkerManagerPerfCounters>,
}

impl WorkerManager {
    pub fn new(backend: Arc<dyn WorkerBackend>) -> Self {
        Self::with_config(backend, WorkerManagerConfig::default())
    }

    pub fn with_config(backend: Arc<dyn WorkerBackend>, config: WorkerManagerConfig) -> Self {
        let perf = Arc::new(WorkerManagerPerfCounters::new(config.perf_metrics_enabled));
        let (global_event_tx, _) = broadcast::channel(config.global_event_buffer.max(1));
        Self {
            backend,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            global_event_tx,
            config,
            perf,
        }
    }

    pub fn backend_kind(&self) -> BackendKind {
        self.backend.kind()
    }

    pub fn backend_capabilities(&self) -> BackendCapabilities {
        self.backend.capabilities()
    }

    pub async fn perf_snapshot(&self) -> WorkerManagerPerfSnapshot {
        let sessions = self.sessions.read().await;
        let mut session_snapshots = sessions
            .iter()
            .filter_map(|(session_id, entry)| match entry {
                SessionEntry::Managed(session) => Some(session.perf.snapshot(session_id.clone())),
                SessionEntry::Starting => None,
            })
            .collect::<Vec<_>>();
        session_snapshots
            .sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));

        WorkerManagerPerfSnapshot {
            metrics_enabled: self.perf.enabled,
            captured_at_monotonic_nanos: saturating_nanos_u64(self.perf.started_at.elapsed()),
            session_count: sessions.len(),
            totals: self.perf.capture_totals(),
            sessions: session_snapshots,
        }
    }

    pub async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
        let session_id = spec.session_id.clone();
        self.reserve_session(&session_id).await?;

        let handle = match self.backend.spawn(spec).await {
            Ok(handle) => handle,
            Err(error) => {
                self.remove_session_entry(&session_id).await;
                return Err(error);
            }
        };

        if handle.session_id != session_id {
            let _ = self.backend.kill(&handle).await;
            let _ = self
                .backend
                .kill(&SessionHandle {
                    session_id: session_id.clone(),
                    backend: handle.backend.clone(),
                })
                .await;
            self.remove_session_entry(&session_id).await;
            return Err(RuntimeError::Internal(format!(
                "backend returned mismatched session id: expected {}, got {}",
                session_id.as_str(),
                handle.session_id.as_str()
            )));
        }

        let stream = match self.backend.subscribe(&handle).await {
            Ok(stream) => stream,
            Err(error) => {
                let _ = self.backend.kill(&handle).await;
                self.remove_session_entry(&session_id).await;
                return Err(error);
            }
        };

        let (event_tx, _) = broadcast::channel(self.config.session_event_buffer.max(1));
        let session_perf = self.perf.new_session();
        if let Err(error) = self
            .install_session(
                session_id.clone(),
                ManagedSession {
                    handle: handle.clone(),
                    status: ManagedSessionStatus::Running,
                    visibility: SessionVisibility::Background,
                    perf: Arc::clone(&session_perf),
                    event_tx: Some(event_tx.clone()),
                    event_task: None,
                    checkpoint_task: None,
                },
            )
            .await
        {
            let _ = self.backend.kill(&handle).await;
            self.remove_session_entry(&session_id).await;
            return Err(error);
        }

        let event_task = Self::spawn_event_task(
            Arc::clone(&self.sessions),
            session_id.clone(),
            stream,
            event_tx,
            self.global_event_tx.clone(),
            Arc::clone(&self.perf),
            Arc::clone(&session_perf),
        );
        let checkpoint_task = self.periodic_checkpoint_prompt().map(|(interval, prompt)| {
            Self::spawn_periodic_checkpoint_task(
                Arc::clone(&self.backend),
                Arc::clone(&self.sessions),
                handle.clone(),
                interval,
                prompt,
                Arc::clone(&self.perf),
                Arc::clone(&session_perf),
            )
        });

        let mut sessions = self.sessions.write().await;
        match sessions.get_mut(&session_id) {
            Some(SessionEntry::Managed(session)) => {
                session.event_task = Some(event_task);
                session.checkpoint_task = checkpoint_task;
            }
            _ => {
                event_task.abort();
                if let Some(task) = checkpoint_task {
                    task.abort();
                }
                return Err(RuntimeError::Internal(format!(
                    "session {} vanished while attaching event task",
                    session_id.as_str()
                )));
            }
        }

        Ok(handle)
    }

    pub async fn kill(&self, session_id: &RuntimeSessionId) -> RuntimeResult<()> {
        let handle = self.running_session_handle(session_id).await?;
        self.backend.kill(&handle).await?;

        let mut sessions = self.sessions.write().await;
        let Some(SessionEntry::Managed(session)) = sessions.get_mut(session_id) else {
            return Err(RuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            ));
        };

        if let Some(task) = session.event_task.take() {
            task.abort();
        }
        if let Some(task) = session.checkpoint_task.take() {
            task.abort();
        }
        session.event_tx = None;
        session.status = ManagedSessionStatus::Killed;
        Ok(())
    }

    pub async fn send_input(
        &self,
        session_id: &RuntimeSessionId,
        input: &[u8],
    ) -> RuntimeResult<()> {
        let handle = self.running_session_handle(session_id).await?;
        self.backend.send_input(&handle, input).await
    }

    pub async fn prompt_checkpoint(&self, session_id: &RuntimeSessionId) -> RuntimeResult<()> {
        let prompt = self.checkpoint_prompt_bytes().ok_or_else(|| {
            RuntimeError::Configuration(
                "checkpoint prompt message is empty; cannot prompt session".to_owned(),
            )
        })?;
        self.send_input(session_id, &prompt).await
    }

    pub async fn resize(
        &self,
        session_id: &RuntimeSessionId,
        cols: u16,
        rows: u16,
    ) -> RuntimeResult<()> {
        let handle = self.running_session_handle(session_id).await?;
        self.backend.resize(&handle, cols, rows).await
    }

    pub async fn subscribe(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<SessionEventSubscription> {
        WorkerManagerPerfCounters::maybe_inc(
            &self.perf.subscribe_session_calls_total,
            self.perf.enabled,
        );
        let sessions = self.sessions.read().await;
        match sessions.get(session_id) {
            Some(SessionEntry::Starting) => {
                WorkerManagerPerfCounters::maybe_inc(
                    &self.perf.subscribe_session_failures_total,
                    self.perf.enabled,
                );
                Err(RuntimeError::Process(format!(
                    "worker session is still starting: {}",
                    session_id.as_str()
                )))
            }
            Some(SessionEntry::Managed(session)) => {
                let event_tx = session.event_tx.as_ref().ok_or_else(|| {
                    WorkerManagerPerfCounters::maybe_inc(
                        &self.perf.subscribe_session_failures_total,
                        self.perf.enabled,
                    );
                    RuntimeError::Process(format!(
                        "worker session output stream is closed: {}",
                        session_id.as_str()
                    ))
                })?;
                Ok(SessionEventSubscription {
                    receiver: event_tx.subscribe(),
                    perf: Arc::clone(&self.perf),
                })
            }
            None => {
                WorkerManagerPerfCounters::maybe_inc(
                    &self.perf.subscribe_session_failures_total,
                    self.perf.enabled,
                );
                Err(RuntimeError::SessionNotFound(
                    session_id.as_str().to_owned(),
                ))
            }
        }
    }

    pub fn subscribe_all(&self) -> WorkerManagerEventSubscription {
        WorkerManagerPerfCounters::maybe_inc(
            &self.perf.subscribe_global_calls_total,
            self.perf.enabled,
        );
        WorkerManagerEventSubscription {
            receiver: self.global_event_tx.subscribe(),
            perf: Arc::clone(&self.perf),
        }
    }

    pub async fn session_status(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<ManagedSessionStatus> {
        let sessions = self.sessions.read().await;
        match sessions.get(session_id) {
            Some(SessionEntry::Starting) => Ok(ManagedSessionStatus::Starting),
            Some(SessionEntry::Managed(session)) => Ok(session.status),
            None => Err(RuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            )),
        }
    }

    pub async fn set_session_visibility(
        &self,
        session_id: &RuntimeSessionId,
        visibility: SessionVisibility,
    ) -> RuntimeResult<()> {
        let mut sessions = self.sessions.write().await;
        match sessions.get_mut(session_id) {
            Some(SessionEntry::Managed(session)) => {
                session.visibility = visibility;
                Ok(())
            }
            Some(SessionEntry::Starting) => Err(RuntimeError::Process(format!(
                "worker session is still starting: {}",
                session_id.as_str()
            ))),
            None => Err(RuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            )),
        }
    }

    pub async fn focus_session(&self, focused: Option<&RuntimeSessionId>) -> RuntimeResult<()> {
        let mut sessions = self.sessions.write().await;

        if let Some(target) = focused {
            match sessions.get(target) {
                Some(SessionEntry::Managed(_)) => {}
                Some(SessionEntry::Starting) => {
                    return Err(RuntimeError::Process(format!(
                        "worker session is still starting: {}",
                        target.as_str()
                    )))
                }
                None => return Err(RuntimeError::SessionNotFound(target.as_str().to_owned())),
            }
        }

        for session in sessions.values_mut() {
            if let SessionEntry::Managed(session) = session {
                session.visibility = SessionVisibility::Background;
            }
        }

        if let Some(target) = focused {
            if let Some(SessionEntry::Managed(session)) = sessions.get_mut(target) {
                session.visibility = SessionVisibility::Focused;
            }
        }

        Ok(())
    }

    pub async fn list_sessions(&self) -> Vec<ManagedSessionSummary> {
        let sessions = self.sessions.read().await;
        let backend = self.backend.kind();

        let mut list = sessions
            .iter()
            .map(|(session_id, entry)| match entry {
                SessionEntry::Starting => ManagedSessionSummary {
                    session_id: session_id.clone(),
                    backend: backend.clone(),
                    status: ManagedSessionStatus::Starting,
                    visibility: SessionVisibility::Background,
                },
                SessionEntry::Managed(session) => ManagedSessionSummary {
                    session_id: session_id.clone(),
                    backend: session.handle.backend.clone(),
                    status: session.status,
                    visibility: session.visibility,
                },
            })
            .collect::<Vec<_>>();
        list.sort_by(|left, right| left.session_id.as_str().cmp(right.session_id.as_str()));
        list
    }

    fn spawn_event_task(
        sessions: Arc<RwLock<HashMap<RuntimeSessionId, SessionEntry>>>,
        session_id: RuntimeSessionId,
        mut stream: WorkerEventStream,
        event_tx: broadcast::Sender<BackendEvent>,
        global_event_tx: broadcast::Sender<WorkerManagerEvent>,
        manager_perf: Arc<WorkerManagerPerfCounters>,
        session_perf: Arc<SessionPerfCounters>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(Some(event)) => {
                        WorkerManagerPerfCounters::maybe_inc(
                            &session_perf.events_received_total,
                            manager_perf.enabled,
                        );
                        WorkerManagerPerfCounters::maybe_inc(
                            &manager_perf.totals.events_received_total,
                            manager_perf.enabled,
                        );

                        let dispatch_started = std::time::Instant::now();
                        let terminal_status = match &event {
                            BackendEvent::Done(_) => Some(ManagedSessionStatus::Done),
                            BackendEvent::Crashed(_) => Some(ManagedSessionStatus::Crashed),
                            _ => None,
                        };

                        let session_receivers = event_tx.receiver_count() as u64;
                        let global_receivers = global_event_tx.receiver_count() as u64;
                        session_perf
                            .session_receiver_count_last
                            .store(session_receivers, Ordering::Relaxed);
                        session_perf
                            .global_receiver_count_last
                            .store(global_receivers, Ordering::Relaxed);
                        manager_perf
                            .totals
                            .session_receiver_count_last
                            .store(session_receivers, Ordering::Relaxed);
                        manager_perf
                            .totals
                            .global_receiver_count_last
                            .store(global_receivers, Ordering::Relaxed);

                        WorkerManagerPerfCounters::maybe_inc(
                            &session_perf.event_clone_ops_total,
                            manager_perf.enabled,
                        );
                        WorkerManagerPerfCounters::maybe_inc(
                            &manager_perf.totals.event_clone_ops_total,
                            manager_perf.enabled,
                        );
                        let _ = event_tx.send(event.clone());
                        WorkerManagerPerfCounters::maybe_inc(
                            &session_perf.events_forwarded_session_total,
                            manager_perf.enabled,
                        );
                        WorkerManagerPerfCounters::maybe_inc(
                            &manager_perf.totals.events_forwarded_session_total,
                            manager_perf.enabled,
                        );
                        let _ = global_event_tx.send(WorkerManagerEvent {
                            session_id: session_id.clone(),
                            event,
                        });
                        WorkerManagerPerfCounters::maybe_inc(
                            &session_perf.events_forwarded_global_total,
                            manager_perf.enabled,
                        );
                        WorkerManagerPerfCounters::maybe_inc(
                            &manager_perf.totals.events_forwarded_global_total,
                            manager_perf.enabled,
                        );

                        let dispatch_nanos = saturating_nanos_u64(dispatch_started.elapsed());
                        WorkerManagerPerfCounters::maybe_add(
                            &session_perf.event_dispatch_nanos_total,
                            manager_perf.enabled,
                            dispatch_nanos,
                        );
                        WorkerManagerPerfCounters::maybe_add(
                            &manager_perf.totals.event_dispatch_nanos_total,
                            manager_perf.enabled,
                            dispatch_nanos,
                        );

                        if let Some(status) = terminal_status {
                            match status {
                                ManagedSessionStatus::Done => {
                                    WorkerManagerPerfCounters::maybe_inc(
                                        &session_perf.done_events_total,
                                        manager_perf.enabled,
                                    );
                                    WorkerManagerPerfCounters::maybe_inc(
                                        &manager_perf.totals.done_events_total,
                                        manager_perf.enabled,
                                    );
                                }
                                ManagedSessionStatus::Crashed => {
                                    WorkerManagerPerfCounters::maybe_inc(
                                        &session_perf.crashed_events_total,
                                        manager_perf.enabled,
                                    );
                                    WorkerManagerPerfCounters::maybe_inc(
                                        &manager_perf.totals.crashed_events_total,
                                        manager_perf.enabled,
                                    );
                                }
                                _ => {}
                            }
                            Self::mark_session_finished(&sessions, &session_id, status).await;
                            break;
                        }
                    }
                    Ok(None) => {
                        let done_event = BackendEvent::Done(BackendDoneEvent { summary: None });
                        let _ = event_tx.send(done_event.clone());
                        let _ = global_event_tx.send(WorkerManagerEvent {
                            session_id: session_id.clone(),
                            event: done_event,
                        });
                        WorkerManagerPerfCounters::maybe_inc(
                            &session_perf.done_events_total,
                            manager_perf.enabled,
                        );
                        WorkerManagerPerfCounters::maybe_inc(
                            &manager_perf.totals.done_events_total,
                            manager_perf.enabled,
                        );
                        Self::mark_session_finished(
                            &sessions,
                            &session_id,
                            ManagedSessionStatus::Done,
                        )
                        .await;
                        break;
                    }
                    Err(error) => {
                        let crashed_event = BackendEvent::Crashed(BackendCrashedEvent {
                            reason: format!("worker output stream failed: {error}"),
                        });
                        let _ = event_tx.send(crashed_event.clone());
                        let _ = global_event_tx.send(WorkerManagerEvent {
                            session_id: session_id.clone(),
                            event: crashed_event,
                        });
                        WorkerManagerPerfCounters::maybe_inc(
                            &session_perf.crashed_events_total,
                            manager_perf.enabled,
                        );
                        WorkerManagerPerfCounters::maybe_inc(
                            &manager_perf.totals.crashed_events_total,
                            manager_perf.enabled,
                        );
                        Self::mark_session_finished(
                            &sessions,
                            &session_id,
                            ManagedSessionStatus::Crashed,
                        )
                        .await;
                        break;
                    }
                }
            }
        })
    }

    fn checkpoint_prompt_bytes(&self) -> Option<Vec<u8>> {
        let prompt = self.config.checkpoint_prompt_message.trim();
        if prompt.is_empty() {
            return None;
        }

        let mut bytes = prompt.as_bytes().to_vec();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        Some(bytes)
    }

    fn periodic_checkpoint_prompt(&self) -> Option<(Duration, Vec<u8>)> {
        let interval = self.config.checkpoint_prompt_interval?;
        if interval.is_zero() {
            return None;
        }
        let prompt = self.checkpoint_prompt_bytes()?;
        Some((interval, prompt))
    }

    fn spawn_periodic_checkpoint_task(
        backend: Arc<dyn WorkerBackend>,
        sessions: Arc<RwLock<HashMap<RuntimeSessionId, SessionEntry>>>,
        handle: SessionHandle,
        interval: Duration,
        prompt: Vec<u8>,
        manager_perf: Arc<WorkerManagerPerfCounters>,
        session_perf: Arc<SessionPerfCounters>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;

            loop {
                ticker.tick().await;
                WorkerManagerPerfCounters::maybe_inc(
                    &session_perf.checkpoint_ticks_total,
                    manager_perf.enabled,
                );
                WorkerManagerPerfCounters::maybe_inc(
                    &manager_perf.totals.checkpoint_ticks_total,
                    manager_perf.enabled,
                );

                let visibility = {
                    let wait_started = std::time::Instant::now();
                    let sessions = sessions.read().await;
                    let wait_nanos = saturating_nanos_u64(wait_started.elapsed());
                    WorkerManagerPerfCounters::maybe_add(
                        &session_perf.checkpoint_loop_read_lock_wait_nanos_total,
                        manager_perf.enabled,
                        wait_nanos,
                    );
                    WorkerManagerPerfCounters::maybe_add(
                        &manager_perf
                            .totals
                            .checkpoint_loop_read_lock_wait_nanos_total,
                        manager_perf.enabled,
                        wait_nanos,
                    );
                    match sessions.get(&handle.session_id) {
                        Some(SessionEntry::Managed(session))
                            if session.status == ManagedSessionStatus::Running =>
                        {
                            Some(session.visibility)
                        }
                        _ => None,
                    }
                };
                let Some(visibility) = visibility else {
                    break;
                };
                if visibility == SessionVisibility::Focused {
                    WorkerManagerPerfCounters::maybe_inc(
                        &session_perf.checkpoint_skips_focused_total,
                        manager_perf.enabled,
                    );
                    WorkerManagerPerfCounters::maybe_inc(
                        &manager_perf.totals.checkpoint_skips_focused_total,
                        manager_perf.enabled,
                    );
                    continue;
                }

                WorkerManagerPerfCounters::maybe_inc(
                    &session_perf.checkpoint_send_attempts_total,
                    manager_perf.enabled,
                );
                WorkerManagerPerfCounters::maybe_inc(
                    &manager_perf.totals.checkpoint_send_attempts_total,
                    manager_perf.enabled,
                );
                let send_started = std::time::Instant::now();
                let send_result = backend.send_input(&handle, &prompt).await;
                let send_nanos = saturating_nanos_u64(send_started.elapsed());
                WorkerManagerPerfCounters::maybe_add(
                    &session_perf.checkpoint_send_nanos_total,
                    manager_perf.enabled,
                    send_nanos,
                );
                WorkerManagerPerfCounters::maybe_add(
                    &manager_perf.totals.checkpoint_send_nanos_total,
                    manager_perf.enabled,
                    send_nanos,
                );

                if send_result.is_err() {
                    WorkerManagerPerfCounters::maybe_inc(
                        &session_perf.checkpoint_send_failures_total,
                        manager_perf.enabled,
                    );
                    WorkerManagerPerfCounters::maybe_inc(
                        &manager_perf.totals.checkpoint_send_failures_total,
                        manager_perf.enabled,
                    );
                    break;
                }
            }
        })
    }

    async fn mark_session_finished(
        sessions: &Arc<RwLock<HashMap<RuntimeSessionId, SessionEntry>>>,
        session_id: &RuntimeSessionId,
        terminal_status: ManagedSessionStatus,
    ) {
        let mut sessions = sessions.write().await;
        if let Some(SessionEntry::Managed(session)) = sessions.get_mut(session_id) {
            if session.status != ManagedSessionStatus::Killed {
                session.status = terminal_status;
            }
            session.event_tx = None;
            if let Some(task) = session.checkpoint_task.take() {
                task.abort();
            }
            session.event_task = None;
        }
    }

    async fn running_session_handle(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<SessionHandle> {
        let sessions = self.sessions.read().await;
        match sessions.get(session_id) {
            Some(SessionEntry::Starting) => Err(RuntimeError::Process(format!(
                "worker session is still starting: {}",
                session_id.as_str()
            ))),
            Some(SessionEntry::Managed(session))
                if session.status == ManagedSessionStatus::Running =>
            {
                Ok(session.handle.clone())
            }
            Some(SessionEntry::Managed(session)) => Err(RuntimeError::Process(format!(
                "worker session is not running: {} ({:?})",
                session_id.as_str(),
                session.status
            ))),
            None => Err(RuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            )),
        }
    }

    async fn reserve_session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<()> {
        let mut sessions = self.sessions.write().await;
        if sessions.contains_key(session_id) {
            return Err(RuntimeError::Configuration(format!(
                "worker session already exists: {}",
                session_id.as_str()
            )));
        }
        sessions.insert(session_id.clone(), SessionEntry::Starting);
        Ok(())
    }

    async fn install_session(
        &self,
        session_id: RuntimeSessionId,
        session: ManagedSession,
    ) -> RuntimeResult<()> {
        let mut sessions = self.sessions.write().await;
        match sessions.get_mut(&session_id) {
            Some(entry @ SessionEntry::Starting) => {
                *entry = SessionEntry::Managed(session);
                Ok(())
            }
            Some(SessionEntry::Managed(_)) => Err(RuntimeError::Configuration(format!(
                "worker session already exists: {}",
                session_id.as_str()
            ))),
            None => Err(RuntimeError::Internal(format!(
                "worker session reservation missing during install: {}",
                session_id.as_str()
            ))),
        }
    }

    async fn remove_session_entry(&self, session_id: &RuntimeSessionId) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(session_id);
    }
}

impl SessionEventSubscription {
    pub async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
        match self.receiver.recv().await {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::RecvError::Closed) => {
                WorkerManagerPerfCounters::maybe_inc(
                    &self.perf.subscriber_closed_session_total,
                    self.perf.enabled,
                );
                Ok(None)
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                WorkerManagerPerfCounters::maybe_inc(
                    &self.perf.subscriber_lagged_errors_session_total,
                    self.perf.enabled,
                );
                Err(RuntimeError::Process(format!(
                    "worker session subscriber lagged; dropped {skipped} events"
                )))
            }
        }
    }
}

impl WorkerManagerEventSubscription {
    pub async fn next_event(&mut self) -> RuntimeResult<Option<WorkerManagerEvent>> {
        match self.receiver.recv().await {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::RecvError::Closed) => {
                WorkerManagerPerfCounters::maybe_inc(
                    &self.perf.subscriber_closed_global_total,
                    self.perf.enabled,
                );
                Ok(None)
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                WorkerManagerPerfCounters::maybe_inc(
                    &self.perf.subscriber_lagged_errors_global_total,
                    self.perf.enabled,
                );
                Err(RuntimeError::Process(format!(
                    "worker manager subscriber lagged; dropped {skipped} events"
                )))
            }
        }
    }
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
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::{
        BackendBlockedEvent, BackendCheckpointEvent, BackendDoneEvent, BackendOutputEvent,
        BackendOutputStream, SessionLifecycle, WorkerEventSubscription,
    };

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    type StreamMessage = RuntimeResult<Option<BackendEvent>>;

    #[derive(Default)]
    struct MockBackend {
        state: Mutex<MockBackendState>,
    }

    #[derive(Default)]
    struct MockBackendState {
        sessions: HashMap<RuntimeSessionId, MockSession>,
        next_spawn_handle_session_id: Option<RuntimeSessionId>,
    }

    struct MockSession {
        event_tx: Option<mpsc::UnboundedSender<StreamMessage>>,
        event_rx: Option<mpsc::UnboundedReceiver<StreamMessage>>,
        resize_calls: Vec<(u16, u16)>,
        sent_input: Vec<Vec<u8>>,
        killed: bool,
    }

    struct MockEventStream {
        receiver: mpsc::UnboundedReceiver<StreamMessage>,
    }

    impl MockBackend {
        fn set_next_spawn_handle_session_id(&self, session_id: RuntimeSessionId) {
            let mut state = self.state.lock().expect("lock backend state");
            state.next_spawn_handle_session_id = Some(session_id);
        }

        fn emit_event(&self, session_id: &RuntimeSessionId, event: BackendEvent) {
            let sender = {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .event_tx
                    .as_ref()
                    .expect("event stream is open")
                    .clone()
            };
            sender
                .send(Ok(Some(event)))
                .expect("send stream event to worker manager");
        }

        fn emit_error(&self, session_id: &RuntimeSessionId, error: RuntimeError) {
            let sender = {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .event_tx
                    .as_ref()
                    .expect("event stream is open")
                    .clone()
            };
            sender
                .send(Err(error))
                .expect("send stream error to worker manager");
        }

        fn close_stream(&self, session_id: &RuntimeSessionId) {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(session_id).expect("session exists");
            session.event_tx = None;
        }

        fn resize_calls(&self, session_id: &RuntimeSessionId) -> Vec<(u16, u16)> {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .resize_calls
                .clone()
        }

        fn sent_input(&self, session_id: &RuntimeSessionId) -> Vec<Vec<u8>> {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .sent_input
                .clone()
        }

        fn is_killed(&self, session_id: &RuntimeSessionId) -> bool {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .killed
        }
    }

    #[async_trait]
    impl SessionLifecycle for MockBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            let mut state = self.state.lock().expect("lock backend state");
            if state.sessions.contains_key(&spec.session_id) {
                return Err(RuntimeError::Configuration(format!(
                    "duplicate mock session id: {}",
                    spec.session_id.as_str()
                )));
            }

            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let spawned_session_id = spec.session_id.clone();
            state.sessions.insert(
                spawned_session_id.clone(),
                MockSession {
                    event_tx: Some(event_tx),
                    event_rx: Some(event_rx),
                    resize_calls: Vec::new(),
                    sent_input: Vec::new(),
                    killed: false,
                },
            );
            let handle_session_id = state
                .next_spawn_handle_session_id
                .take()
                .unwrap_or(spawned_session_id);

            Ok(SessionHandle {
                session_id: handle_session_id,
                backend: BackendKind::OpenCode,
            })
        }

        async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.killed = true;
            Ok(())
        }

        async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.sent_input.push(input.to_vec());
            Ok(())
        }

        async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.resize_calls.push((cols, rows));
            Ok(())
        }
    }

    #[async_trait]
    impl WorkerBackend for MockBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::OpenCode
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                structured_events: true,
                session_export: false,
                diff_provider: false,
                supports_background: true,
            }
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            Ok(())
        }

        async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            let receiver = session.event_rx.take().ok_or_else(|| {
                RuntimeError::Process("mock backend allows only one subscription".to_owned())
            })?;

            Ok(Box::new(MockEventStream { receiver }))
        }
    }

    #[async_trait]
    impl WorkerEventSubscription for MockEventStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            match self.receiver.recv().await {
                Some(event) => event,
                None => Ok(None),
            }
        }
    }

    fn spawn_spec(session_id: &str) -> SpawnSpec {
        SpawnSpec {
            session_id: RuntimeSessionId::new(session_id),
            workdir: std::env::current_dir().expect("resolve workdir"),
            model: Some("test-model".to_owned()),
            instruction_prelude: None,
            environment: Vec::new(),
        }
    }

    async fn next_session_event(subscription: &mut SessionEventSubscription) -> BackendEvent {
        timeout(TEST_TIMEOUT, subscription.next_event())
            .await
            .expect("session event timeout")
            .expect("session event result")
            .expect("session stream should produce event")
    }

    async fn next_global_event(
        subscription: &mut WorkerManagerEventSubscription,
    ) -> WorkerManagerEvent {
        timeout(TEST_TIMEOUT, subscription.next_event())
            .await
            .expect("global event timeout")
            .expect("global event result")
            .expect("global stream should produce event")
    }

    async fn wait_for_status(
        manager: &WorkerManager,
        session_id: &RuntimeSessionId,
        expected: ManagedSessionStatus,
    ) {
        timeout(TEST_TIMEOUT, async {
            loop {
                let status = manager
                    .session_status(session_id)
                    .await
                    .expect("session status lookup");
                if status == expected {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("status transition timeout");
    }

    async fn wait_for_sent_input_count(
        backend: &MockBackend,
        session_id: &RuntimeSessionId,
        minimum: usize,
    ) {
        timeout(TEST_TIMEOUT, async {
            loop {
                if backend.sent_input(session_id).len() >= minimum {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("input count timeout");
    }

    async fn assert_sent_input_count_stable(
        backend: &MockBackend,
        session_id: &RuntimeSessionId,
        expected_count: usize,
        stable_for: Duration,
    ) {
        timeout(TEST_TIMEOUT, async {
            let started = std::time::Instant::now();
            loop {
                assert_eq!(
                    backend.sent_input(session_id).len(),
                    expected_count,
                    "unexpected checkpoint prompt count while asserting stable window",
                );
                if started.elapsed() >= stable_for {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stable checkpoint prompt window timeout");
    }

    async fn kill_all_sessions(manager: &WorkerManager, session_ids: &[RuntimeSessionId]) {
        for session_id in session_ids {
            let _ = manager.kill(session_id).await;
        }
    }

    fn print_perf_metrics_line(
        scenario: &str,
        session_count: usize,
        elapsed: Duration,
        snapshot: &WorkerManagerPerfSnapshot,
    ) {
        let metrics = json!({
            "scenario": scenario,
            "session_count": session_count,
            "elapsed_ms": elapsed.as_millis(),
            "session_count_snapshot": snapshot.session_count,
            "checkpoint_ticks_total": snapshot.totals.checkpoint_ticks_total,
            "checkpoint_send_attempts_total": snapshot.totals.checkpoint_send_attempts_total,
            "checkpoint_send_failures_total": snapshot.totals.checkpoint_send_failures_total,
            "events_received_total": snapshot.totals.events_received_total,
            "events_forwarded_session_total": snapshot.totals.events_forwarded_session_total,
            "events_forwarded_global_total": snapshot.totals.events_forwarded_global_total,
            "event_dispatch_nanos_total": snapshot.totals.event_dispatch_nanos_total,
            "subscribe_session_calls_total": snapshot.totals.subscribe_session_calls_total,
            "subscribe_global_calls_total": snapshot.totals.subscribe_global_calls_total,
            "subscriber_lagged_errors_session_total": snapshot.totals.subscriber_lagged_errors_session_total,
            "subscriber_lagged_errors_global_total": snapshot.totals.subscriber_lagged_errors_global_total,
            "checkpoint_ticks_per_session": if session_count > 0 {
                snapshot.totals.checkpoint_ticks_total / session_count as u64
            } else {
                0
            },
        });
        println!("{metrics}");
    }

    fn make_manager(backend: Arc<MockBackend>, config: WorkerManagerConfig) -> WorkerManager {
        let backend_dyn: Arc<dyn WorkerBackend> = backend;
        WorkerManager::with_config(backend_dyn, config)
    }

    #[tokio::test]
    async fn worker_manager_forwards_lifecycle_input_resize_and_kill() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let spec = spawn_spec("wm-lifecycle");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn managed session");
        assert_eq!(
            manager
                .session_status(&session_id)
                .await
                .expect("running status"),
            ManagedSessionStatus::Running
        );

        manager
            .send_input(&session_id, b"hello\n")
            .await
            .expect("forward session input");
        manager
            .resize(&session_id, 120, 40)
            .await
            .expect("forward session resize");

        assert_eq!(backend.sent_input(&session_id), vec![b"hello\n".to_vec()]);
        assert_eq!(backend.resize_calls(&session_id), vec![(120, 40)]);

        manager
            .kill(&session_id)
            .await
            .expect("kill managed session");
        assert!(backend.is_killed(&session_id));
        assert_eq!(
            manager
                .session_status(&session_id)
                .await
                .expect("killed status"),
            ManagedSessionStatus::Killed
        );

        let send_after_kill = manager.send_input(&session_id, b"late input").await;
        assert!(matches!(send_after_kill, Err(RuntimeError::Process(_))));
    }

    #[tokio::test]
    async fn worker_manager_prompt_checkpoint_sends_configured_message() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: None,
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                ..WorkerManagerConfig::default()
            },
        );
        let spec = spawn_spec("wm-prompt-checkpoint");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        manager
            .prompt_checkpoint(&session_id)
            .await
            .expect("prompt checkpoint");

        assert_eq!(
            backend.sent_input(&session_id),
            vec![b"Emit a checkpoint now.\n".to_vec()]
        );
    }

    #[tokio::test]
    async fn worker_manager_prompt_checkpoint_rejects_empty_message() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: None,
                checkpoint_prompt_message: "   ".to_owned(),
                ..WorkerManagerConfig::default()
            },
        );
        let spec = spawn_spec("wm-prompt-checkpoint-empty");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        let error = manager
            .prompt_checkpoint(&session_id)
            .await
            .expect_err("empty prompt message should be rejected");
        assert!(matches!(
            error,
            RuntimeError::Configuration(message)
                if message.contains("checkpoint prompt message is empty")
        ));
        assert!(backend.sent_input(&session_id).is_empty());
    }

    #[tokio::test]
    async fn worker_manager_periodically_prompts_for_checkpoints() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: Some(Duration::from_millis(40)),
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                ..WorkerManagerConfig::default()
            },
        );
        let spec = spawn_spec("wm-periodic-checkpoint");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        wait_for_sent_input_count(&backend, &session_id, 1).await;

        let sent = backend.sent_input(&session_id);
        assert!(sent
            .iter()
            .all(|entry| entry == b"Emit a checkpoint now.\n"));
    }

    #[tokio::test]
    async fn worker_manager_periodic_checkpoint_prompts_pause_while_focused() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: Some(Duration::from_millis(40)),
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                ..WorkerManagerConfig::default()
            },
        );
        let spec = spawn_spec("wm-periodic-checkpoint-focused");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        manager
            .focus_session(Some(&session_id))
            .await
            .expect("focus session");
        let focused_count = backend.sent_input(&session_id).len();
        assert_sent_input_count_stable(
            &backend,
            &session_id,
            focused_count,
            Duration::from_millis(130),
        )
        .await;

        manager
            .focus_session(None)
            .await
            .expect("clear focused session");
        wait_for_sent_input_count(&backend, &session_id, focused_count + 1).await;
    }

    #[tokio::test]
    async fn worker_manager_stops_periodic_checkpoint_prompts_after_done() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: Some(Duration::from_millis(35)),
                checkpoint_prompt_message: "Emit a checkpoint now.".to_owned(),
                ..WorkerManagerConfig::default()
            },
        );
        let spec = spawn_spec("wm-periodic-checkpoint-stop");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        wait_for_sent_input_count(&backend, &session_id, 1).await;

        backend.emit_event(
            &session_id,
            BackendEvent::Done(BackendDoneEvent {
                summary: Some("finished".to_owned()),
            }),
        );
        wait_for_status(&manager, &session_id, ManagedSessionStatus::Done).await;

        let sent_before = backend.sent_input(&session_id).len();
        assert_sent_input_count_stable(
            &backend,
            &session_id,
            sent_before,
            Duration::from_millis(120),
        )
        .await;
    }

    #[tokio::test]
    async fn worker_manager_multiplexes_events_globally_and_per_session() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let session_a = RuntimeSessionId::new("wm-mux-a");
        let session_b = RuntimeSessionId::new("wm-mux-b");

        manager
            .spawn(spawn_spec(session_a.as_str()))
            .await
            .expect("spawn session a");
        manager
            .spawn(spawn_spec(session_b.as_str()))
            .await
            .expect("spawn session b");

        let mut global = manager.subscribe_all();
        let mut only_a = manager
            .subscribe(&session_a)
            .await
            .expect("subscribe session a");

        backend.emit_event(
            &session_a,
            BackendEvent::Checkpoint(BackendCheckpointEvent {
                summary: "checkpoint-a".to_owned(),
                detail: None,
                file_refs: vec!["src/lib.rs".to_owned()],
            }),
        );
        backend.emit_event(
            &session_b,
            BackendEvent::Blocked(BackendBlockedEvent {
                reason: "needs decision".to_owned(),
                hint: Some("choose profile".to_owned()),
                log_ref: None,
            }),
        );

        let session_event = next_session_event(&mut only_a).await;
        assert!(matches!(session_event, BackendEvent::Checkpoint(_)));

        let global_one = next_global_event(&mut global).await;
        let global_two = next_global_event(&mut global).await;
        let global_ids = HashSet::from([
            global_one.session_id.as_str().to_owned(),
            global_two.session_id.as_str().to_owned(),
        ]);

        assert_eq!(
            global_ids,
            HashSet::from([session_a.as_str().to_owned(), session_b.as_str().to_owned()])
        );
    }

    #[tokio::test]
    async fn worker_manager_stream_failure_emits_crashed_event_and_status() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let spec = spawn_spec("wm-stream-failure");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        let mut global = manager.subscribe_all();

        backend.emit_error(
            &session_id,
            RuntimeError::Process("stream exploded".to_owned()),
        );

        let event = next_global_event(&mut global).await;
        assert_eq!(event.session_id, session_id);
        match event.event {
            BackendEvent::Crashed(crashed) => {
                assert!(crashed.reason.contains("stream exploded"));
            }
            other => panic!("expected crashed event, got {other:?}"),
        }

        wait_for_status(&manager, &session_id, ManagedSessionStatus::Crashed).await;
    }

    #[tokio::test]
    async fn worker_manager_stream_end_emits_done_event_and_status() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let spec = spawn_spec("wm-stream-end");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        let mut global = manager.subscribe_all();
        let mut per_session = manager
            .subscribe(&session_id)
            .await
            .expect("subscribe per session");

        backend.close_stream(&session_id);

        let per_session_done = next_session_event(&mut per_session).await;
        assert!(matches!(
            per_session_done,
            BackendEvent::Done(BackendDoneEvent { summary: None })
        ));

        let global_done = next_global_event(&mut global).await;
        assert_eq!(global_done.session_id, session_id);
        assert!(matches!(
            global_done.event,
            BackendEvent::Done(BackendDoneEvent { summary: None })
        ));

        wait_for_status(&manager, &session_id, ManagedSessionStatus::Done).await;

        let stream_end = timeout(TEST_TIMEOUT, per_session.next_event())
            .await
            .expect("session stream close timeout")
            .expect("session stream close result");
        assert!(stream_end.is_none());
    }

    #[tokio::test]
    async fn worker_manager_spawn_mismatched_handle_cleans_up_spawned_session() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let spec = spawn_spec("wm-mismatched-handle");
        let session_id = spec.session_id.clone();

        backend.set_next_spawn_handle_session_id(RuntimeSessionId::new("wm-unexpected-handle"));

        let spawn_result = manager.spawn(spec).await;
        assert!(matches!(spawn_result, Err(RuntimeError::Internal(_))));
        assert!(backend.is_killed(&session_id));
        assert!(matches!(
            manager.session_status(&session_id).await,
            Err(RuntimeError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn worker_manager_focus_switches_other_sessions_to_background() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let session_a = RuntimeSessionId::new("wm-focus-a");
        let session_b = RuntimeSessionId::new("wm-focus-b");

        manager
            .spawn(spawn_spec(session_a.as_str()))
            .await
            .expect("spawn session a");
        manager
            .spawn(spawn_spec(session_b.as_str()))
            .await
            .expect("spawn session b");

        manager
            .focus_session(Some(&session_a))
            .await
            .expect("focus session a");

        let sessions = manager.list_sessions().await;
        let a = sessions
            .iter()
            .find(|session| session.session_id == session_a)
            .expect("session a summary");
        let b = sessions
            .iter()
            .find(|session| session.session_id == session_b)
            .expect("session b summary");

        assert_eq!(a.visibility, SessionVisibility::Focused);
        assert_eq!(b.visibility, SessionVisibility::Background);

        manager
            .focus_session(None)
            .await
            .expect("clear focused session");
        let sessions = manager.list_sessions().await;
        for session in sessions {
            assert_eq!(session.visibility, SessionVisibility::Background);
        }
    }

    #[tokio::test]
    async fn worker_manager_done_event_transitions_status() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(Arc::clone(&backend), WorkerManagerConfig::default());
        let spec = spawn_spec("wm-done-status");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        let mut per_session = manager
            .subscribe(&session_id)
            .await
            .expect("subscribe per session");

        backend.emit_event(
            &session_id,
            BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: b"running".to_vec(),
            }),
        );
        let _ = next_session_event(&mut per_session).await;

        backend.emit_event(
            &session_id,
            BackendEvent::Done(BackendDoneEvent {
                summary: Some("all done".to_owned()),
            }),
        );

        wait_for_status(&manager, &session_id, ManagedSessionStatus::Done).await;
    }

    #[tokio::test]
    async fn worker_manager_perf_snapshot_tracks_checkpoint_and_event_counters() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: Some(Duration::from_millis(30)),
                ..WorkerManagerConfig::default()
            },
        );
        let session_id = RuntimeSessionId::new("wm-perf-snapshot");
        manager
            .spawn(spawn_spec(session_id.as_str()))
            .await
            .expect("spawn session");
        let _global = manager.subscribe_all();

        backend.emit_event(
            &session_id,
            BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: b"perf".to_vec(),
            }),
        );
        sleep(Duration::from_millis(80)).await;

        let snapshot = manager.perf_snapshot().await;
        assert!(snapshot.metrics_enabled);
        assert!(snapshot.totals.events_received_total >= 1);
        assert!(snapshot.totals.events_forwarded_global_total >= 1);
        assert!(snapshot.totals.checkpoint_ticks_total >= 1);
        assert_eq!(snapshot.sessions.len(), 1);

        let _ = manager.kill(&session_id).await;
    }

    #[tokio::test]
    #[ignore = "perf investigation scenario"]
    async fn perf_checkpoint_overhead_10_20_30_sessions() {
        let session_counts = [10_usize, 20, 30];
        for session_count in session_counts {
            let backend = Arc::new(MockBackend::default());
            let manager = make_manager(
                Arc::clone(&backend),
                WorkerManagerConfig {
                    checkpoint_prompt_interval: Some(Duration::from_millis(25)),
                    ..WorkerManagerConfig::default()
                },
            );
            let mut session_ids = Vec::with_capacity(session_count);
            for index in 0..session_count {
                let session_id =
                    RuntimeSessionId::new(format!("wm-perf-ckpt-{session_count}-{index}"));
                manager
                    .spawn(spawn_spec(session_id.as_str()))
                    .await
                    .expect("spawn session");
                session_ids.push(session_id);
            }

            let started = std::time::Instant::now();
            sleep(Duration::from_millis(180)).await;
            let elapsed = started.elapsed();
            let snapshot = manager.perf_snapshot().await;
            print_perf_metrics_line("checkpoint_overhead", session_count, elapsed, &snapshot);
            assert!(snapshot.totals.checkpoint_ticks_total > 0);

            kill_all_sessions(&manager, &session_ids).await;
        }
    }

    #[tokio::test]
    #[ignore = "perf investigation scenario"]
    async fn perf_fanout_overhead_10_20_30_sessions() {
        let session_counts = [10_usize, 20, 30];
        for session_count in session_counts {
            let backend = Arc::new(MockBackend::default());
            let manager = make_manager(
                Arc::clone(&backend),
                WorkerManagerConfig {
                    checkpoint_prompt_interval: None,
                    ..WorkerManagerConfig::default()
                },
            );
            let mut session_ids = Vec::with_capacity(session_count);
            for index in 0..session_count {
                let session_id =
                    RuntimeSessionId::new(format!("wm-perf-fanout-{session_count}-{index}"));
                manager
                    .spawn(spawn_spec(session_id.as_str()))
                    .await
                    .expect("spawn session");
                session_ids.push(session_id);
            }
            let _global = manager.subscribe_all();

            let started = std::time::Instant::now();
            for _ in 0..40 {
                for session_id in &session_ids {
                    backend.emit_event(
                        session_id,
                        BackendEvent::Output(BackendOutputEvent {
                            stream: BackendOutputStream::Stdout,
                            bytes: b"fanout".to_vec(),
                        }),
                    );
                }
            }
            sleep(Duration::from_millis(120)).await;
            let elapsed = started.elapsed();
            let snapshot = manager.perf_snapshot().await;
            print_perf_metrics_line("fanout_overhead", session_count, elapsed, &snapshot);
            assert!(snapshot.totals.events_received_total > 0);
            assert!(snapshot.totals.events_forwarded_session_total > 0);
            assert!(snapshot.totals.events_forwarded_global_total > 0);

            kill_all_sessions(&manager, &session_ids).await;
        }
    }

    #[tokio::test]
    #[ignore = "perf investigation scenario"]
    async fn perf_subscription_lifecycle_churn() {
        let session_count = 10_usize;
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                checkpoint_prompt_interval: None,
                session_event_buffer: 8,
                ..WorkerManagerConfig::default()
            },
        );
        let mut session_ids = Vec::with_capacity(session_count);
        for index in 0..session_count {
            let session_id = RuntimeSessionId::new(format!("wm-perf-sub-{index}"));
            manager
                .spawn(spawn_spec(session_id.as_str()))
                .await
                .expect("spawn session");
            session_ids.push(session_id);
        }

        let started = std::time::Instant::now();
        for _ in 0..50 {
            let _global = manager.subscribe_all();
            for session_id in &session_ids {
                let _session_sub = manager.subscribe(session_id).await.expect("subscribe");
            }
        }
        let elapsed = started.elapsed();
        let snapshot = manager.perf_snapshot().await;
        print_perf_metrics_line(
            "subscription_lifecycle_churn",
            session_count,
            elapsed,
            &snapshot,
        );
        assert!(snapshot.totals.subscribe_session_calls_total >= (session_count * 50) as u64);
        assert!(snapshot.totals.subscribe_global_calls_total >= 50);

        kill_all_sessions(&manager, &session_ids).await;
    }
}
