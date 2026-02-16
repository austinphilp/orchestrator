use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;

use crate::{
    BackendCapabilities, BackendCrashedEvent, BackendDoneEvent, BackendEvent, BackendKind,
    RuntimeError, RuntimeResult, RuntimeSessionId, SessionHandle, SpawnSpec, TerminalSnapshot,
    WorkerBackend, WorkerEventStream,
};

const DEFAULT_SESSION_EVENT_BUFFER: usize = 256;
const DEFAULT_GLOBAL_EVENT_BUFFER: usize = 1_024;
const DEFAULT_BACKGROUND_SNAPSHOT_INTERVAL_MS: u64 = 250;
const DEFAULT_CHECKPOINT_PROMPT_INTERVAL_SECS: u64 = 120;
const DEFAULT_CHECKPOINT_PROMPT_MESSAGE: &str = "Emit a checkpoint now.";

#[derive(Debug, Clone)]
pub struct WorkerManagerConfig {
    pub session_event_buffer: usize,
    pub global_event_buffer: usize,
    pub background_snapshot_interval: Duration,
    pub checkpoint_prompt_interval: Option<Duration>,
    pub checkpoint_prompt_message: String,
}

impl Default for WorkerManagerConfig {
    fn default() -> Self {
        Self {
            session_event_buffer: DEFAULT_SESSION_EVENT_BUFFER,
            global_event_buffer: DEFAULT_GLOBAL_EVENT_BUFFER,
            background_snapshot_interval: Duration::from_millis(
                DEFAULT_BACKGROUND_SNAPSHOT_INTERVAL_MS,
            ),
            checkpoint_prompt_interval: Some(Duration::from_secs(
                DEFAULT_CHECKPOINT_PROMPT_INTERVAL_SECS,
            )),
            checkpoint_prompt_message: DEFAULT_CHECKPOINT_PROMPT_MESSAGE.to_owned(),
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

pub struct SessionEventSubscription {
    receiver: broadcast::Receiver<BackendEvent>,
}

pub struct WorkerManagerEventSubscription {
    receiver: broadcast::Receiver<WorkerManagerEvent>,
}

struct CachedSnapshot {
    snapshot: TerminalSnapshot,
    captured_at: Instant,
}

struct ManagedSession {
    handle: SessionHandle,
    status: ManagedSessionStatus,
    visibility: SessionVisibility,
    event_tx: Option<broadcast::Sender<BackendEvent>>,
    event_task: Option<JoinHandle<()>>,
    checkpoint_task: Option<JoinHandle<()>>,
    last_snapshot: Option<CachedSnapshot>,
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
}

impl WorkerManager {
    pub fn new(backend: Arc<dyn WorkerBackend>) -> Self {
        Self::with_config(backend, WorkerManagerConfig::default())
    }

    pub fn with_config(backend: Arc<dyn WorkerBackend>, config: WorkerManagerConfig) -> Self {
        let (global_event_tx, _) = broadcast::channel(config.global_event_buffer.max(1));
        Self {
            backend,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            global_event_tx,
            config,
        }
    }

    pub fn backend_kind(&self) -> BackendKind {
        self.backend.kind()
    }

    pub fn backend_capabilities(&self) -> BackendCapabilities {
        self.backend.capabilities()
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
        if let Err(error) = self
            .install_session(
                session_id.clone(),
                ManagedSession {
                    handle: handle.clone(),
                    status: ManagedSessionStatus::Running,
                    visibility: SessionVisibility::Background,
                    event_tx: Some(event_tx.clone()),
                    event_task: None,
                    checkpoint_task: None,
                    last_snapshot: None,
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
        );
        let checkpoint_task = self.periodic_checkpoint_prompt().map(|(interval, prompt)| {
            Self::spawn_periodic_checkpoint_task(
                Arc::clone(&self.backend),
                Arc::clone(&self.sessions),
                handle.clone(),
                interval,
                prompt,
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

    pub async fn snapshot(&self, session_id: &RuntimeSessionId) -> RuntimeResult<TerminalSnapshot> {
        let handle = {
            let sessions = self.sessions.read().await;
            match sessions.get(session_id) {
                Some(SessionEntry::Starting) => {
                    return Err(RuntimeError::Process(format!(
                        "worker session is still starting: {}",
                        session_id.as_str()
                    )))
                }
                Some(SessionEntry::Managed(session))
                    if session.status != ManagedSessionStatus::Running =>
                {
                    if let Some(cache) = &session.last_snapshot {
                        return Ok(cache.snapshot.clone());
                    }
                    return Err(RuntimeError::Process(format!(
                        "worker session is not running: {} ({:?})",
                        session_id.as_str(),
                        session.status
                    )));
                }
                Some(SessionEntry::Managed(session)) => {
                    if session.visibility == SessionVisibility::Background {
                        if let Some(cache) = &session.last_snapshot {
                            if cache.captured_at.elapsed()
                                < self.config.background_snapshot_interval
                            {
                                return Ok(cache.snapshot.clone());
                            }
                        }
                    }
                    session.handle.clone()
                }
                None => {
                    return Err(RuntimeError::SessionNotFound(
                        session_id.as_str().to_owned(),
                    ))
                }
            }
        };

        let snapshot = self.backend.snapshot(&handle).await?;
        let mut sessions = self.sessions.write().await;
        if let Some(SessionEntry::Managed(session)) = sessions.get_mut(session_id) {
            session.last_snapshot = Some(CachedSnapshot {
                snapshot: snapshot.clone(),
                captured_at: Instant::now(),
            });
        }
        Ok(snapshot)
    }

    pub async fn subscribe(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<SessionEventSubscription> {
        let sessions = self.sessions.read().await;
        match sessions.get(session_id) {
            Some(SessionEntry::Starting) => Err(RuntimeError::Process(format!(
                "worker session is still starting: {}",
                session_id.as_str()
            ))),
            Some(SessionEntry::Managed(session)) => {
                let event_tx = session.event_tx.as_ref().ok_or_else(|| {
                    RuntimeError::Process(format!(
                        "worker session output stream is closed: {}",
                        session_id.as_str()
                    ))
                })?;
                Ok(SessionEventSubscription {
                    receiver: event_tx.subscribe(),
                })
            }
            None => Err(RuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            )),
        }
    }

    pub fn subscribe_all(&self) -> WorkerManagerEventSubscription {
        WorkerManagerEventSubscription {
            receiver: self.global_event_tx.subscribe(),
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
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(Some(event)) => {
                        let terminal_status = match &event {
                            BackendEvent::Done(_) => Some(ManagedSessionStatus::Done),
                            BackendEvent::Crashed(_) => Some(ManagedSessionStatus::Crashed),
                            _ => None,
                        };

                        let _ = event_tx.send(event.clone());
                        let _ = global_event_tx.send(WorkerManagerEvent {
                            session_id: session_id.clone(),
                            event,
                        });

                        if let Some(status) = terminal_status {
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
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;

            loop {
                ticker.tick().await;

                let visibility = {
                    let sessions = sessions.read().await;
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
                    continue;
                }

                if backend.send_input(&handle, &prompt).await.is_err() {
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
            Err(broadcast::error::RecvError::Closed) => Ok(None),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Err(RuntimeError::Process(
                format!("worker session subscriber lagged; dropped {skipped} events"),
            )),
        }
    }
}

impl WorkerManagerEventSubscription {
    pub async fn next_event(&mut self) -> RuntimeResult<Option<WorkerManagerEvent>> {
        match self.receiver.recv().await {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::RecvError::Closed) => Ok(None),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Err(RuntimeError::Process(
                format!("worker manager subscriber lagged; dropped {skipped} events"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
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
        snapshot_calls: usize,
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

        fn snapshot_calls(&self, session_id: &RuntimeSessionId) -> usize {
            let state = self.state.lock().expect("lock backend state");
            state
                .sessions
                .get(session_id)
                .expect("session exists")
                .snapshot_calls
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
                    snapshot_calls: 0,
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

        async fn snapshot(&self, session: &SessionHandle) -> RuntimeResult<TerminalSnapshot> {
            let mut state = self.state.lock().expect("lock backend state");
            let session = state.sessions.get_mut(&session.session_id).ok_or_else(|| {
                RuntimeError::SessionNotFound(session.session_id.as_str().to_owned())
            })?;
            session.snapshot_calls += 1;

            let line = format!("snapshot-{}", session.snapshot_calls);
            Ok(TerminalSnapshot {
                cols: 80,
                rows: 24,
                cursor_col: line.len() as u16,
                cursor_row: 0,
                lines: vec![line],
            })
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
        sleep(Duration::from_millis(130)).await;
        assert!(
            backend.sent_input(&session_id).is_empty(),
            "focused sessions should not receive periodic checkpoint prompts"
        );

        manager
            .focus_session(None)
            .await
            .expect("clear focused session");
        wait_for_sent_input_count(&backend, &session_id, 1).await;
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
        sleep(Duration::from_millis(120)).await;
        let sent_after = backend.sent_input(&session_id).len();
        assert_eq!(sent_after, sent_before);
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
    async fn worker_manager_throttles_background_snapshots() {
        let backend = Arc::new(MockBackend::default());
        let manager = make_manager(
            Arc::clone(&backend),
            WorkerManagerConfig {
                session_event_buffer: 32,
                global_event_buffer: 64,
                background_snapshot_interval: Duration::from_millis(150),
                checkpoint_prompt_interval: Some(Duration::from_secs(120)),
                checkpoint_prompt_message: DEFAULT_CHECKPOINT_PROMPT_MESSAGE.to_owned(),
            },
        );
        let spec = spawn_spec("wm-background-snapshots");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        manager
            .set_session_visibility(&session_id, SessionVisibility::Background)
            .await
            .expect("set background visibility");

        let first = manager.snapshot(&session_id).await.expect("first snapshot");
        let second = manager
            .snapshot(&session_id)
            .await
            .expect("second snapshot");
        assert_eq!(first, second);
        assert_eq!(backend.snapshot_calls(&session_id), 1);

        sleep(Duration::from_millis(170)).await;
        let third = manager.snapshot(&session_id).await.expect("third snapshot");
        assert_ne!(third.lines, first.lines);
        assert_eq!(backend.snapshot_calls(&session_id), 2);

        manager
            .set_session_visibility(&session_id, SessionVisibility::Focused)
            .await
            .expect("focus session");
        let focused_one = manager
            .snapshot(&session_id)
            .await
            .expect("focused snapshot one");
        let focused_two = manager
            .snapshot(&session_id)
            .await
            .expect("focused snapshot two");
        assert_ne!(focused_one.lines, focused_two.lines);
        assert_eq!(backend.snapshot_calls(&session_id), 4);
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
}
