use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use orchestrator_runtime::{
    BackendCapabilities, BackendCrashedEvent, BackendDoneEvent, BackendEvent, BackendKind,
    BackendNeedsInputAnswer, BackendNeedsInputEvent, BackendNeedsInputOption,
    BackendNeedsInputQuestion, BackendOutputEvent, BackendOutputStream, BackendTurnStateEvent,
    RuntimeError, RuntimeResult, RuntimeSessionId, SessionHandle, SessionLifecycle, SpawnSpec,
    WorkerBackend, WorkerEventStream, WorkerEventSubscription,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};

const DEFAULT_CODEX_BINARY: &str = "codex";
const DEFAULT_OUTPUT_BUFFER: usize = 256;
const MAX_SESSION_HISTORY_EVENTS: usize = 2048;
const DEFAULT_SERVER_STARTUP_TIMEOUT_SECS: u64 = 10;
const HARNESS_LOG_DIR_NAME: &str = "logs";
const HARNESS_RAW_LOG_FILE_NAME: &str = "harness-raw.log";
const HARNESS_NORMALIZED_LOG_FILE_NAME: &str = "harness-normalized.log";
const ENV_HARNESS_SESSION_ID: &str = "ORCHESTRATOR_HARNESS_SESSION_ID";
const REQUEST_TIMEOUT_SECS: u64 = 120;
const PLAN_COLLABORATION_MODE_KIND: &str = "plan";
const DEFAULT_COLLABORATION_MODE_KIND: &str = "default";
const PLAN_TO_IMPLEMENTATION_TRANSITION_MARKER: &str =
    "workflow transition approved: planning -> implement";
const END_PLANNING_MODE_MARKER: &str = "end planning mode now";
const TERMINAL_META_PREFIX: &str = "[[orchestrator-meta|";

#[derive(Debug, Clone)]
pub struct CodexBackendConfig {
    pub binary: PathBuf,
    pub base_args: Vec<String>,
    pub server_startup_timeout: Duration,
    pub legacy_server_base_url: Option<String>,
    pub harness_log_raw_events: bool,
    pub harness_log_normalized_events: bool,
}

impl Default for CodexBackendConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from(DEFAULT_CODEX_BINARY),
            base_args: Vec::new(),
            server_startup_timeout: Duration::from_secs(DEFAULT_SERVER_STARTUP_TIMEOUT_SECS),
            legacy_server_base_url: None,
            harness_log_raw_events: false,
            harness_log_normalized_events: false,
        }
    }
}

#[derive(Clone)]
pub struct CodexBackend {
    config: CodexBackendConfig,
    connection: Arc<AsyncMutex<Option<Arc<CodexConnection>>>>,
    sessions: Arc<AsyncMutex<HashMap<RuntimeSessionId, Arc<CodexSession>>>>,
}

struct CodexSession {
    thread_id: String,
    harness_log_normalized_events: bool,
    model: Option<String>,
    cwd: String,
    initial_instruction: Option<String>,
    collaboration_mode: std::sync::Mutex<Option<Value>>,
    default_collaboration_mode: Option<Value>,
    event_tx: broadcast::Sender<BackendEvent>,
    terminal_event_sent: AtomicBool,
    turn_active: AtomicBool,
    history_seeded: AtomicBool,
    event_history: std::sync::Mutex<VecDeque<BackendEvent>>,
    pending_user_input_requests: AsyncMutex<HashMap<String, PendingUserInputRequest>>,
}

struct CodexConnection {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    child: Arc<AsyncMutex<Child>>,
    pending: Arc<AsyncMutex<HashMap<String, oneshot::Sender<RuntimeResult<Value>>>>>,
    next_request_id: AtomicU64,
}

struct PendingUserInputRequest {
    request_id: Value,
    question_ids: Vec<String>,
}

struct BroadcastSubscription {
    receiver: broadcast::Receiver<BackendEvent>,
    replay: VecDeque<BackendEvent>,
    closed: bool,
}

impl CodexSession {
    fn collaboration_mode(&self) -> Option<Value> {
        self.collaboration_mode
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    fn set_collaboration_mode(&self, mode: Option<Value>) {
        if let Ok(mut value) = self.collaboration_mode.lock() {
            *value = mode;
        }
    }

    fn exit_planning_mode(&self) {
        self.set_collaboration_mode(self.default_collaboration_mode.clone());
    }

    fn planning_mode_active(&self) -> bool {
        let mode = self.collaboration_mode();
        collaboration_mode_is_plan(mode.as_ref())
    }

    fn record_event(&self, event: &BackendEvent) {
        if let Ok(mut history) = self.event_history.lock() {
            history.push_back(event.clone());
            if history.len() > MAX_SESSION_HISTORY_EVENTS {
                let _ = history.pop_front();
            }
        }
    }

    fn emit_non_terminal_event(&self, event: BackendEvent) {
        self.record_event(&event);
        maybe_log_normalized_harness_event(
            self.harness_log_normalized_events,
            self.thread_id.as_str(),
            false,
            &event,
        );
        let _ = self.event_tx.send(event);
    }

    fn set_turn_active(&self, active: bool) {
        let was_active = self.turn_active.swap(active, Ordering::SeqCst);
        if was_active == active {
            return;
        }
        self.emit_non_terminal_event(BackendEvent::TurnState(BackendTurnStateEvent { active }));
    }

    fn emit_terminal_event(&self, event: BackendEvent) {
        if self.terminal_event_sent.swap(true, Ordering::SeqCst) {
            return;
        }
        self.set_turn_active(false);
        self.record_event(&event);
        maybe_log_normalized_harness_event(
            self.harness_log_normalized_events,
            self.thread_id.as_str(),
            true,
            &event,
        );
        let _ = self.event_tx.send(event);
    }

    async fn register_pending_user_input_request(
        &self,
        prompt_id: String,
        request_id: Value,
        question_ids: Vec<String>,
    ) {
        self.pending_user_input_requests.lock().await.insert(
            prompt_id,
            PendingUserInputRequest {
                request_id,
                question_ids,
            },
        );
    }

    async fn take_pending_user_input_request(
        &self,
        prompt_id: &str,
    ) -> Option<PendingUserInputRequest> {
        self.pending_user_input_requests
            .lock()
            .await
            .remove(prompt_id)
    }

    async fn restore_pending_user_input_request(
        &self,
        prompt_id: String,
        request: PendingUserInputRequest,
    ) {
        self.pending_user_input_requests
            .lock()
            .await
            .insert(prompt_id, request);
    }
}

impl CodexConnection {
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> RuntimeResult<Value> {
        let id_number = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let id = id_number.to_string();
        let (tx, rx) = oneshot::channel::<RuntimeResult<Value>>();
        self.pending.lock().await.insert(id.clone(), tx);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id_number,
            "method": method,
            "params": params
        });
        if let Err(error) = self.send_json(&message).await {
            self.pending.lock().await.remove(id.as_str());
            return Err(error);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RuntimeError::Process(format!(
                "codex app-server request channel closed: {method}"
            ))),
            Err(_) => {
                self.pending.lock().await.remove(id.as_str());
                Err(RuntimeError::Protocol(format!(
                    "codex app-server request timed out after {:?}: {method}",
                    timeout
                )))
            }
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> RuntimeResult<()> {
        let mut message = serde_json::Map::new();
        message.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        message.insert("method".to_owned(), Value::String(method.to_owned()));
        if let Some(params) = params {
            message.insert("params".to_owned(), params);
        }
        self.send_json(&Value::Object(message)).await
    }

