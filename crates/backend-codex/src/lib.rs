use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use orchestrator_runtime::{
    BackendCapabilities, BackendCrashedEvent, BackendDoneEvent, BackendEvent, BackendKind,
    BackendOutputEvent, BackendOutputStream, RuntimeError, RuntimeResult, RuntimeSessionId,
    SessionHandle, SessionLifecycle, SpawnSpec, WorkerBackend, WorkerEventStream,
    WorkerEventSubscription,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};

const DEFAULT_CODEX_BINARY: &str = "codex";
const DEFAULT_OUTPUT_BUFFER: usize = 256;
const MAX_SESSION_HISTORY_EVENTS: usize = 2048;
const DEFAULT_SERVER_STARTUP_TIMEOUT_SECS: u64 = 10;
const ENV_CODEX_BIN: &str = "ORCHESTRATOR_CODEX_BIN";
const ENV_CODEX_SERVER_BASE_URL: &str = "ORCHESTRATOR_CODEX_SERVER_BASE_URL";
const ENV_HARNESS_LOG_RAW_EVENTS: &str = "ORCHESTRATOR_HARNESS_LOG_RAW_EVENTS";
const ENV_HARNESS_LOG_NORMALIZED_EVENTS: &str =
    "ORCHESTRATOR_HARNESS_LOG_NORMALIZED_EVENTS";
const ENV_HARNESS_SESSION_ID: &str = "ORCHESTRATOR_HARNESS_SESSION_ID";
const ENV_HARNESS_SERVER_STARTUP_TIMEOUT_SECS: &str =
    "ORCHESTRATOR_HARNESS_SERVER_STARTUP_TIMEOUT_SECS";
const REQUEST_TIMEOUT_SECS: u64 = 120;
const PLAN_COLLABORATION_MODE_KIND: &str = "plan";
const DEFAULT_COLLABORATION_MODE_KIND: &str = "default";
const PLAN_TO_IMPLEMENTATION_TRANSITION_MARKER: &str =
    "workflow transition approved: planning -> implementation";

#[derive(Debug, Clone)]
pub struct CodexBackendConfig {
    pub binary: PathBuf,
    pub base_args: Vec<String>,
    pub server_startup_timeout: Duration,
    pub legacy_server_base_url: Option<String>,
}

impl Default for CodexBackendConfig {
    fn default() -> Self {
        Self {
            binary: std::env::var_os(ENV_CODEX_BIN)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CODEX_BINARY)),
            base_args: Vec::new(),
            server_startup_timeout: Duration::from_secs(
                std::env::var(ENV_HARNESS_SERVER_STARTUP_TIMEOUT_SECS)
                    .ok()
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .unwrap_or(DEFAULT_SERVER_STARTUP_TIMEOUT_SECS),
            ),
            legacy_server_base_url: std::env::var(ENV_CODEX_SERVER_BASE_URL).ok(),
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
    model: Option<String>,
    cwd: String,
    initial_instruction: Option<String>,
    collaboration_mode: std::sync::Mutex<Option<Value>>,
    default_collaboration_mode: Option<Value>,
    event_tx: broadcast::Sender<BackendEvent>,
    terminal_event_sent: AtomicBool,
    history_seeded: AtomicBool,
    event_history: std::sync::Mutex<VecDeque<BackendEvent>>,
}

