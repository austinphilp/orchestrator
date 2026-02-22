use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use orchestrator_worker_eventbus::{WorkerEventBus, WorkerSessionEventSubscription};
use orchestrator_worker_protocol::backend::{
    WorkerBackendCapabilities, WorkerBackendInfo, WorkerBackendKind, WorkerEventStream,
    WorkerSessionControl, WorkerSessionStreamSource,
};
use orchestrator_worker_protocol::error::{WorkerRuntimeError, WorkerRuntimeResult};
use orchestrator_worker_protocol::event::{WorkerCrashedEvent, WorkerDoneEvent, WorkerEvent};
use orchestrator_worker_protocol::ids::WorkerSessionId;
use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::registry::{WorkerLifecycleRegistry, WorkerLifecycleSnapshot, WorkerLifecycleSummary};
use crate::state::{WorkerSessionState, WorkerSessionVisibility};

pub trait WorkerLifecycleBackend:
    WorkerSessionControl + WorkerSessionStreamSource + WorkerBackendInfo + Send + Sync
{
}

impl<T> WorkerLifecycleBackend for T where
    T: WorkerSessionControl + WorkerSessionStreamSource + WorkerBackendInfo + Send + Sync
{
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerLifecycleTaskSnapshot {
    pub active_stream_ingestion_tasks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerLifecyclePerfSnapshot {
    pub session_count: usize,
    pub starting_sessions: usize,
    pub running_sessions: usize,
    pub done_sessions: usize,
    pub crashed_sessions: usize,
    pub killed_sessions: usize,
    pub focused_sessions: usize,
    pub active_stream_ingestion_tasks: usize,
    pub spawn_requests_total: u64,
    pub spawn_success_total: u64,
    pub spawn_failures_total: u64,
    pub kill_requests_total: u64,
    pub kill_success_total: u64,
    pub kill_failures_total: u64,
    pub send_input_requests_total: u64,
    pub send_input_failures_total: u64,
    pub resize_requests_total: u64,
    pub resize_failures_total: u64,
    pub needs_input_response_requests_total: u64,
    pub needs_input_response_failures_total: u64,
    pub focus_requests_total: u64,
    pub focus_failures_total: u64,
    pub set_visibility_requests_total: u64,
    pub set_visibility_failures_total: u64,
    pub subscribe_session_requests_total: u64,
    pub subscribe_session_failures_total: u64,
    pub stream_events_forwarded_total: u64,
    pub stream_closed_total: u64,
    pub stream_errors_total: u64,
    pub terminal_transitions_done_total: u64,
    pub terminal_transitions_crashed_total: u64,
    pub terminal_transitions_killed_total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct WorkerLifecycleStateSummary {
    session_count: usize,
    starting_sessions: usize,
    running_sessions: usize,
    done_sessions: usize,
    crashed_sessions: usize,
    killed_sessions: usize,
    focused_sessions: usize,
}

#[derive(Debug, Default)]
struct WorkerLifecyclePerfCounters {
    spawn_requests_total: AtomicU64,
    spawn_success_total: AtomicU64,
    spawn_failures_total: AtomicU64,
    kill_requests_total: AtomicU64,
    kill_success_total: AtomicU64,
    kill_failures_total: AtomicU64,
    send_input_requests_total: AtomicU64,
    send_input_failures_total: AtomicU64,
    resize_requests_total: AtomicU64,
    resize_failures_total: AtomicU64,
    needs_input_response_requests_total: AtomicU64,
    needs_input_response_failures_total: AtomicU64,
    focus_requests_total: AtomicU64,
    focus_failures_total: AtomicU64,
    set_visibility_requests_total: AtomicU64,
    set_visibility_failures_total: AtomicU64,
    subscribe_session_requests_total: AtomicU64,
    subscribe_session_failures_total: AtomicU64,
    stream_events_forwarded_total: AtomicU64,
    stream_closed_total: AtomicU64,
    stream_errors_total: AtomicU64,
    terminal_transitions_done_total: AtomicU64,
    terminal_transitions_crashed_total: AtomicU64,
    terminal_transitions_killed_total: AtomicU64,
}

impl WorkerLifecyclePerfCounters {
    fn record_terminal_transition(&self, state: WorkerSessionState) {
        match state {
            WorkerSessionState::Done => {
                self.terminal_transitions_done_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            WorkerSessionState::Crashed => {
                self.terminal_transitions_crashed_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            WorkerSessionState::Killed => {
                self.terminal_transitions_killed_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            WorkerSessionState::Starting | WorkerSessionState::Running => {}
        }
    }

    fn snapshot(
        &self,
        state: WorkerLifecycleStateSummary,
        active_stream_ingestion_tasks: usize,
    ) -> WorkerLifecyclePerfSnapshot {
        WorkerLifecyclePerfSnapshot {
            session_count: state.session_count,
            starting_sessions: state.starting_sessions,
            running_sessions: state.running_sessions,
            done_sessions: state.done_sessions,
            crashed_sessions: state.crashed_sessions,
            killed_sessions: state.killed_sessions,
            focused_sessions: state.focused_sessions,
            active_stream_ingestion_tasks,
            spawn_requests_total: self.spawn_requests_total.load(Ordering::Relaxed),
            spawn_success_total: self.spawn_success_total.load(Ordering::Relaxed),
            spawn_failures_total: self.spawn_failures_total.load(Ordering::Relaxed),
            kill_requests_total: self.kill_requests_total.load(Ordering::Relaxed),
            kill_success_total: self.kill_success_total.load(Ordering::Relaxed),
            kill_failures_total: self.kill_failures_total.load(Ordering::Relaxed),
            send_input_requests_total: self.send_input_requests_total.load(Ordering::Relaxed),
            send_input_failures_total: self.send_input_failures_total.load(Ordering::Relaxed),
            resize_requests_total: self.resize_requests_total.load(Ordering::Relaxed),
            resize_failures_total: self.resize_failures_total.load(Ordering::Relaxed),
            needs_input_response_requests_total: self
                .needs_input_response_requests_total
                .load(Ordering::Relaxed),
            needs_input_response_failures_total: self
                .needs_input_response_failures_total
                .load(Ordering::Relaxed),
            focus_requests_total: self.focus_requests_total.load(Ordering::Relaxed),
            focus_failures_total: self.focus_failures_total.load(Ordering::Relaxed),
            set_visibility_requests_total: self
                .set_visibility_requests_total
                .load(Ordering::Relaxed),
            set_visibility_failures_total: self
                .set_visibility_failures_total
                .load(Ordering::Relaxed),
            subscribe_session_requests_total: self
                .subscribe_session_requests_total
                .load(Ordering::Relaxed),
            subscribe_session_failures_total: self
                .subscribe_session_failures_total
                .load(Ordering::Relaxed),
            stream_events_forwarded_total: self
                .stream_events_forwarded_total
                .load(Ordering::Relaxed),
            stream_closed_total: self.stream_closed_total.load(Ordering::Relaxed),
            stream_errors_total: self.stream_errors_total.load(Ordering::Relaxed),
            terminal_transitions_done_total: self
                .terminal_transitions_done_total
                .load(Ordering::Relaxed),
            terminal_transitions_crashed_total: self
                .terminal_transitions_crashed_total
                .load(Ordering::Relaxed),
            terminal_transitions_killed_total: self
                .terminal_transitions_killed_total
                .load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub struct WorkerLifecycle {
    backend: Arc<dyn WorkerLifecycleBackend>,
    registry: Arc<RwLock<WorkerLifecycleRegistry>>,
    eventbus: Arc<WorkerEventBus>,
    stream_tasks: Arc<RwLock<HashMap<WorkerSessionId, JoinHandle<()>>>>,
    perf: Arc<WorkerLifecyclePerfCounters>,
}

impl WorkerLifecycle {
    pub fn new(backend: Arc<dyn WorkerLifecycleBackend>, eventbus: Arc<WorkerEventBus>) -> Self {
        Self {
            backend,
            registry: Arc::new(RwLock::new(WorkerLifecycleRegistry::default())),
            eventbus,
            stream_tasks: Arc::new(RwLock::new(HashMap::new())),
            perf: Arc::new(WorkerLifecyclePerfCounters::default()),
        }
    }

    pub fn backend_kind(&self) -> WorkerBackendKind {
        self.backend.kind()
    }

    pub fn backend_capabilities(&self) -> WorkerBackendCapabilities {
        self.backend.capabilities()
    }

    pub async fn health_check(&self) -> WorkerRuntimeResult<()> {
        self.backend.health_check().await
    }

    pub async fn spawn(
        &self,
        spec: WorkerSpawnRequest,
    ) -> WorkerRuntimeResult<WorkerSessionHandle> {
        self.perf
            .spawn_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let session_id = spec.session_id.clone();
        {
            let mut registry = self.registry.write().await;
            if let Err(error) = registry.reserve_session(session_id.clone()) {
                self.perf
                    .spawn_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        }

        let handle = match self.backend.spawn(spec).await {
            Ok(handle) => handle,
            Err(error) => {
                self.perf
                    .spawn_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                self.rollback_spawn(&session_id).await;
                return Err(error);
            }
        };

        if handle.session_id != session_id {
            let _ = self.backend.kill(&handle).await;
            let _ = self
                .backend
                .kill(&WorkerSessionHandle {
                    session_id: session_id.clone(),
                    backend: handle.backend.clone(),
                })
                .await;
            self.rollback_spawn(&session_id).await;
            self.perf
                .spawn_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(WorkerRuntimeError::Internal(format!(
                "backend returned mismatched session id: expected {}, got {}",
                session_id.as_str(),
                handle.session_id.as_str()
            )));
        }

        let stream = match self.backend.subscribe(&handle).await {
            Ok(stream) => stream,
            Err(error) => {
                self.perf
                    .spawn_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                let _ = self.backend.kill(&handle).await;
                self.rollback_spawn(&session_id).await;
                return Err(error);
            }
        };

        {
            let mut registry = self.registry.write().await;
            if let Err(error) = registry.mark_spawned(&session_id, handle.clone()) {
                let _ = self.backend.kill(&handle).await;
                self.rollback_spawn(&session_id).await;
                self.perf
                    .spawn_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        }

        let mut stream_tasks = self.stream_tasks.write().await;
        if stream_tasks.contains_key(&session_id) {
            drop(stream_tasks);
            let _ = self.backend.kill(&handle).await;
            let mut registry = self.registry.write().await;
            if let Ok(transition) = registry.mark_killed(&session_id) {
                self.perf
                    .record_terminal_transition(transition.resulting_state);
            }
            self.eventbus.remove_session(&session_id);
            self.perf
                .spawn_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(WorkerRuntimeError::Internal(format!(
                "worker stream ingestion task already exists: {}",
                session_id.as_str()
            )));
        }
        let stream_task = self.spawn_stream_ingestion_task(session_id.clone(), stream);
        stream_tasks.insert(session_id, stream_task);

        self.perf
            .spawn_success_total
            .fetch_add(1, Ordering::Relaxed);
        Ok(handle)
    }

    pub async fn kill(&self, session_id: &WorkerSessionId) -> WorkerRuntimeResult<()> {
        self.perf
            .kill_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let handle = match self.running_session_handle(session_id).await {
            Ok(handle) => handle,
            Err(error) => {
                self.perf
                    .kill_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        if let Err(error) = self.backend.kill(&handle).await {
            self.perf
                .kill_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }

        {
            let mut registry = self.registry.write().await;
            match registry.mark_killed(session_id) {
                Ok(transition) => {
                    self.perf
                        .record_terminal_transition(transition.resulting_state);
                }
                Err(error) => {
                    self.perf
                        .kill_failures_total
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(error);
                }
            }
        }

        self.abort_stream_task(session_id).await;
        self.eventbus.remove_session(session_id);
        self.perf.kill_success_total.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn send_input(
        &self,
        session_id: &WorkerSessionId,
        input: &[u8],
    ) -> WorkerRuntimeResult<()> {
        self.perf
            .send_input_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let handle = match self.running_session_handle(session_id).await {
            Ok(handle) => handle,
            Err(error) => {
                self.perf
                    .send_input_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        if let Err(error) = self.backend.send_input(&handle, input).await {
            self.perf
                .send_input_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub async fn resize(
        &self,
        session_id: &WorkerSessionId,
        cols: u16,
        rows: u16,
    ) -> WorkerRuntimeResult<()> {
        self.perf
            .resize_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let handle = match self.running_session_handle(session_id).await {
            Ok(handle) => handle,
            Err(error) => {
                self.perf
                    .resize_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        if let Err(error) = self.backend.resize(&handle, cols, rows).await {
            self.perf
                .resize_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub async fn respond_to_needs_input(
        &self,
        session_id: &WorkerSessionId,
        prompt_id: &str,
        answers: &[orchestrator_worker_protocol::event::WorkerNeedsInputAnswer],
    ) -> WorkerRuntimeResult<()> {
        self.perf
            .needs_input_response_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let handle = match self.running_session_handle(session_id).await {
            Ok(handle) => handle,
            Err(error) => {
                self.perf
                    .needs_input_response_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        if let Err(error) = self
            .backend
            .respond_to_needs_input(&handle, prompt_id, answers)
            .await
        {
            self.perf
                .needs_input_response_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub async fn focus_session(
        &self,
        focused: Option<&WorkerSessionId>,
    ) -> WorkerRuntimeResult<()> {
        self.perf
            .focus_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let mut registry = self.registry.write().await;
        if let Err(error) = registry.focus_session(focused) {
            self.perf
                .focus_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub async fn set_visibility(
        &self,
        session_id: &WorkerSessionId,
        visibility: WorkerSessionVisibility,
    ) -> WorkerRuntimeResult<()> {
        self.perf
            .set_visibility_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let mut registry = self.registry.write().await;
        if let Err(error) = registry.set_visibility(session_id, visibility) {
            self.perf
                .set_visibility_failures_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub async fn list_sessions(&self) -> Vec<WorkerLifecycleSummary> {
        let registry = self.registry.read().await;
        registry.list_sessions()
    }

    pub async fn status(
        &self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerSessionState> {
        let registry = self.registry.read().await;
        registry.status(session_id)
    }

    pub async fn subscribe_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerSessionEventSubscription> {
        self.perf
            .subscribe_session_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let registry = self.registry.read().await;
        let status = match registry.status(session_id) {
            Ok(status) => status,
            Err(error) => {
                self.perf
                    .subscribe_session_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        match status {
            WorkerSessionState::Starting => {
                self.perf
                    .subscribe_session_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                Err(WorkerRuntimeError::Process(format!(
                    "worker session is still starting: {}",
                    session_id.as_str()
                )))
            }
            WorkerSessionState::Running => Ok(self.eventbus.subscribe_session(session_id.clone())),
            WorkerSessionState::Done | WorkerSessionState::Crashed | WorkerSessionState::Killed => {
                self.perf
                    .subscribe_session_failures_total
                    .fetch_add(1, Ordering::Relaxed);
                Err(WorkerRuntimeError::Process(format!(
                    "worker session output stream is closed: {}",
                    session_id.as_str()
                )))
            }
        }
    }

    pub async fn snapshot(&self, session_id: &WorkerSessionId) -> Option<WorkerLifecycleSnapshot> {
        let registry = self.registry.read().await;
        registry.snapshot(session_id)
    }

    pub async fn task_snapshot(&self) -> WorkerLifecycleTaskSnapshot {
        let stream_tasks = self.stream_tasks.read().await;
        WorkerLifecycleTaskSnapshot {
            active_stream_ingestion_tasks: stream_tasks.len(),
        }
    }

    pub async fn perf_snapshot(&self) -> WorkerLifecyclePerfSnapshot {
        let state_summary = {
            let registry = self.registry.read().await;
            summarize_registry_state(&registry)
        };
        let stream_tasks = self.stream_tasks.read().await;
        self.perf.snapshot(state_summary, stream_tasks.len())
    }

    fn spawn_stream_ingestion_task(
        &self,
        session_id: WorkerSessionId,
        mut stream: WorkerEventStream,
    ) -> JoinHandle<()> {
        let registry = Arc::clone(&self.registry);
        let eventbus = Arc::clone(&self.eventbus);
        let stream_tasks = Arc::clone(&self.stream_tasks);
        let perf = Arc::clone(&self.perf);

        tokio::spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(Some(event)) => {
                        perf.stream_events_forwarded_total
                            .fetch_add(1, Ordering::Relaxed);
                        let terminal_state = terminal_state_from_event(&event);
                        eventbus.publish(session_id.clone(), event);
                        if let Some(terminal_state) = terminal_state {
                            transition_terminal_state(
                                &registry,
                                &eventbus,
                                &perf,
                                &session_id,
                                terminal_state,
                            )
                            .await;
                            break;
                        }
                    }
                    Ok(None) => {
                        perf.stream_closed_total.fetch_add(1, Ordering::Relaxed);
                        eventbus.publish(
                            session_id.clone(),
                            WorkerEvent::Done(WorkerDoneEvent { summary: None }),
                        );
                        transition_terminal_state(
                            &registry,
                            &eventbus,
                            &perf,
                            &session_id,
                            WorkerSessionState::Done,
                        )
                        .await;
                        break;
                    }
                    Err(error) => {
                        perf.stream_errors_total.fetch_add(1, Ordering::Relaxed);
                        eventbus.publish(
                            session_id.clone(),
                            WorkerEvent::Crashed(WorkerCrashedEvent {
                                reason: format!("worker output stream failed: {error}"),
                            }),
                        );
                        transition_terminal_state(
                            &registry,
                            &eventbus,
                            &perf,
                            &session_id,
                            WorkerSessionState::Crashed,
                        )
                        .await;
                        break;
                    }
                }
            }

            let mut stream_tasks = stream_tasks.write().await;
            stream_tasks.remove(&session_id);
        })
    }

    async fn running_session_handle(
        &self,
        session_id: &WorkerSessionId,
    ) -> WorkerRuntimeResult<WorkerSessionHandle> {
        let registry = self.registry.read().await;
        registry.running_session_handle(session_id)
    }

    async fn rollback_spawn(&self, session_id: &WorkerSessionId) {
        let mut registry = self.registry.write().await;
        let _ = registry.rollback_spawn(session_id);
    }

    async fn abort_stream_task(&self, session_id: &WorkerSessionId) {
        let mut stream_tasks = self.stream_tasks.write().await;
        if let Some(task) = stream_tasks.remove(session_id) {
            task.abort();
        }
    }
}

fn terminal_state_from_event(event: &WorkerEvent) -> Option<WorkerSessionState> {
    match event {
        WorkerEvent::Done(_) => Some(WorkerSessionState::Done),
        WorkerEvent::Crashed(_) => Some(WorkerSessionState::Crashed),
        _ => None,
    }
}

async fn transition_terminal_state(
    registry: &Arc<RwLock<WorkerLifecycleRegistry>>,
    eventbus: &Arc<WorkerEventBus>,
    perf: &Arc<WorkerLifecyclePerfCounters>,
    session_id: &WorkerSessionId,
    state: WorkerSessionState,
) {
    if matches!(
        state,
        WorkerSessionState::Starting | WorkerSessionState::Running
    ) {
        return;
    }

    let mut registry = registry.write().await;
    if let Ok(transition) = match state {
        WorkerSessionState::Done => registry.mark_done(session_id),
        WorkerSessionState::Crashed => registry.mark_crashed(session_id),
        WorkerSessionState::Killed => registry.mark_killed(session_id),
        WorkerSessionState::Starting | WorkerSessionState::Running => return,
    } {
        perf.record_terminal_transition(transition.resulting_state);
    }
    drop(registry);
    eventbus.remove_session(session_id);
}

fn summarize_registry_state(registry: &WorkerLifecycleRegistry) -> WorkerLifecycleStateSummary {
    let sessions = registry.list_sessions();
    let mut summary = WorkerLifecycleStateSummary {
        session_count: sessions.len(),
        ..WorkerLifecycleStateSummary::default()
    };
    for session in sessions {
        match session.state {
            WorkerSessionState::Starting => summary.starting_sessions += 1,
            WorkerSessionState::Running => summary.running_sessions += 1,
            WorkerSessionState::Done => summary.done_sessions += 1,
            WorkerSessionState::Crashed => summary.crashed_sessions += 1,
            WorkerSessionState::Killed => summary.killed_sessions += 1,
        }
        if session.visibility == WorkerSessionVisibility::Focused {
            summary.focused_sessions += 1;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use orchestrator_worker_eventbus::WorkerEventBus;
    use orchestrator_worker_protocol::backend::{
        WorkerBackendCapabilities, WorkerBackendInfo, WorkerBackendKind, WorkerEventStream,
        WorkerEventSubscription, WorkerSessionControl, WorkerSessionStreamSource,
    };
    use orchestrator_worker_protocol::error::{WorkerRuntimeError, WorkerRuntimeResult};
    use orchestrator_worker_protocol::event::{
        WorkerDoneEvent, WorkerEvent, WorkerNeedsInputAnswer, WorkerOutputEvent, WorkerOutputStream,
    };
    use orchestrator_worker_protocol::ids::WorkerSessionId;
    use orchestrator_worker_protocol::session::{WorkerSessionHandle, WorkerSpawnRequest};
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout};

    use crate::controller::WorkerLifecycle;
    use crate::state::{WorkerSessionState, WorkerSessionVisibility};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);
    type StreamMessage = WorkerRuntimeResult<Option<WorkerEvent>>;

    #[derive(Default)]
    struct MockBackend {
        state: Mutex<MockBackendState>,
    }

    #[derive(Default)]
    struct MockBackendState {
        sessions: HashMap<WorkerSessionId, MockSession>,
    }

    struct MockSession {
        event_tx: Option<mpsc::UnboundedSender<StreamMessage>>,
        event_rx: Option<mpsc::UnboundedReceiver<StreamMessage>>,
        sent_input: Vec<Vec<u8>>,
        needs_input_responses: Vec<(String, Vec<WorkerNeedsInputAnswer>)>,
        killed: bool,
        resize_calls: Vec<(u16, u16)>,
    }

    struct MockEventStream {
        receiver: mpsc::UnboundedReceiver<StreamMessage>,
    }

    impl MockBackend {
        fn emit_event(&self, session_id: &WorkerSessionId, event: WorkerEvent) {
            let sender = {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .event_tx
                    .as_ref()
                    .expect("stream sender")
                    .clone()
            };
            sender.send(Ok(Some(event))).expect("emit mock event");
        }

        fn emit_error(&self, session_id: &WorkerSessionId, error: WorkerRuntimeError) {
            let sender = {
                let state = self.state.lock().expect("lock backend state");
                state
                    .sessions
                    .get(session_id)
                    .expect("session exists")
                    .event_tx
                    .as_ref()
                    .expect("stream sender")
                    .clone()
            };
            sender.send(Err(error)).expect("emit mock stream error");
        }

        fn close_stream(&self, session_id: &WorkerSessionId) {
            let mut state = self.state.lock().expect("lock backend state");
            if let Some(session) = state.sessions.get_mut(session_id) {
                session.event_tx = None;
            }
        }
    }

    #[async_trait]
    impl WorkerSessionControl for MockBackend {
        async fn spawn(
            &self,
            spec: WorkerSpawnRequest,
        ) -> WorkerRuntimeResult<WorkerSessionHandle> {
            let mut state = self.state.lock().expect("lock backend state");
            if state.sessions.contains_key(&spec.session_id) {
                return Err(WorkerRuntimeError::Configuration(format!(
                    "duplicate mock session id: {}",
                    spec.session_id.as_str()
                )));
            }
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            state.sessions.insert(
                spec.session_id.clone(),
                MockSession {
                    event_tx: Some(event_tx),
                    event_rx: Some(event_rx),
                    sent_input: Vec::new(),
                    needs_input_responses: Vec::new(),
                    killed: false,
                    resize_calls: Vec::new(),
                },
            );
            Ok(WorkerSessionHandle {
                session_id: spec.session_id,
                backend: WorkerBackendKind::OpenCode,
            })
        }

        async fn kill(&self, session: &WorkerSessionHandle) -> WorkerRuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                WorkerRuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.killed = true;
            Ok(())
        }

        async fn send_input(
            &self,
            session: &WorkerSessionHandle,
            input: &[u8],
        ) -> WorkerRuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                WorkerRuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.sent_input.push(input.to_vec());
            Ok(())
        }

        async fn respond_to_needs_input(
            &self,
            session: &WorkerSessionHandle,
            prompt_id: &str,
            answers: &[WorkerNeedsInputAnswer],
        ) -> WorkerRuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                WorkerRuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session
                .needs_input_responses
                .push((prompt_id.to_owned(), answers.to_vec()));
            Ok(())
        }

        async fn resize(
            &self,
            session: &WorkerSessionHandle,
            cols: u16,
            rows: u16,
        ) -> WorkerRuntimeResult<()> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                WorkerRuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.resize_calls.push((cols, rows));
            Ok(())
        }
    }

    #[async_trait]
    impl WorkerSessionStreamSource for MockBackend {
        async fn subscribe(
            &self,
            session: &WorkerSessionHandle,
        ) -> WorkerRuntimeResult<WorkerEventStream> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                WorkerRuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            let receiver = session.event_rx.take().ok_or_else(|| {
                WorkerRuntimeError::Process(
                    "mock backend supports only one subscription".to_owned(),
                )
            })?;
            Ok(Box::new(MockEventStream { receiver }))
        }
    }

    #[async_trait]
    impl WorkerBackendInfo for MockBackend {
        fn kind(&self) -> WorkerBackendKind {
            WorkerBackendKind::OpenCode
        }

        fn capabilities(&self) -> WorkerBackendCapabilities {
            WorkerBackendCapabilities {
                structured_events: true,
                session_export: false,
                diff_provider: false,
                supports_background: true,
            }
        }

        async fn health_check(&self) -> WorkerRuntimeResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl WorkerEventSubscription for MockEventStream {
        async fn next_event(&mut self) -> WorkerRuntimeResult<Option<WorkerEvent>> {
            match self.receiver.recv().await {
                Some(event) => event,
                None => Ok(None),
            }
        }
    }

    fn spawn_request(session_id: &str) -> WorkerSpawnRequest {
        WorkerSpawnRequest {
            session_id: WorkerSessionId::new(session_id),
            workdir: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(session_id),
            model: None,
            instruction_prelude: None,
            environment: Vec::new(),
        }
    }

    async fn wait_for_status(
        lifecycle: &WorkerLifecycle,
        session_id: &WorkerSessionId,
        expected: WorkerSessionState,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let status = lifecycle.status(session_id).await.expect("session status");
            if status == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for status transition to {expected:?}; observed {status:?}"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn spawn_output_done_path_publishes_to_eventbus_and_marks_done() {
        let backend = Arc::new(MockBackend::default());
        let eventbus = Arc::new(WorkerEventBus::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), eventbus.clone());

        let handle = lifecycle
            .spawn(spawn_request("lifecycle-done"))
            .await
            .expect("spawn session");
        let mut session_sub = lifecycle
            .subscribe_session(&handle.session_id)
            .await
            .expect("subscribe session");
        assert_eq!(
            lifecycle
                .task_snapshot()
                .await
                .active_stream_ingestion_tasks,
            1
        );

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Output(WorkerOutputEvent {
                stream: WorkerOutputStream::Stdout,
                bytes: b"hello from lifecycle".to_vec(),
            }),
        );
        let output = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("output event timeout")
            .expect("output event should arrive");
        match output.event {
            WorkerEvent::Output(output) => assert_eq!(output.bytes, b"hello from lifecycle"),
            other => panic!("expected output event, got {other:?}"),
        }

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent {
                summary: Some("all done".to_owned()),
            }),
        );
        let done = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("done event timeout")
            .expect("done event should arrive");
        match done.event {
            WorkerEvent::Done(done) => assert_eq!(done.summary.as_deref(), Some("all done")),
            other => panic!("expected done event, got {other:?}"),
        }

        let closed = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("closed session timeout");
        assert!(closed.is_none());

        wait_for_status(&lifecycle, &handle.session_id, WorkerSessionState::Done).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if lifecycle
                .task_snapshot()
                .await
                .active_stream_ingestion_tasks
                == 0
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for stream ingestion task to clear"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn spawn_stream_error_publishes_crashed_and_marks_session_crashed() {
        let backend = Arc::new(MockBackend::default());
        let eventbus = Arc::new(WorkerEventBus::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), eventbus.clone());

        let handle = lifecycle
            .spawn(spawn_request("lifecycle-crash"))
            .await
            .expect("spawn session");
        let mut session_sub = eventbus.subscribe_session(handle.session_id.clone());

        backend.emit_error(
            &handle.session_id,
            WorkerRuntimeError::Process("simulated stream failure".to_owned()),
        );

        let crashed = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("crashed event timeout")
            .expect("crashed event should arrive");
        match crashed.event {
            WorkerEvent::Crashed(crashed) => {
                assert!(crashed.reason.contains("simulated stream failure"));
            }
            other => panic!("expected crashed event, got {other:?}"),
        }

        let closed = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("closed session timeout");
        assert!(closed.is_none());
        wait_for_status(&lifecycle, &handle.session_id, WorkerSessionState::Crashed).await;
        let snapshot = lifecycle.perf_snapshot().await;
        assert_eq!(snapshot.stream_errors_total, 1);
        assert_eq!(snapshot.stream_events_forwarded_total, 0);
        assert_eq!(snapshot.terminal_transitions_crashed_total, 1);
    }

    #[tokio::test]
    async fn stream_close_publishes_synthetic_done_and_marks_session_done() {
        let backend = Arc::new(MockBackend::default());
        let eventbus = Arc::new(WorkerEventBus::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), eventbus.clone());

        let handle = lifecycle
            .spawn(spawn_request("lifecycle-stream-close"))
            .await
            .expect("spawn session");
        let mut session_sub = eventbus.subscribe_session(handle.session_id.clone());

        backend.close_stream(&handle.session_id);

        let done = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("done event timeout")
            .expect("done event should arrive");
        assert!(matches!(done.event, WorkerEvent::Done(_)));
        let closed = timeout(TEST_TIMEOUT, session_sub.next_event())
            .await
            .expect("closed session timeout");
        assert!(closed.is_none());
        wait_for_status(&lifecycle, &handle.session_id, WorkerSessionState::Done).await;

        let snapshot = lifecycle.perf_snapshot().await;
        assert_eq!(snapshot.stream_closed_total, 1);
        assert_eq!(snapshot.stream_events_forwarded_total, 0);
        assert_eq!(snapshot.terminal_transitions_done_total, 1);
    }

    #[tokio::test]
    async fn focus_and_visibility_controls_update_runtime_state() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));

        let session_a = lifecycle
            .spawn(spawn_request("lifecycle-focus-a"))
            .await
            .expect("spawn session a");
        let session_b = lifecycle
            .spawn(spawn_request("lifecycle-focus-b"))
            .await
            .expect("spawn session b");

        lifecycle
            .focus_session(Some(&session_a.session_id))
            .await
            .expect("focus session a");
        let focused_a = lifecycle
            .snapshot(&session_a.session_id)
            .await
            .expect("snapshot a");
        let focused_b = lifecycle
            .snapshot(&session_b.session_id)
            .await
            .expect("snapshot b");
        assert_eq!(focused_a.visibility, WorkerSessionVisibility::Focused);
        assert_eq!(focused_b.visibility, WorkerSessionVisibility::Background);

        lifecycle
            .set_visibility(&session_b.session_id, WorkerSessionVisibility::Focused)
            .await
            .expect("move focus to b");
        let moved_a = lifecycle
            .snapshot(&session_a.session_id)
            .await
            .expect("snapshot a");
        let moved_b = lifecycle
            .snapshot(&session_b.session_id)
            .await
            .expect("snapshot b");
        assert_eq!(moved_a.visibility, WorkerSessionVisibility::Background);
        assert_eq!(moved_b.visibility, WorkerSessionVisibility::Focused);

        lifecycle.focus_session(None).await.expect("clear focus");
        let list = lifecycle.list_sessions().await;
        let a = list
            .iter()
            .find(|summary| summary.session_id == session_a.session_id)
            .expect("session a listed");
        let b = list
            .iter()
            .find(|summary| summary.session_id == session_b.session_id)
            .expect("session b listed");
        assert_eq!(a.visibility, WorkerSessionVisibility::Background);
        assert_eq!(b.visibility, WorkerSessionVisibility::Background);
    }

    #[tokio::test]
    async fn subscribe_session_rejects_unknown_and_closed_sessions() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));

        let missing = WorkerSessionId::new("lifecycle-missing");
        assert!(matches!(
            lifecycle.subscribe_session(&missing).await,
            Err(WorkerRuntimeError::SessionNotFound(_))
        ));

        let handle = lifecycle
            .spawn(spawn_request("lifecycle-subscribe-closed"))
            .await
            .expect("spawn session");
        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent { summary: None }),
        );
        wait_for_status(&lifecycle, &handle.session_id, WorkerSessionState::Done).await;

        let closed = lifecycle
            .subscribe_session(&handle.session_id)
            .await
            .expect_err("closed session subscription must fail");
        assert!(matches!(closed, WorkerRuntimeError::Process(_)));
        assert!(closed.to_string().contains("output stream is closed"));
    }

    #[tokio::test]
    async fn perf_snapshot_aggregates_lifecycle_operation_counters() {
        let backend = Arc::new(MockBackend::default());
        let lifecycle = WorkerLifecycle::new(backend.clone(), Arc::new(WorkerEventBus::default()));

        let handle = lifecycle
            .spawn(spawn_request("lifecycle-perf"))
            .await
            .expect("spawn session");
        lifecycle
            .send_input(&handle.session_id, b"checkpoint\n")
            .await
            .expect("send input");
        lifecycle
            .resize(&handle.session_id, 120, 30)
            .await
            .expect("resize");
        lifecycle
            .respond_to_needs_input(
                &handle.session_id,
                "prompt-1",
                &[WorkerNeedsInputAnswer {
                    question_id: "q-1".to_owned(),
                    answers: vec!["y".to_owned()],
                }],
            )
            .await
            .expect("respond needs input");
        lifecycle
            .focus_session(Some(&handle.session_id))
            .await
            .expect("focus session");
        lifecycle
            .set_visibility(&handle.session_id, WorkerSessionVisibility::Background)
            .await
            .expect("set visibility");
        let _subscription = lifecycle
            .subscribe_session(&handle.session_id)
            .await
            .expect("subscribe session");

        backend.emit_event(
            &handle.session_id,
            WorkerEvent::Done(WorkerDoneEvent { summary: None }),
        );
        wait_for_status(&lifecycle, &handle.session_id, WorkerSessionState::Done).await;

        let snapshot = lifecycle.perf_snapshot().await;
        assert_eq!(snapshot.spawn_requests_total, 1);
        assert_eq!(snapshot.spawn_success_total, 1);
        assert_eq!(snapshot.kill_requests_total, 0);
        assert_eq!(snapshot.send_input_requests_total, 1);
        assert_eq!(snapshot.resize_requests_total, 1);
        assert_eq!(snapshot.needs_input_response_requests_total, 1);
        assert_eq!(snapshot.focus_requests_total, 1);
        assert_eq!(snapshot.set_visibility_requests_total, 1);
        assert_eq!(snapshot.subscribe_session_requests_total, 1);
        assert_eq!(snapshot.stream_events_forwarded_total, 1);
        assert_eq!(snapshot.terminal_transitions_done_total, 1);
        assert_eq!(snapshot.done_sessions, 1);
        assert_eq!(snapshot.active_stream_ingestion_tasks, 0);
    }
}