    async fn send_json(&self, value: &Value) -> RuntimeResult<()> {
        let encoded = serde_json::to_string(value).map_err(|error| {
            RuntimeError::Protocol(format!(
                "failed to encode codex app-server JSON-RPC payload: {error}"
            ))
        })?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(encoded.as_bytes()).await.map_err(|error| {
            RuntimeError::Process(format!("failed to write codex app-server request: {error}"))
        })?;
        stdin.write_all(b"\n").await.map_err(|error| {
            RuntimeError::Process(format!(
                "failed to delimit codex app-server request: {error}"
            ))
        })?;
        stdin.flush().await.map_err(|error| {
            RuntimeError::Process(format!("failed to flush codex app-server request: {error}"))
        })?;
        Ok(())
    }

    async fn is_alive(&self) -> bool {
        let mut child = self.child.lock().await;
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(_) => false,
        }
    }

    async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

impl CodexBackend {
    pub fn new(config: CodexBackendConfig) -> Self {
        Self {
            config,
            connection: Arc::new(AsyncMutex::new(None)),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    async fn ensure_connection(&self) -> RuntimeResult<Arc<CodexConnection>> {
        if let Some(base_url) = self.config.legacy_server_base_url.as_ref() {
            return Err(RuntimeError::Configuration(format!(
                "legacy Codex server base URL is no longer supported. Remove it and use `codex app-server` transport (configured value: {base_url})."
            )));
        }

        if let Some(existing) = self.connection.lock().await.clone() {
            if existing.is_alive().await {
                return Ok(existing);
            }
            existing.shutdown().await;
            *self.connection.lock().await = None;
        }

        let mut command = Command::new(&self.config.binary);
        command.args(&self.config.base_args);
        command.arg("app-server");
        command.arg("--listen");
        command.arg("stdio://");
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());

        let mut child = command.spawn().map_err(|error| {
            RuntimeError::DependencyUnavailable(format!(
                "failed to launch codex app-server '{}': {error}",
                self.config.binary.display()
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            RuntimeError::Process("codex app-server stdin unavailable".to_owned())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            RuntimeError::Process("codex app-server stdout unavailable".to_owned())
        })?;

        let connection = Arc::new(CodexConnection {
            stdin: Arc::new(AsyncMutex::new(stdin)),
            child: Arc::new(AsyncMutex::new(child)),
            pending: Arc::new(AsyncMutex::new(HashMap::new())),
            next_request_id: AtomicU64::new(1),
        });

        tokio::spawn(run_reader_loop(
            stdout,
            Arc::clone(&connection.stdin),
            Arc::clone(&connection.pending),
            Arc::clone(&self.sessions),
            self.config.harness_log_raw_events,
        ));

        let initialize_params = json!({
            "clientInfo": {
                "name": "orchestrator",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "experimentalApi": true
            }
        });
        if let Err(error) = connection
            .request(
                "initialize",
                initialize_params,
                self.config.server_startup_timeout,
            )
            .await
        {
            connection.shutdown().await;
            return Err(error);
        }
        if let Err(error) = connection.notify("initialized", None).await {
            connection.shutdown().await;
            return Err(error);
        }

        *self.connection.lock().await = Some(Arc::clone(&connection));
        Ok(connection)
    }

    async fn session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<Arc<CodexSession>> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| RuntimeError::SessionNotFound(session_id.as_str().to_owned()))
    }

    async fn remove_session(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<Arc<CodexSession>> {
        let mut sessions = self.sessions.lock().await;
        sessions
            .remove(session_id)
            .ok_or_else(|| RuntimeError::SessionNotFound(session_id.as_str().to_owned()))
    }
}

#[async_trait::async_trait]
impl SessionLifecycle for CodexBackend {
    async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
        let connection = self.ensure_connection().await?;

        if let Some(previous) = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(&spec.session_id)
        } {
            let _ = connection
                .request(
                    "thread/archive",
                    json!({ "threadId": previous.thread_id }),
                    Duration::from_secs(REQUEST_TIMEOUT_SECS),
                )
                .await;
        }

        let mut params = serde_json::Map::new();
        params.insert(
            "cwd".to_owned(),
            Value::String(spec.workdir.to_string_lossy().into_owned()),
        );
        params.insert(
            "approvalPolicy".to_owned(),
            Value::String("never".to_owned()),
        );
        if let Some(model) = spec.model.clone() {
            params.insert("model".to_owned(), Value::String(model));
        }
        if let Some(instruction) = spec.instruction_prelude.clone() {
            if !instruction.trim().is_empty() {
                params.insert(
                    "developerInstructions".to_owned(),
                    Value::String(instruction),
                );
            }
        }
        let response =
            if let Some(thread_id) = harness_session_id_from_environment(&spec.environment) {
                let mut resume_params = params.clone();
                resume_params.insert("threadId".to_owned(), Value::String(thread_id));
                match connection
                    .request(
                        "thread/resume",
                        Value::Object(resume_params),
                        Duration::from_secs(REQUEST_TIMEOUT_SECS),
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::warn!(
                            session_id = spec.session_id.as_str(),
                            error = %error,
                            "codex thread/resume failed; starting a new thread"
                        );
                        connection
                            .request(
                                "thread/start",
                                Value::Object(params),
                                Duration::from_secs(REQUEST_TIMEOUT_SECS),
                            )
                            .await?
                    }
                }
            } else {
                connection
                    .request(
                        "thread/start",
                        Value::Object(params),
                        Duration::from_secs(REQUEST_TIMEOUT_SECS),
                    )
                    .await?
            };
        let thread_id = extract_thread_id(&response).ok_or_else(|| {
            RuntimeError::Protocol(format!(
                "codex app-server thread/start response missing thread id: {}",
                sanitize_json(&response)
            ))
        })?;