struct CodexConnection {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    child: Arc<AsyncMutex<Child>>,
    pending: Arc<AsyncMutex<HashMap<String, oneshot::Sender<RuntimeResult<Value>>>>>,
    next_request_id: AtomicU64,
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
        if harness_log_normalized_events_enabled() {
            tracing::debug!(
                target: "orchestrator_harness_events",
                backend = "codex",
                thread_id = self.thread_id.as_str(),
                event = ?event,
                "normalized harness event"
            );
        }
        let _ = self.event_tx.send(event);
    }

    fn emit_terminal_event(&self, event: BackendEvent) {
        if self.terminal_event_sent.swap(true, Ordering::SeqCst) {
            return;
        }
        self.record_event(&event);
        if harness_log_normalized_events_enabled() {
            tracing::debug!(
                target: "orchestrator_harness_events",
                backend = "codex",
                thread_id = self.thread_id.as_str(),
                event = ?event,
                "normalized harness terminal event"
            );
        }
        let _ = self.event_tx.send(event);
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
            RuntimeError::Process(format!("failed to delimit codex app-server request: {error}"))
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

    pub fn from_env() -> Self {
        Self::new(CodexBackendConfig::default())
    }

    async fn ensure_connection(&self) -> RuntimeResult<Arc<CodexConnection>> {
        if let Some(base_url) = self.config.legacy_server_base_url.as_ref() {
            return Err(RuntimeError::Configuration(format!(
                "{ENV_CODEX_SERVER_BASE_URL} is no longer supported for Codex. Remove it and use `codex app-server` transport (configured value: {base_url})."
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
            .request("initialize", initialize_params, self.config.server_startup_timeout)
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

    async fn remove_session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<Arc<CodexSession>> {
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
                params.insert("developerInstructions".to_owned(), Value::String(instruction));
            }
        }
        let response = if let Some(thread_id) = harness_session_id_from_environment(&spec.environment) {
            let mut resume_params = params.clone();
            resume_params.insert(
                "threadId".to_owned(),
                Value::String(thread_id),
            );
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

        let collaboration_modes = resolve_collaboration_modes(connection.as_ref()).await;
        let (event_tx, _drop_rx) = broadcast::channel(DEFAULT_OUTPUT_BUFFER);
        let session = Arc::new(CodexSession {
            thread_id,
            model: spec.model.clone(),
            cwd: spec.workdir.to_string_lossy().into_owned(),
            initial_instruction: spec.instruction_prelude.clone(),
            collaboration_mode: std::sync::Mutex::new(collaboration_modes.plan.clone()),
            default_collaboration_mode: collaboration_modes.default,
            event_tx,
            terminal_event_sent: AtomicBool::new(false),
            history_seeded: AtomicBool::new(false),
            event_history: std::sync::Mutex::new(VecDeque::new()),
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
            Ok(_) => Ok(()),
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
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
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
        maybe_log_raw_harness_line(line.as_str());

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
        pending_guard.drain().map(|(_, sender)| sender).collect::<Vec<_>>()
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

fn server_request_result(method: &str) -> Option<Value> {
    match method {
        "item/commandExecution/requestApproval" => Some(json!({ "decision": "accept" })),
        "item/fileChange/requestApproval" => Some(json!({ "decision": "accept" })),
        "execCommandApproval" => Some(json!({ "decision": "approved" })),
        "applyPatchApproval" => Some(json!({ "decision": "approved" })),
        "item/tool/requestUserInput" => Some(json!({ "answers": {} })),
        "item/tool/call" => Some(json!({
            "success": false,
            "contentItems": [{ "type": "inputText", "text": "tool call unsupported by orchestrator backend" }]
        })),
        _ => None,
    }
}

async fn send_rpc_result(stdin: &Arc<AsyncMutex<ChildStdin>>, id: &Value, result: Value) -> RuntimeResult<()> {
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

async fn write_rpc_payload(stdin: &Arc<AsyncMutex<ChildStdin>>, payload: &Value) -> RuntimeResult<()> {
    let encoded = serde_json::to_string(payload).map_err(|error| {
        RuntimeError::Protocol(format!("failed to encode codex app-server response payload: {error}"))
    })?;
    let mut stdin = stdin.lock().await;
    stdin.write_all(encoded.as_bytes()).await.map_err(|error| {
        RuntimeError::Process(format!("failed to write codex app-server response payload: {error}"))
    })?;
    stdin.write_all(b"\n").await.map_err(|error| {
        RuntimeError::Process(format!("failed to delimit codex app-server response payload: {error}"))
    })?;
    stdin.flush().await.map_err(|error| {
        RuntimeError::Process(format!("failed to flush codex app-server response payload: {error}"))
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
        "item/agentMessage/delta"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/reasoning/textDelta"
        | "item/reasoning/summaryTextDelta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    session.emit_non_terminal_event(BackendEvent::Output(BackendOutputEvent {
                        stream: BackendOutputStream::Stdout,
                        bytes: delta.as_bytes().to_vec(),
                    }));
                }
            }
        }
        "item/agentMessage" | "item/agentMessage/completed" | "item/agentMessage/created" => {
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

    if let Some(turns) = extract_turns(&response) {
        for turn in turns {
            if let Some(items) = turn.get("items").and_then(Value::as_array) {
                for item in items {
                    emit_history_item(session, item, &mut emitted_any);
                }
            }
        }
    }
    if let Some(items) = extract_history_items(&response) {
        for item in items {
            emit_history_item(session, item, &mut emitted_any);
        }
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
    response
        .get("turns")
        .and_then(Value::as_array)
        .or_else(|| response.get("thread").and_then(|thread| thread.get("turns")).and_then(Value::as_array))
}

fn extract_history_items(response: &Value) -> Option<&Vec<Value>> {
    response
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| {
            response
                .get("thread")
                .and_then(|thread| thread.get("items"))
                .and_then(Value::as_array)
        })
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
    if item_type != "agentMessage" {
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

async fn resolve_collaboration_modes(connection: &CodexConnection) -> CollaborationModes {
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
            return fallback_collaboration_modes();
        }
    };

    extract_collaboration_modes(&response).unwrap_or_else(fallback_collaboration_modes)
}

fn extract_collaboration_modes(response: &Value) -> Option<CollaborationModes> {
    let mut modes = CollaborationModes::default();
    let data = response.get("data").and_then(Value::as_array)?;
    for entry in data {
        let Some(mode) = entry.get("mode").and_then(Value::as_str) else {
            continue;
        };
        let normalized = mode.trim().to_ascii_lowercase();
        if normalized == PLAN_COLLABORATION_MODE_KIND {
            modes.plan = Some(entry.clone());
            continue;
        }
        if normalized == DEFAULT_COLLABORATION_MODE_KIND {
            modes.default = Some(entry.clone());
        }
    }

    if modes.plan.is_none() {
        modes.plan = Some(fallback_plan_collaboration_mode());
    }
    if modes.default.is_none() {
        modes.default = Some(fallback_default_collaboration_mode());
    }
    Some(modes)
}

fn fallback_collaboration_modes() -> CollaborationModes {
    CollaborationModes {
        plan: Some(fallback_plan_collaboration_mode()),
        default: Some(fallback_default_collaboration_mode()),
    }
}

fn fallback_plan_collaboration_mode() -> Value {
    json!({
        "name": "Plan",
        "mode": PLAN_COLLABORATION_MODE_KIND,
        "model": null,
        "reasoning_effort": "medium",
        "developer_instructions": null
    })
}

fn fallback_default_collaboration_mode() -> Value {
    json!({
        "name": "Default",
        "mode": DEFAULT_COLLABORATION_MODE_KIND,
        "model": null,
        "reasoning_effort": null,
        "developer_instructions": null
    })
}

fn disables_planning_mode(input_text: &str) -> bool {
    input_text
        .to_ascii_lowercase()
        .contains(PLAN_TO_IMPLEMENTATION_TRANSITION_MARKER)
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
    environment
        .iter()
        .rev()
        .find_map(|(name, value)| {
            if name == ENV_HARNESS_SESSION_ID {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
            None
        })
}

fn harness_log_raw_events_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| env_flag_enabled(ENV_HARNESS_LOG_RAW_EVENTS))
}

fn harness_log_normalized_events_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| env_flag_enabled(ENV_HARNESS_LOG_NORMALIZED_EVENTS))
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn maybe_log_raw_harness_line(line: &str) {
    if !harness_log_raw_events_enabled() {
        return;
    }
    tracing::debug!(
        target: "orchestrator_harness_events",
        backend = "codex",
        raw = sanitize_raw_log_line(line),
        "raw harness event line"
    );
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
    fn defaults_use_codex_binary_from_env_or_codex() {
        let config = CodexBackendConfig::default();
        assert!(!config.binary.as_os_str().is_empty());
    }

    #[test]
    fn backend_reports_codex_kind() {
        let backend = CodexBackend::from_env();
        assert_eq!(backend.kind(), BackendKind::Codex);
    }

    #[test]
    fn extracts_thread_id_from_app_server_result_shapes() {
        let value = json!({ "thread": { "id": "thread-1" } });
        assert_eq!(extract_thread_id(&value).as_deref(), Some("thread-1"));
    }
}