        let collaboration_modes =
            resolve_collaboration_modes(connection.as_ref(), spec.model.as_deref()).await;
        let (event_tx, _drop_rx) = broadcast::channel(DEFAULT_OUTPUT_BUFFER);
        let session = Arc::new(CodexSession {
            thread_id,
            harness_log_normalized_events: self.config.harness_log_normalized_events,
            model: spec.model.clone(),
            cwd: spec.workdir.to_string_lossy().into_owned(),
            initial_instruction: spec.instruction_prelude.clone(),
            collaboration_mode: std::sync::Mutex::new(collaboration_modes.plan.clone()),
            default_collaboration_mode: collaboration_modes.default,
            event_tx,
            terminal_event_sent: AtomicBool::new(false),
            turn_active: AtomicBool::new(false),
            history_seeded: AtomicBool::new(false),
            event_history: std::sync::Mutex::new(VecDeque::new()),
            pending_user_input_requests: AsyncMutex::new(HashMap::new()),
        });
        self.sessions
            .lock()
            .await
            .insert(spec.session_id.clone(), Arc::clone(&session));

        Ok(SessionHandle {
            session_id: spec.session_id,
            backend: BackendKind::Codex,
        })
    }

    async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
        let session = self.remove_session(&session.session_id).await?;
        if let Ok(connection) = self.ensure_connection().await {
            let _ = connection
                .request(
                    "thread/archive",
                    json!({ "threadId": session.thread_id.clone() }),
                    Duration::from_secs(REQUEST_TIMEOUT_SECS),
                )
                .await;
        }
        Ok(())
    }

    async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
        let session = self.session(&session.session_id).await?;
        let connection = self.ensure_connection().await?;
        let input_text = String::from_utf8_lossy(input).trim().to_owned();
        if !input_text.is_empty() {
            session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: format!("you: {input_text}\n").into_bytes(),
            }));
        }
        if disables_planning_mode(input_text.as_str()) {
            session.exit_planning_mode();
        }

        let mut params = serde_json::Map::new();
        params.insert(
            "threadId".to_owned(),
            Value::String(session.thread_id.clone()),
        );
        params.insert(
            "input".to_owned(),
            Value::Array(vec![json!({
                "type": "text",
                "text": String::from_utf8_lossy(input).into_owned()
            })]),
        );
        params.insert(
            "approvalPolicy".to_owned(),
            Value::String("never".to_owned()),
        );
        params.insert("cwd".to_owned(), Value::String(session.cwd.clone()));
        if let Some(model) = session.model.clone() {
            params.insert("model".to_owned(), Value::String(model));
        }
        let collaboration_mode = session.collaboration_mode();
        if let Some(mode) = collaboration_mode.clone() {
            params.insert("collaborationMode".to_owned(), mode);
        }

        let request = connection
            .request(
                "turn/start",
                Value::Object(params),
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
            )
            .await;
        match request {
            Ok(_) => {
                session.set_turn_active(true);
                Ok(())
            }
            Err(error) => {
                if collaboration_mode.is_some() && is_collaboration_mode_error(&error) {
                    tracing::warn!(
                        thread_id = session.thread_id.as_str(),
                        error = %error,
                        "codex collaboration mode request failed; retrying turn/start without collaborationMode"
                    );
                    session.set_collaboration_mode(None);
                    let mut fallback_params = serde_json::Map::new();
                    fallback_params.insert(
                        "threadId".to_owned(),
                        Value::String(session.thread_id.clone()),
                    );
                    fallback_params.insert(
                        "input".to_owned(),
                        Value::Array(vec![json!({
                            "type": "text",
                            "text": String::from_utf8_lossy(input).into_owned()
                        })]),
                    );
                    fallback_params.insert(
                        "approvalPolicy".to_owned(),
                        Value::String("never".to_owned()),
                    );
                    fallback_params.insert("cwd".to_owned(), Value::String(session.cwd.clone()));
                    if let Some(model) = session.model.clone() {
                        fallback_params.insert("model".to_owned(), Value::String(model));
                    }
                    connection
                        .request(
                            "turn/start",
                            Value::Object(fallback_params),
                            Duration::from_secs(REQUEST_TIMEOUT_SECS),
                        )
                        .await?;
                    session.set_turn_active(true);
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn respond_to_needs_input(
        &self,
        session: &SessionHandle,
        prompt_id: &str,
        answers: &[BackendNeedsInputAnswer],
    ) -> RuntimeResult<()> {
        let session = self.session(&session.session_id).await?;
        let prompt_id = prompt_id.trim();
        if prompt_id.is_empty() {
            return Err(RuntimeError::Protocol(
                "codex needs-input response requires a non-empty prompt id".to_owned(),
            ));
        }

        let pending = session
            .take_pending_user_input_request(prompt_id)
            .await
            .ok_or_else(|| {
                RuntimeError::Protocol(format!(
                    "codex needs-input prompt '{prompt_id}' is not pending"
                ))
            })?;
        let result = match build_codex_tool_user_input_response(answers) {
            Ok(result) => result,
            Err(error) => {
                session
                    .restore_pending_user_input_request(prompt_id.to_owned(), pending)
                    .await;
                return Err(error);
            }
        };

        let missing_question_id = pending
            .question_ids
            .iter()
            .find(|question_id| {
                !result
                    .get("answers")
                    .and_then(|entry| entry.get(*question_id))
                    .is_some()
            })
            .cloned();
        if let Some(question_id) = missing_question_id {
            session
                .restore_pending_user_input_request(prompt_id.to_owned(), pending)
                .await;
            return Err(RuntimeError::Protocol(format!(
                "codex needs-input response missing answer for question '{question_id}'"
            )));
        }

        let connection = self.ensure_connection().await?;
        if let Err(error) = send_rpc_result(&connection.stdin, &pending.request_id, result).await {
            session
                .restore_pending_user_input_request(prompt_id.to_owned(), pending)
                .await;
            return Err(error);
        }
        if session.planning_mode_active() {
            session.exit_planning_mode();
            emit_codex_meta_output(
                &session,
                "workflow",
                "planning input complete; switched to default collaboration mode",
            );
        }
        emit_codex_meta_output(&session, "tool-call", "tool user input submitted");
        Ok(())
    }

    async fn resize(&self, _session: &SessionHandle, _cols: u16, _rows: u16) -> RuntimeResult<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkerBackend for CodexBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            structured_events: true,
            ..BackendCapabilities::default()
        }
    }

    async fn health_check(&self) -> RuntimeResult<()> {
        self.ensure_connection().await.map(|_| ())
    }

    async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
        let session = self.session(&session.session_id).await?;
        if !session.history_seeded.swap(true, Ordering::SeqCst) {
            if let Ok(connection) = self.ensure_connection().await {
                if let Err(error) = seed_session_history(
                    connection.as_ref(),
                    session.as_ref(),
                    session.initial_instruction.as_deref(),
                )
                .await
                {
                    tracing::debug!(
                        thread_id = session.thread_id.as_str(),
                        error = %error,
                        "failed to seed codex terminal history from thread/read during subscribe"
                    );
                }
            }
        }
        let replay = session
            .event_history
            .lock()
            .map(|history| history.iter().cloned().collect::<VecDeque<_>>())
            .unwrap_or_default();
        Ok(Box::new(BroadcastSubscription {
            receiver: session.event_tx.subscribe(),
            replay,
            closed: false,
        }))
    }

    async fn harness_session_id(&self, session: &SessionHandle) -> RuntimeResult<Option<String>> {
        let session = self.session(&session.session_id).await?;
        Ok(Some(session.thread_id.clone()))
    }
}

#[async_trait::async_trait]
impl WorkerEventSubscription for BroadcastSubscription {
    async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
        if self.closed {
            return Ok(None);
        }
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(event));
        }

        loop {
            match self.receiver.recv().await {
                Ok(event) => return Ok(Some(event)),
                Err(broadcast::error::RecvError::Closed) => {
                    self.closed = true;
                    return Ok(None);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    }
}

async fn run_reader_loop(
    stdout: ChildStdout,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: Arc<AsyncMutex<HashMap<String, oneshot::Sender<RuntimeResult<Value>>>>>,
    sessions: Arc<AsyncMutex<HashMap<RuntimeSessionId, Arc<CodexSession>>>>,
    harness_log_raw_events: bool,
) {
    let mut reader = BufReader::new(stdout).lines();
    let reason = loop {
        let line = match reader.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break "codex app-server process ended".to_owned(),
            Err(error) => break format!("failed to read codex app-server output: {error}"),
        };
        if line.trim().is_empty() {
            continue;
        }
        maybe_log_raw_harness_line(harness_log_raw_events, line.as_str());

        let message = match serde_json::from_str::<Value>(line.as_str()) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if let Some(response_id) = message.get("id").and_then(normalize_request_id) {
            if message.get("result").is_some() || message.get("error").is_some() {
                if let Some(sender) = pending.lock().await.remove(response_id.as_str()) {
                    if let Some(result) = message.get("result") {
                        let _ = sender.send(Ok(result.clone()));
                    } else {
                        let _ = sender.send(Err(RuntimeError::Protocol(format!(
                            "codex app-server request failed: {}",
                            sanitize_json(message.get("error").unwrap_or(&Value::Null))
                        ))));
                    }
                }
                continue;
            }
        }

        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        if let Some(request_id) = message.get("id") {
            if method == "item/tool/requestUserInput" {
                match route_tool_request_user_input(request_id, &params, &sessions).await {
                    Ok(()) => {}
                    Err(error) => {
                        let message = error.to_string();
                        let _ = send_rpc_error(&stdin, request_id, -32602, message.as_str()).await;
                    }
                }
                continue;
            }
            emit_codex_request_meta_event(method, &params, &sessions).await;
            if let Some(result) = server_request_result(method) {
                let _ = send_rpc_result(&stdin, request_id, result).await;
            } else {
                let _ = send_rpc_error(
                    &stdin,
                    request_id,
                    -32601,
                    format!("unsupported codex server request method: {method}").as_str(),
                )
                .await;
            }
            continue;
        }

        route_notification(method, &params, &sessions).await;
    };
    let waiters = {
        let mut pending_guard = pending.lock().await;
        pending_guard
            .drain()
            .map(|(_, sender)| sender)
            .collect::<Vec<_>>()
    };
    for waiter in waiters {
        let _ = waiter.send(Err(RuntimeError::Process(reason.clone())));
    }

    let sessions_snapshot = {
        let sessions = sessions.lock().await;
        sessions.values().cloned().collect::<Vec<_>>()
    };
    for session in sessions_snapshot {
        session.emit_terminal_event(BackendEvent::Crashed(BackendCrashedEvent {
            reason: reason.clone(),
        }));
    }
}

async fn route_tool_request_user_input(
    request_id: &Value,
    params: &Value,
    sessions: &Arc<AsyncMutex<HashMap<RuntimeSessionId, Arc<CodexSession>>>>,
) -> RuntimeResult<()> {
    let thread_id = extract_thread_id(params).ok_or_else(|| {
        RuntimeError::Protocol("tool requestUserInput payload missing thread id".to_owned())
    })?;
    let event = parse_tool_request_user_input_event(params)?;
    let question_ids = event
        .questions
        .iter()
        .map(|question| question.id.clone())
        .collect::<Vec<_>>();

    let session = {
        let sessions = sessions.lock().await;
        sessions
            .values()
            .find(|session| session.thread_id == thread_id)
            .cloned()
    }
    .ok_or_else(|| {
        RuntimeError::SessionNotFound(format!("no codex session found for thread '{}'", thread_id))
    })?;

    session
        .register_pending_user_input_request(
            event.prompt_id.clone(),
            request_id.clone(),
            question_ids,
        )
        .await;
    session.emit_non_terminal_event(BackendEvent::NeedsInput(event));
    emit_codex_meta_output(
        &session,
        "tool-call",
        "tool user input requested (awaiting user response)",
    );
    Ok(())
}

fn parse_tool_request_user_input_event(params: &Value) -> RuntimeResult<BackendNeedsInputEvent> {
    let prompt_id = normalize_optional_string(
        params
            .get("itemId")
            .and_then(Value::as_str)
            .or_else(|| params.get("item_id").and_then(Value::as_str)),
    )
    .map(ToOwned::to_owned)
    .or_else(|| {
        normalize_optional_string(
            params
                .get("turnId")
                .and_then(Value::as_str)
                .or_else(|| params.get("turn_id").and_then(Value::as_str)),
        )
        .map(|turn_id| format!("prompt-{turn_id}"))
    })
    .unwrap_or_else(|| "prompt".to_owned());

    let raw_questions = params
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            RuntimeError::Protocol(
                "tool requestUserInput payload missing questions array".to_owned(),
            )
        })?;
    if raw_questions.is_empty() {
        return Err(RuntimeError::Protocol(
            "tool requestUserInput payload contained no questions".to_owned(),
        ));
    }

    let mut questions = Vec::with_capacity(raw_questions.len());
    for (index, raw_question) in raw_questions.iter().enumerate() {
        questions.push(parse_tool_request_user_input_question(raw_question, index));
    }
    let summary_question = questions.first().expect("non-empty questions");
    let options = summary_question
        .options
        .as_ref()
        .map(|options| {
            options
                .iter()
                .map(|option| option.label.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(BackendNeedsInputEvent {
        prompt_id,
        question: summary_question.question.clone(),
        options,
        default_option: None,
        questions,
    })
}

fn parse_tool_request_user_input_question(
    value: &Value,
    index: usize,
) -> BackendNeedsInputQuestion {
    let fallback_id = format!("question-{}", index + 1);
    let id = normalize_optional_string(
        value
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| value.get("questionId").and_then(Value::as_str))
            .or_else(|| value.get("question_id").and_then(Value::as_str)),
    )
    .map(ToOwned::to_owned)
    .unwrap_or_else(|| fallback_id.clone());

    let header = normalize_optional_string(value.get("header").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("Question {}", index + 1));
    let question = normalize_optional_string(
        value
            .get("question")
            .and_then(Value::as_str)
            .or_else(|| value.get("prompt").and_then(Value::as_str)),
    )
    .map(ToOwned::to_owned)
    .unwrap_or_else(|| "input required".to_owned());
    let is_other = value
        .get("isOther")
        .or_else(|| value.get("is_other"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_secret = value
        .get("isSecret")
        .or_else(|| value.get("is_secret"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let options = value
        .get("options")
        .and_then(Value::as_array)
        .and_then(|raw| {
            let options = raw
                .iter()
                .filter_map(|entry| {
                    let label = normalize_optional_string(
                        entry
                            .get("label")
                            .and_then(Value::as_str)
                            .or_else(|| entry.as_str()),
                    )?
                    .to_owned();
                    let description =
                        normalize_optional_string(entry.get("description").and_then(Value::as_str))
                            .map(ToOwned::to_owned)
                            .unwrap_or_default();
                    Some(BackendNeedsInputOption { label, description })
                })
                .collect::<Vec<_>>();
            (!options.is_empty()).then_some(options)
        });

    BackendNeedsInputQuestion {
        id,
        header,
        question,
        is_other,
        is_secret,
        options,
    }
}

fn normalize_request_id(value: &Value) -> Option<String> {
    if let Some(raw) = value.as_str() {
        return Some(raw.to_owned());
    }
    if let Some(raw) = value.as_i64() {
        return Some(raw.to_string());
    }
    if let Some(raw) = value.as_u64() {
        return Some(raw.to_string());
    }
    None
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn server_request_result(method: &str) -> Option<Value> {
    match method {
        "item/commandExecution/requestApproval" => Some(json!({ "decision": "accept" })),
        "item/fileChange/requestApproval" => Some(json!({ "decision": "accept" })),
        "execCommandApproval" => Some(json!({ "decision": "approved" })),
        "applyPatchApproval" => Some(json!({ "decision": "approved" })),
        "item/tool/call" => Some(json!({
            "success": false,
            "contentItems": [{ "type": "inputText", "text": "tool call unsupported by orchestrator backend" }]
        })),
        _ => None,
    }
}

fn build_codex_tool_user_input_response(
    answers: &[BackendNeedsInputAnswer],
) -> RuntimeResult<Value> {
    let mut response_answers = serde_json::Map::new();
    for answer in answers {
        let question_id =
            normalize_optional_string(Some(answer.question_id.as_str())).ok_or_else(|| {
                RuntimeError::Protocol("codex needs-input answer requires a question id".to_owned())
            })?;
        if response_answers.contains_key(question_id) {
            return Err(RuntimeError::Protocol(format!(
                "codex needs-input response contains duplicate question id '{}'",
                question_id
            )));
        }
        let answer_values = answer
            .answers
            .iter()
            .map(|entry| Value::String(entry.clone()))
            .collect::<Vec<_>>();
        response_answers.insert(question_id.to_owned(), json!({ "answers": answer_values }));
    }

    if response_answers.is_empty() {
        return Err(RuntimeError::Protocol(
            "codex needs-input response must include at least one answer".to_owned(),
        ));
    }
    Ok(json!({ "answers": response_answers }))
}

async fn send_rpc_result(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    id: &Value,
    result: Value,
) -> RuntimeResult<()> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": id.clone(),
        "result": result
    });
    write_rpc_payload(stdin, &payload).await
}

async fn send_rpc_error(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    id: &Value,
    code: i64,
    message: &str,
) -> RuntimeResult<()> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": id.clone(),
        "error": {
            "code": code,
            "message": message
        }
    });
    write_rpc_payload(stdin, &payload).await
}

async fn write_rpc_payload(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    payload: &Value,
) -> RuntimeResult<()> {
    let encoded = serde_json::to_string(payload).map_err(|error| {
        RuntimeError::Protocol(format!(
            "failed to encode codex app-server response payload: {error}"
        ))
    })?;
    let mut stdin = stdin.lock().await;
    stdin.write_all(encoded.as_bytes()).await.map_err(|error| {
        RuntimeError::Process(format!(
            "failed to write codex app-server response payload: {error}"
        ))
    })?;
    stdin.write_all(b"\n").await.map_err(|error| {
        RuntimeError::Process(format!(
            "failed to delimit codex app-server response payload: {error}"
        ))
    })?;
    stdin.flush().await.map_err(|error| {
        RuntimeError::Process(format!(
            "failed to flush codex app-server response payload: {error}"
        ))
    })?;
    Ok(())
}

async fn route_notification(
    method: &str,
    params: &Value,
    sessions: &Arc<AsyncMutex<HashMap<RuntimeSessionId, Arc<CodexSession>>>>,
) {
    let Some(thread_id) = extract_thread_id(params) else {
        return;
    };
    let session = {
        let sessions = sessions.lock().await;
        sessions
            .values()
            .find(|session| session.thread_id == thread_id)
            .cloned()
    };
    let Some(session) = session else {
        return;
    };

    match method {
        "turn/started" | "turn/created" | "turn/inProgress" => {
            session.set_turn_active(true);
        }
        "item/agentMessage/delta" | "item/plan/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                        stream: BackendOutputStream::Stdout,
                        bytes: delta.as_bytes().to_vec(),
                    }));
                }
            }
        }
        "item/commandExecution/outputDelta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                emit_codex_meta_output(&session, "command", delta);
            }
        }
        "item/fileChange/outputDelta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                emit_codex_meta_output(&session, "file-change", delta);
            }
        }
        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                emit_codex_meta_output(&session, "reasoning", delta);
            }
        }
        "item/agentMessage"
        | "item/agentMessage/completed"
        | "item/agentMessage/created"
        | "item/plan"
        | "item/plan/completed"
        | "item/plan/created" => {
            if let Some(text) = extract_agent_text_from_notification(params) {
                if !text.is_empty() {
                    session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                        stream: BackendOutputStream::Stdout,
                        bytes: format!("{text}\n").into_bytes(),
                    }));
                }
            }
        }
        "error" => {
            let reason = params
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("codex app-server error");
            if params
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                    stream: BackendOutputStream::Stderr,
                    bytes: format!("{reason}\n").into_bytes(),
                }));
            } else {
                session.emit_terminal_event(BackendEvent::Crashed(BackendCrashedEvent {
                    reason: reason.to_owned(),
                }));
            }
        }
        "turn/completed" => {
            session.set_turn_active(false);
            let status = params
                .get("turn")
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed");
            if status == "failed" {
                let reason = params
                    .get("turn")
                    .and_then(|turn| turn.get("error"))
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("codex turn failed");
                session.emit_terminal_event(BackendEvent::Crashed(BackendCrashedEvent {
                    reason: reason.to_owned(),
                }));
            } else {
                session.emit_terminal_event(BackendEvent::Done(BackendDoneEvent { summary: None }));
            }
        }
        _ => {}
    }
}

async fn emit_codex_request_meta_event(
    method: &str,
    params: &Value,
    sessions: &Arc<AsyncMutex<HashMap<RuntimeSessionId, Arc<CodexSession>>>>,
) {
    let (kind, detail) = match method {
        "item/commandExecution/requestApproval" => {
            ("command", "command approval requested (auto-accepted)")
        }
        "item/fileChange/requestApproval" => (
            "file-change",
            "file change approval requested (auto-accepted)",
        ),
        "item/tool/requestUserInput" => (
            "tool-call",
            "tool user input requested (awaiting user response)",
        ),
        "item/tool/call" => (
            "tool-call",
            "tool call requested (currently unsupported by orchestrator backend)",
        ),
        "execCommandApproval" => ("command", "exec command approval requested (auto-approved)"),
        "applyPatchApproval" => (
            "file-change",
            "apply patch approval requested (auto-approved)",
        ),
        _ => return,
    };

    let Some(thread_id) = extract_thread_id(params) else {
        return;
    };
    let session = {
        let sessions = sessions.lock().await;
        sessions
            .values()
            .find(|session| session.thread_id == thread_id)
            .cloned()
    };
    let Some(session) = session else {
        return;
    };
    emit_codex_meta_output(&session, kind, detail);
}

fn emit_codex_meta_output(session: &CodexSession, kind: &str, text: &str) {
    let details = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if details.is_empty() {
        return;
    }
    for detail in details {
        session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
            stream: BackendOutputStream::Stdout,
            bytes: format!("\n{TERMINAL_META_PREFIX}{kind}]] {detail}\n").into_bytes(),
        }));
    }
}

fn extract_agent_text_from_notification(params: &Value) -> Option<String> {
    if let Some(text) = params.get("text").and_then(Value::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Some(delta) = params.get("delta").and_then(Value::as_str) {
        let trimmed = delta.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }

    let item = params.get("item").unwrap_or(params);
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    extract_message_content_text(item)
}

fn extract_thread_id(value: &Value) -> Option<String> {
    if let Some(thread_id) = value.get("threadId").and_then(Value::as_str) {
        return Some(thread_id.to_owned());
    }
    if let Some(thread_id) = value.get("thread_id").and_then(Value::as_str) {
        return Some(thread_id.to_owned());
    }
    if let Some(thread_id) = value
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
    {
        return Some(thread_id.to_owned());
    }
    if let Some(thread_id) = value
        .get("data")
        .and_then(|data| data.get("thread"))
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
    {
        return Some(thread_id.to_owned());
    }
    None
}

fn sanitize_json(value: &Value) -> String {
    let serialized = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    if serialized.len() > 512 {
        format!("{}...", &serialized[..512])
    } else {
        serialized
    }
}

async fn seed_session_history(
    connection: &CodexConnection,
    session: &CodexSession,
    instruction_prelude: Option<&str>,
) -> RuntimeResult<()> {
    let response = match connection
        .request(
            "thread/read",
            json!({
                "threadId": session.thread_id.clone(),
                "includeTurns": true
            }),
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => {
            if let Some(instruction) = instruction_prelude {
                let line = instruction.trim();
                if !line.is_empty() {
                    session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                        stream: BackendOutputStream::Stdout,
                        bytes: format!("system: {line}\n").into_bytes(),
                    }));
                }
            }
            return Err(error);
        }
    };

    let mut emitted_any = false;
    if let Some(instructions) = extract_thread_developer_instructions(&response) {
        let line = instructions.trim();
        if !line.is_empty() {
            emitted_any = true;
            session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: format!("system: {line}\n").into_bytes(),
            }));
        }
    } else if let Some(instruction) = instruction_prelude {
        let line = instruction.trim();
        if !line.is_empty() {
            emitted_any = true;
            session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: format!("system: {line}\n").into_bytes(),
            }));
        }
    }

    for item in history_items_for_thread_read(&response) {
        emit_history_item(session, item, &mut emitted_any);
    }

    if emitted_any {
        session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
            stream: BackendOutputStream::Stdout,
            bytes: b"\n".to_vec(),
        }));
    }

    Ok(())
}

fn extract_turns(response: &Value) -> Option<&Vec<Value>> {
    response.get("turns").and_then(Value::as_array).or_else(|| {
        response
            .get("thread")
            .and_then(|thread| thread.get("turns"))
            .and_then(Value::as_array)
    })
}

fn extract_history_items(response: &Value) -> Option<&Vec<Value>> {
    response.get("items").and_then(Value::as_array).or_else(|| {
        response
            .get("thread")
            .and_then(|thread| thread.get("items"))
            .and_then(Value::as_array)
    })
}

fn history_items_for_thread_read(response: &Value) -> Vec<&Value> {
    let turn_items = extract_turns(response)
        .map(|turns| {
            turns
                .iter()
                .filter_map(|turn| turn.get("items").and_then(Value::as_array))
                .flat_map(|items| items.iter())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !turn_items.is_empty() {
        return turn_items;
    }

    extract_history_items(response)
        .map(|items| items.iter().collect::<Vec<_>>())
        .unwrap_or_default()
}

fn emit_history_item(session: &CodexSession, item: &Value, emitted_any: &mut bool) {
    if let Some(user_text) = extract_history_user_text(item) {
        let text = user_text.trim();
        if !text.is_empty() {
            *emitted_any = true;
            session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: format!("you: {text}\n").into_bytes(),
            }));
        }
    }
    if let Some(agent_text) = extract_history_agent_text(item) {
        let text = agent_text.trim();
        if !text.is_empty() {
            *emitted_any = true;
            session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                stream: BackendOutputStream::Stdout,
                bytes: format!("{text}\n").into_bytes(),
            }));
        }
    }
}

fn extract_thread_developer_instructions(response: &Value) -> Option<String> {
    response
        .get("thread")
        .and_then(|thread| {
            thread
                .get("developerInstructions")
                .and_then(Value::as_str)
                .or_else(|| thread.get("developer_instructions").and_then(Value::as_str))
                .or_else(|| {
                    thread
                        .get("settings")
                        .and_then(|settings| settings.get("developerInstructions"))
                        .and_then(Value::as_str)
                })
        })
        .map(|value| value.to_owned())
}

fn extract_history_user_text(item: &Value) -> Option<String> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    if item_type != "userMessage" {
        return None;
    }

    extract_message_content_text(item)
}

fn extract_history_agent_text(item: &Value) -> Option<String> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    if item_type != "agentMessage" && item_type != "plan" {
        return None;
    }

    if let Some(text) = item.get("text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return Some(text.to_owned());
        }
    }

    extract_message_content_text(item)
}

fn extract_message_content_text(item: &Value) -> Option<String> {
    let content = item.get("content").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for entry in content {
        if let Some(text) = entry.get("text").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_owned());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[derive(Debug, Default)]
struct CollaborationModes {
    plan: Option<Value>,
    default: Option<Value>,
}

async fn resolve_collaboration_modes(
    connection: &CodexConnection,
    preferred_model: Option<&str>,
) -> CollaborationModes {
    let default_model_id = if let Some(model) = preferred_model {
        Some(model.to_owned())
    } else {
        resolve_default_model_id(connection).await.ok().flatten()
    };
    let response = match connection
        .request(
            "collaborationMode/list",
            json!({}),
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(
                error = %error,
                "codex collaborationMode/list unavailable; falling back to built-in collaboration mode objects"
            );
            return fallback_collaboration_modes(default_model_id.as_deref());
        }
    };

    extract_collaboration_modes(&response, default_model_id.as_deref())
        .unwrap_or_else(|| fallback_collaboration_modes(default_model_id.as_deref()))
}

fn extract_collaboration_modes(
    response: &Value,
    default_model_id: Option<&str>,
) -> Option<CollaborationModes> {
    let mut modes = CollaborationModes::default();
    let data = response.get("data").and_then(Value::as_array)?;
    for entry in data {
        let Some(mode) = entry.get("mode").and_then(Value::as_str) else {
            continue;
        };
        let normalized = mode.trim().to_ascii_lowercase();
        if normalized == PLAN_COLLABORATION_MODE_KIND {
            modes.plan =
                collaboration_mode_payload(PLAN_COLLABORATION_MODE_KIND, entry, default_model_id);
            continue;
        }
        if normalized == DEFAULT_COLLABORATION_MODE_KIND {
            modes.default = collaboration_mode_payload(
                DEFAULT_COLLABORATION_MODE_KIND,
                entry,
                default_model_id,
            );
        }
    }

    if modes.plan.is_none() {
        modes.plan = fallback_plan_collaboration_mode(default_model_id);
    }
    if modes.default.is_none() {
        modes.default = fallback_default_collaboration_mode(default_model_id);
    }
    Some(modes)
}

fn fallback_collaboration_modes(default_model_id: Option<&str>) -> CollaborationModes {
    CollaborationModes {
        plan: fallback_plan_collaboration_mode(default_model_id),
        default: fallback_default_collaboration_mode(default_model_id),
    }
}

fn fallback_plan_collaboration_mode(default_model_id: Option<&str>) -> Option<Value> {
    let model = default_model_id?;
    Some(json!({
        "mode": PLAN_COLLABORATION_MODE_KIND,
        "settings": {
            "model": model,
            "reasoning_effort": "medium",
            "developer_instructions": Value::Null
        }
    }))
}

fn fallback_default_collaboration_mode(default_model_id: Option<&str>) -> Option<Value> {
    let model = default_model_id?;
    Some(json!({
        "mode": DEFAULT_COLLABORATION_MODE_KIND,
        "settings": {
            "model": model,
            "reasoning_effort": Value::Null,
            "developer_instructions": Value::Null
        }
    }))
}

fn collaboration_mode_payload(
    mode_kind: &str,
    source: &Value,
    default_model_id: Option<&str>,
) -> Option<Value> {
    let model = extract_collaboration_mode_model(source).or(default_model_id.map(str::to_owned))?;
    let reasoning_effort =
        extract_collaboration_mode_reasoning_effort(source).unwrap_or(Value::Null);
    let developer_instructions =
        extract_collaboration_mode_developer_instructions(source).unwrap_or(Value::Null);

    Some(json!({
        "mode": mode_kind,
        "settings": {
            "model": model,
            "reasoning_effort": reasoning_effort,
            "developer_instructions": developer_instructions
        }
    }))
}

fn extract_collaboration_mode_model(value: &Value) -> Option<String> {
    value
        .get("settings")
        .and_then(|settings| settings.get("model"))
        .and_then(Value::as_str)
        .map(|entry| entry.to_owned())
        .or_else(|| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(|entry| entry.to_owned())
        })
}

fn extract_collaboration_mode_reasoning_effort(value: &Value) -> Option<Value> {
    value
        .get("settings")
        .and_then(|settings| settings.get("reasoning_effort"))
        .cloned()
        .or_else(|| value.get("reasoning_effort").cloned())
}

fn extract_collaboration_mode_developer_instructions(value: &Value) -> Option<Value> {
    value
        .get("settings")
        .and_then(|settings| settings.get("developer_instructions"))
        .cloned()
        .or_else(|| value.get("developer_instructions").cloned())
}

async fn resolve_default_model_id(connection: &CodexConnection) -> RuntimeResult<Option<String>> {
    let response = connection
        .request(
            "model/list",
            json!({}),
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await?;

    let data = response
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if data.is_empty() {
        return Ok(None);
    }

    let preferred = data
        .iter()
        .find(|entry| {
            entry
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .or_else(|| data.first());
    Ok(preferred.and_then(model_identifier_from_entry))
}

fn model_identifier_from_entry(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(Value::as_str)
        .map(|entry| entry.to_owned())
        .or_else(|| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(|entry| entry.to_owned())
        })
}

fn disables_planning_mode(input_text: &str) -> bool {
    let normalized = input_text.to_ascii_lowercase();
    normalized.contains(PLAN_TO_IMPLEMENTATION_TRANSITION_MARKER)
        || normalized.contains(END_PLANNING_MODE_MARKER)
}

fn collaboration_mode_is_plan(mode: Option<&Value>) -> bool {
    mode.and_then(|value| value.get("mode"))
        .and_then(Value::as_str)
        .is_some_and(|entry| {
            entry
                .trim()
                .eq_ignore_ascii_case(PLAN_COLLABORATION_MODE_KIND)
        })
}

fn is_collaboration_mode_error(error: &RuntimeError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("collaborationmode")
        || message.contains("collaboration mode")
        || message.contains("experimentalapi")
        || message.contains("invalid params")
        || message.contains("unknown field")
}

fn harness_session_id_from_environment(environment: &[(String, String)]) -> Option<String> {
    environment.iter().rev().find_map(|(name, value)| {
        if name == ENV_HARNESS_SESSION_ID {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
        None
    })
}

fn maybe_log_raw_harness_line(enabled: bool, line: &str) {
    if !enabled {
        return;
    }
    let payload = json!({
        "backend": "codex",
        "raw": sanitize_raw_log_line(line),
    });
    append_harness_log_line(
        harness_raw_log_path().as_path(),
        payload.to_string().as_str(),
    );
}

fn maybe_log_normalized_harness_event(
    enabled: bool,
    thread_id: &str,
    terminal: bool,
    event: &BackendEvent,
) {
    if !enabled {
        return;
    }
    let payload = match event {
        BackendEvent::Output(output) => json!({
            "backend": "codex",
            "thread_id": thread_id,
            "terminal": terminal,
            "event": "Output",
            "stream": format!("{:?}", output.stream),
            "text": String::from_utf8_lossy(&output.bytes).into_owned(),
        }),
        _ => json!({
            "backend": "codex",
            "thread_id": thread_id,
            "terminal": terminal,
            "event": format!("{event:?}"),
        }),
    };
    append_harness_log_line(
        harness_normalized_log_path().as_path(),
        payload.to_string().as_str(),
    );
}

fn append_harness_log_line(path: &Path, line: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn harness_raw_log_path() -> &'static PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| resolve_orchestrator_logs_dir().join(HARNESS_RAW_LOG_FILE_NAME))
}

fn harness_normalized_log_path() -> &'static PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| resolve_orchestrator_logs_dir().join(HARNESS_NORMALIZED_LOG_FILE_NAME))
}

fn resolve_orchestrator_logs_dir() -> PathBuf {
    resolve_data_root()
        .join("orchestrator")
        .join(HARNESS_LOG_DIR_NAME)
}

fn resolve_data_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(path) = std::env::var("LOCALAPPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(PathBuf::from(path));
            }
        }
        if let Ok(path) = std::env::var("APPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join("AppData").join("Local");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = resolve_home_dir() {
            return home.join("Library").join("Application Support");
        }
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Ok(path) = std::env::var("XDG_DATA_HOME") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join(".local").join("share");
        }
    }

    std::env::temp_dir()
}

fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

fn absolutize_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }

    if let Ok(current) = std::env::current_dir() {
        return current.join(path);
    }

    std::env::temp_dir().join(path)
}

fn sanitize_raw_log_line(line: &str) -> String {
    let mut sanitized = line
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 512;
    if sanitized.len() > MAX_LEN {
        format!("{}...", &sanitized[..MAX_LEN])
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_codex_binary() {
        let config = CodexBackendConfig::default();
        assert!(!config.binary.as_os_str().is_empty());
    }

    #[test]
    fn backend_reports_codex_kind() {
        let backend = CodexBackend::new(CodexBackendConfig::default());
        assert_eq!(backend.kind(), BackendKind::Codex);
    }

    #[test]
    fn extracts_thread_id_from_app_server_result_shapes() {
        let value = json!({ "thread": { "id": "thread-1" } });
        assert_eq!(extract_thread_id(&value).as_deref(), Some("thread-1"));
    }

    #[test]
    fn planning_to_implementing_transition_disables_planning_mode() {
        assert!(disables_planning_mode(
            "Workflow transition approved: Planning -> Implementing. End planning mode now."
        ));
    }

    #[test]
    fn planning_to_implementation_transition_disables_planning_mode() {
        assert!(disables_planning_mode(
            "Workflow transition approved: Planning -> Implementation. End planning mode now."
        ));
    }

    #[test]
    fn explicit_end_planning_mode_instruction_disables_planning_mode() {
        assert!(disables_planning_mode(
            "Planning is already complete for this ticket. End planning mode now."
        ));
    }

    #[test]
    fn non_transition_instruction_does_not_disable_planning_mode() {
        assert!(!disables_planning_mode(
            "Workflow transition approved: New -> Planning. Begin planning mode."
        ));
    }

    #[test]
    fn collaboration_mode_plan_is_detected() {
        assert!(collaboration_mode_is_plan(Some(&json!({
            "mode": "plan",
            "settings": {
                "model": "gpt-5-codex"
            }
        }))));
    }

    #[test]
    fn collaboration_mode_default_is_not_plan() {
        assert!(!collaboration_mode_is_plan(Some(&json!({
            "mode": "default",
            "settings": {
                "model": "gpt-5-codex"
            }
        }))));
    }

    #[test]
    fn collaboration_mode_missing_mode_is_not_plan() {
        assert!(!collaboration_mode_is_plan(Some(&json!({
            "settings": {
                "model": "gpt-5-codex"
            }
        }))));
    }

    #[test]
    fn history_items_for_thread_read_prefers_turn_items() {
        let response = json!({
            "turns": [
                {
                    "items": [
                        {
                            "type": "userMessage",
                            "content": [{ "text": "from turn" }]
                        }
                    ]
                }
            ],
            "items": [
                {
                    "type": "userMessage",
                    "content": [{ "text": "from top-level" }]
                }
            ]
        });

        let items = history_items_for_thread_read(&response);
        assert_eq!(items.len(), 1);
        assert_eq!(
            extract_history_user_text(items[0]).as_deref(),
            Some("from turn")
        );
    }

    #[test]
    fn history_items_for_thread_read_falls_back_to_top_level_items() {
        let response = json!({
            "turns": [
                {
                    "items": []
                }
            ],
            "items": [
                {
                    "type": "userMessage",
                    "content": [{ "text": "from top-level" }]
                }
            ]
        });

        let items = history_items_for_thread_read(&response);
        assert_eq!(items.len(), 1);
        assert_eq!(
            extract_history_user_text(items[0]).as_deref(),
            Some("from top-level")
        );
    }
}
