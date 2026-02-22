use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command as SyncCommand;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use orchestrator_worker_protocol::{
    WorkerArtifactEvent as BackendArtifactEvent, WorkerArtifactId as RuntimeArtifactId,
    WorkerArtifactKind as BackendArtifactKind, WorkerBackendCapabilities as BackendCapabilities,
    WorkerBackendInfo, WorkerBackendKind as BackendKind, WorkerBlockedEvent as BackendBlockedEvent,
    WorkerCheckpointEvent as BackendCheckpointEvent, WorkerCrashedEvent as BackendCrashedEvent,
    WorkerDoneEvent as BackendDoneEvent, WorkerEvent as BackendEvent, WorkerEventStream,
    WorkerEventSubscription, WorkerNeedsInputEvent as BackendNeedsInputEvent,
    WorkerNeedsInputOption as BackendNeedsInputOption,
    WorkerNeedsInputQuestion as BackendNeedsInputQuestion, WorkerOutputEvent as BackendOutputEvent,
    WorkerOutputStream as BackendOutputStream, WorkerRuntimeError as RuntimeError,
    WorkerRuntimeResult as RuntimeResult, WorkerSessionControl,
    WorkerSessionHandle as SessionHandle, WorkerSessionId as RuntimeSessionId,
    WorkerSessionStreamSource, WorkerSpawnRequest as SpawnSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

const DEFAULT_OPENCODE_BINARY: &str = "opencode";
const DEFAULT_OPENCODE_SERVER_BASE_URL: &str = "http://127.0.0.1:8787";
const HARNESS_LOG_DIR_NAME: &str = "logs";
const HARNESS_RAW_LOG_FILE_NAME: &str = "harness-raw.log";
const HARNESS_NORMALIZED_LOG_FILE_NAME: &str = "harness-normalized.log";
const DEFAULT_OUTPUT_BUFFER: usize = 256;
const DEFAULT_SERVER_STARTUP_TIMEOUT_SECS: u64 = 10;
const SESSION_EXPORT_HELP_MARKERS: &[&str] =
    &["session-export", "session_export", "session export"];
const DIFF_PROVIDER_HELP_MARKERS: &[&str] = &["diff-provider", "diff_provider", "diff provider"];
const OPTIONAL_CAPABILITY_HELP_ARGS: [&str; 2] = ["--help", "-h"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeBackendConfig {
    pub binary: PathBuf,
    pub base_args: Vec<String>,
    pub output_buffer: usize,
    pub server_base_url: Option<String>,
    pub server_startup_timeout: Duration,
    pub allow_unsafe_command_paths: bool,
    pub harness_log_raw_events: bool,
    pub harness_log_normalized_events: bool,
}

impl Default for OpenCodeBackendConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from(DEFAULT_OPENCODE_BINARY),
            base_args: Vec::new(),
            output_buffer: DEFAULT_OUTPUT_BUFFER,
            server_base_url: Some(DEFAULT_OPENCODE_SERVER_BASE_URL.to_owned()),
            server_startup_timeout: Duration::from_secs(DEFAULT_SERVER_STARTUP_TIMEOUT_SECS),
            allow_unsafe_command_paths: false,
            harness_log_raw_events: false,
            harness_log_normalized_events: false,
        }
    }
}

#[derive(Clone)]
pub struct OpenCodeBackend {
    config: OpenCodeBackendConfig,
    backend_kind: BackendKind,
    client: reqwest::Client,
    server_state: Arc<AsyncMutex<ServerState>>,
    sessions: Arc<AsyncMutex<HashMap<RuntimeSessionId, Arc<OpenCodeSession>>>>,
    backend_capabilities: BackendCapabilities,
}

#[derive(Debug)]
struct OpenCodeSession {
    backend_kind: BackendKind,
    remote_session_id: String,
    harness_log_raw_events: bool,
    harness_log_normalized_events: bool,
    relay_task: AsyncMutex<Option<JoinHandle<()>>>,
    event_tx: broadcast::Sender<BackendEvent>,
    terminal_event_sent: AtomicBool,
}

#[derive(Default)]
struct ServerState {
    base_url: Option<String>,
    process: Option<Child>,
}

impl OpenCodeSession {
    fn emit_non_terminal_event(&self, event: BackendEvent) {
        maybe_log_normalized_harness_event(
            self.harness_log_normalized_events,
            self.backend_kind.clone(),
            self.remote_session_id.as_str(),
            false,
            &event,
        );
        let _ = self.event_tx.send(event);
    }

    fn emit_terminal_event(&self, event: BackendEvent) {
        if self.terminal_event_sent.swap(true, Ordering::SeqCst) {
            return;
        }
        maybe_log_normalized_harness_event(
            self.harness_log_normalized_events,
            self.backend_kind.clone(),
            self.remote_session_id.as_str(),
            true,
            &event,
        );
        let _ = self.event_tx.send(event);
    }
}

impl OpenCodeBackend {
    pub fn new(config: OpenCodeBackendConfig) -> Self {
        Self::new_with_kind(config, BackendKind::OpenCode)
    }

    pub fn new_with_kind(config: OpenCodeBackendConfig, backend_kind: BackendKind) -> Self {
        let backend_capabilities = detect_optional_capabilities(&config.binary);
        Self {
            config,
            backend_kind,
            client: reqwest::Client::new(),
            server_state: Arc::new(AsyncMutex::new(ServerState::default())),
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            backend_capabilities,
        }
    }

    fn default_server_base_url(&self) -> String {
        match self.backend_kind {
            BackendKind::OpenCode => DEFAULT_OPENCODE_SERVER_BASE_URL.to_owned(),
            BackendKind::Codex => "http://127.0.0.1:8788".to_owned(),
            _ => DEFAULT_OPENCODE_SERVER_BASE_URL.to_owned(),
        }
    }

    fn output_buffer(&self) -> usize {
        self.config.output_buffer.max(1)
    }

    async fn session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<Arc<OpenCodeSession>> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| RuntimeError::SessionNotFound(session_id.as_str().to_owned()))
    }

    async fn remove_session(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<Arc<OpenCodeSession>> {
        let mut sessions = self.sessions.lock().await;
        sessions
            .remove(session_id)
            .ok_or_else(|| RuntimeError::SessionNotFound(session_id.as_str().to_owned()))
    }

    async fn ensure_server_base_url(&self) -> RuntimeResult<String> {
        if let Some(base_url) = self.config.server_base_url.clone() {
            return Ok(base_url);
        }

        {
            let state = self.server_state.lock().await;
            if let Some(base_url) = state.base_url.as_ref() {
                return Ok(base_url.clone());
            }
        }

        self.start_managed_server().await?;
        let state = self.server_state.lock().await;
        state.base_url.clone().ok_or_else(|| {
            RuntimeError::Process("managed harness server started without base URL".to_owned())
        })
    }

    async fn start_managed_server(&self) -> RuntimeResult<()> {
        let base_url = self.default_server_base_url();
        {
            let mut state = self.server_state.lock().await;
            if state.base_url.is_some() {
                return Ok(());
            }

            validate_command_binary_path(
                &self.config.binary,
                self.config.allow_unsafe_command_paths,
            )?;
            let mut command = Command::new(&self.config.binary);
            command.args(&self.config.base_args);
            command.arg("serve");
            command.stdin(Stdio::null());
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());

            let child = command.spawn().map_err(|error| {
                RuntimeError::DependencyUnavailable(format!(
                    "failed to start {:?} harness server '{}': {error}",
                    self.backend_kind,
                    self.config.binary.display()
                ))
            })?;

            state.process = Some(child);
            state.base_url = Some(base_url.clone());
        }

        if let Err(error) = self.wait_for_server_health(base_url.as_str()).await {
            let _ = self.stop_managed_server().await;
            return Err(error);
        }

        Ok(())
    }

    async fn stop_managed_server(&self) -> RuntimeResult<()> {
        let mut process = {
            let mut state = self.server_state.lock().await;
            state.base_url = None;
            state.process.take()
        };

        if let Some(child) = process.as_mut() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        Ok(())
    }

    async fn wait_for_server_health(&self, base_url: &str) -> RuntimeResult<()> {
        let health_url = format!("{base_url}/health");
        let started = tokio::time::Instant::now();
        while started.elapsed() <= self.config.server_startup_timeout {
            let check = self.client.get(health_url.as_str()).send().await;
            if let Ok(response) = check {
                if response.status().is_success() {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Err(RuntimeError::DependencyUnavailable(format!(
            "{:?} harness server failed health check at {health_url} within {:?}",
            self.backend_kind, self.config.server_startup_timeout
        )))
    }

    async fn create_remote_session(
        &self,
        base_url: &str,
        spec: &SpawnSpec,
        instruction_prelude: Option<String>,
    ) -> RuntimeResult<CreateSessionResponse> {
        let request = CreateSessionRequest {
            runtime_session_id: spec.session_id.as_str().to_owned(),
            workdir: spec.workdir.to_string_lossy().into_owned(),
            model: spec.model.clone(),
            instruction_prelude,
            environment: spec.environment.clone(),
        };

        let response = self
            .client
            .post(format!("{base_url}/v1/sessions"))
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                RuntimeError::Process(format!(
                    "{:?} session create request failed: {error}",
                    self.backend_kind
                ))
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = sanitize_error_body(response.text().await.unwrap_or_default().as_str());
            return Err(RuntimeError::Process(format!(
                "{:?} session create failed with status {status}: {body}",
                self.backend_kind
            )));
        }

        let body = response.text().await.map_err(|error| {
            RuntimeError::Protocol(format!(
                "{:?} session create response body read failed: {error}",
                self.backend_kind
            ))
        })?;
        parse_create_session_response_body(body.as_str()).ok_or_else(|| {
            RuntimeError::Protocol(format!(
                "{:?} session create response parse failed: unsupported shape; body: {}",
                self.backend_kind,
                sanitize_error_body(body.as_str())
            ))
        })
    }

    async fn send_remote_input(
        &self,
        base_url: &str,
        remote_session_id: &str,
        input: &[u8],
    ) -> RuntimeResult<()> {
        let request = SendInputRequest {
            input: String::from_utf8_lossy(input).into_owned(),
        };
        let response = self
            .client
            .post(format!("{base_url}/v1/sessions/{remote_session_id}/input"))
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                RuntimeError::Process(format!(
                    "{:?} send-input request failed: {error}",
                    self.backend_kind
                ))
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = sanitize_error_body(response.text().await.unwrap_or_default().as_str());
            Err(RuntimeError::Process(format!(
                "{:?} send-input failed with status {status}: {body}",
                self.backend_kind
            )))
        }
    }

    async fn close_remote_session(
        &self,
        base_url: &str,
        remote_session_id: &str,
    ) -> RuntimeResult<()> {
        let response = self
            .client
            .delete(format!("{base_url}/v1/sessions/{remote_session_id}"))
            .send()
            .await
            .map_err(|error| {
                RuntimeError::Process(format!(
                    "{:?} session close request failed: {error}",
                    self.backend_kind
                ))
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = sanitize_error_body(response.text().await.unwrap_or_default().as_str());
            Err(RuntimeError::Process(format!(
                "{:?} session close failed with status {status}: {body}",
                self.backend_kind
            )))
        }
    }

    fn spawn_event_relay_task(
        client: reqwest::Client,
        backend_kind: BackendKind,
        base_url: String,
        remote_session_id: String,
        session: Arc<OpenCodeSession>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let stream_response = client
                .get(format!("{base_url}/v1/sessions/{remote_session_id}/events"))
                .send()
                .await;
            let response = match stream_response {
                Ok(response) => response,
                Err(error) => {
                    session.emit_terminal_event(BackendEvent::Crashed(BackendCrashedEvent {
                        reason: format!("{backend_kind:?} stream request failed: {error}"),
                    }));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                session.emit_terminal_event(BackendEvent::Crashed(BackendCrashedEvent {
                    reason: format!("{backend_kind:?} stream failed with status {status}: {body}"),
                }));
                return;
            }

            let mut bytes_stream = response.bytes_stream();
            let mut line_buffer = Vec::new();
            while let Some(chunk_result) = bytes_stream.next().await {
                let chunk = match chunk_result {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        session.emit_terminal_event(BackendEvent::Crashed(BackendCrashedEvent {
                            reason: format!("{backend_kind:?} stream read failed: {error}"),
                        }));
                        return;
                    }
                };

                line_buffer.extend_from_slice(&chunk);
                while let Some(newline_index) = line_buffer.iter().position(|byte| *byte == b'\n') {
                    let mut line = line_buffer.drain(..=newline_index).collect::<Vec<_>>();
                    if matches!(line.last(), Some(b'\n')) {
                        line.pop();
                    }
                    if matches!(line.last(), Some(b'\r')) {
                        line.pop();
                    }

                    maybe_log_raw_harness_line(
                        session.harness_log_raw_events,
                        backend_kind.clone(),
                        remote_session_id.as_str(),
                        line.as_slice(),
                    );

                    if let Some(event) = parse_server_event_line(&line) {
                        if matches!(event, BackendEvent::Done(_) | BackendEvent::Crashed(_)) {
                            session.emit_terminal_event(event);
                        } else {
                            session.emit_non_terminal_event(event);
                        }
                    }
                }
            }

            if !line_buffer.is_empty() {
                maybe_log_raw_harness_line(
                    session.harness_log_raw_events,
                    backend_kind.clone(),
                    remote_session_id.as_str(),
                    line_buffer.as_slice(),
                );
                if let Some(event) = parse_server_event_line(&line_buffer) {
                    if matches!(event, BackendEvent::Done(_) | BackendEvent::Crashed(_)) {
                        session.emit_terminal_event(event);
                    } else {
                        session.emit_non_terminal_event(event);
                    }
                }
            }

            session.emit_terminal_event(BackendEvent::Done(BackendDoneEvent { summary: None }));
        })
    }
}

fn detect_optional_capabilities(binary: &Path) -> BackendCapabilities {
    static CAPABILITIES_BY_BINARY: OnceLock<Mutex<HashMap<PathBuf, BackendCapabilities>>> =
        OnceLock::new();
    let cache = CAPABILITIES_BY_BINARY.get_or_init(Default::default);

    if let Ok(cache) = cache.lock() {
        if let Some(capabilities) = cache.get(binary) {
            return capabilities.clone();
        }
    }

    let mut capabilities = BackendCapabilities {
        structured_events: true,
        session_export: false,
        diff_provider: false,
        supports_background: true,
    };

    let help_text = match query_binary_help_text(binary) {
        Some(text) => text,
        None => return capabilities,
    };

    let features = parse_optional_features_from_help(&help_text);
    capabilities.session_export = features.0;
    capabilities.diff_provider = features.1;

    if let Ok(mut cache) = cache.lock() {
        cache.insert(binary.to_path_buf(), capabilities.clone());
    }

    capabilities
}

fn query_binary_help_text(binary: &Path) -> Option<String> {
    if validate_command_binary_path(binary, false).is_err() {
        return None;
    }

    for arg in OPTIONAL_CAPABILITY_HELP_ARGS {
        let output = SyncCommand::new(binary)
            .arg(arg)
            .output()
            .ok()
            .and_then(|result| {
                if result.stdout.is_empty() && result.stderr.is_empty() {
                    None
                } else {
                    Some(result)
                }
            })?;

        let combined = String::from_utf8_lossy(&output.stdout)
            .into_owned()
            .chars()
            .chain(String::from_utf8_lossy(&output.stderr).chars())
            .collect::<String>();
        if !combined.trim().is_empty() {
            return Some(combined);
        }
    }

    None
}

fn parse_optional_features_from_help(help_text: &str) -> (bool, bool) {
    let normalized = help_text.to_ascii_lowercase();

    let has_marker = |markers: &[&str]| markers.iter().any(|marker| normalized.contains(*marker));

    (
        has_marker(SESSION_EXPORT_HELP_MARKERS),
        has_marker(DIFF_PROVIDER_HELP_MARKERS),
    )
}

#[derive(Debug, Serialize)]
struct CreateSessionRequest {
    runtime_session_id: String,
    workdir: String,
    model: Option<String>,
    instruction_prelude: Option<String>,
    environment: Vec<(String, String)>,
}

struct CreateSessionResponse {
    session_id: String,
}

#[derive(Debug, Serialize)]
struct SendInputRequest {
    input: String,
}

#[derive(Debug, Deserialize)]
struct ServerEventLine {
    #[serde(rename = "type", default)]
    event_type: Option<String>,
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    bytes: Option<Vec<u8>>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    file_refs: Vec<String>,
    #[serde(default)]
    prompt_id: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    default_option: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    log_ref: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    artifact_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

fn parse_server_event_line(line: &[u8]) -> Option<BackendEvent> {
    let line = std::str::from_utf8(line).ok()?.trim();
    if line.is_empty() {
        return None;
    }

    let event: ServerEventLine = serde_json::from_str(line).ok()?;
    let event_type = event.event_type.unwrap_or_default().to_ascii_lowercase();
    match event_type.as_str() {
        "output" => {
            let stream = match event
                .stream
                .as_deref()
                .unwrap_or("stdout")
                .to_ascii_lowercase()
                .as_str()
            {
                "stderr" => BackendOutputStream::Stderr,
                _ => BackendOutputStream::Stdout,
            };
            let bytes = event
                .bytes
                .or_else(|| event.text.map(|value| value.into_bytes()))
                .unwrap_or_default();
            Some(BackendEvent::Output(BackendOutputEvent { stream, bytes }))
        }
        "checkpoint" => Some(BackendEvent::Checkpoint(BackendCheckpointEvent {
            summary: event.summary.unwrap_or_else(|| "checkpoint".to_owned()),
            detail: event.detail,
            file_refs: event.file_refs,
        })),
        "needs_input" => {
            let prompt_id = event.prompt_id.unwrap_or_else(|| "prompt".to_owned());
            let question = event
                .question
                .unwrap_or_else(|| "input required".to_owned());
            let options = event.options;
            let question_options = (!options.is_empty()).then(|| {
                options
                    .iter()
                    .map(|label| BackendNeedsInputOption {
                        label: label.clone(),
                        description: String::new(),
                    })
                    .collect::<Vec<_>>()
            });
            let questions = vec![BackendNeedsInputQuestion {
                id: prompt_id.clone(),
                header: "Input".to_owned(),
                question: question.clone(),
                is_other: false,
                is_secret: false,
                options: question_options,
            }];

            Some(BackendEvent::NeedsInput(BackendNeedsInputEvent {
                prompt_id,
                question,
                options,
                default_option: event.default_option,
                questions,
            }))
        }
        "blocked" => Some(BackendEvent::Blocked(BackendBlockedEvent {
            reason: event.reason.unwrap_or_else(|| "blocked".to_owned()),
            hint: event.hint,
            log_ref: event.log_ref,
        })),
        "artifact" => Some(BackendEvent::Artifact(BackendArtifactEvent {
            kind: parse_artifact_kind(event.kind.as_deref().unwrap_or("link")),
            artifact_id: event.artifact_id.map(RuntimeArtifactId::new),
            label: event.label,
            uri: event.uri,
        })),
        "done" => Some(BackendEvent::Done(BackendDoneEvent {
            summary: event.summary,
        })),
        "crashed" => Some(BackendEvent::Crashed(BackendCrashedEvent {
            reason: event.reason.unwrap_or_else(|| "session crashed".to_owned()),
        })),
        _ => None,
    }
}

fn parse_create_session_response_body(body: &str) -> Option<CreateSessionResponse> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let session_id = extract_session_id(&value)?;
        return Some(CreateSessionResponse { session_id });
    }

    if trimmed.contains(char::is_whitespace) {
        return None;
    }

    Some(CreateSessionResponse {
        session_id: trimmed.to_owned(),
    })
}

fn extract_session_id(value: &Value) -> Option<String> {
    extract_named_string(value, &["session_id", "sessionId", "id"])
        .or_else(|| extract_named_string(value, &["thread_id", "threadId"]))
}

fn extract_named_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = find_string_key_recursive(value, key, 6) {
            return Some(value.to_owned());
        }
    }
    None
}

fn find_string_key_recursive(value: &Value, key: &str, depth: usize) -> Option<String> {
    if depth == 0 {
        return None;
    }

    match value {
        Value::Object(map) => {
            if let Some(value) = map.get(key).and_then(Value::as_str) {
                return Some(value.to_owned());
            }
            for value in map.values() {
                if let Some(found) = find_string_key_recursive(value, key, depth - 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => {
            for value in items {
                if let Some(found) = find_string_key_recursive(value, key, depth - 1) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn sanitize_error_body(body: &str) -> String {
    let mut sanitized = body
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 240;
    if sanitized.len() > MAX_LEN {
        format!("{}...", &sanitized[..MAX_LEN])
    } else {
        sanitized
    }
}

fn maybe_log_raw_harness_line(
    enabled: bool,
    backend_kind: BackendKind,
    remote_session_id: &str,
    line: &[u8],
) {
    if !enabled {
        return;
    }
    let text = String::from_utf8_lossy(line).into_owned();
    let payload = json!({
        "backend": format!("{:?}", backend_kind),
        "remote_session_id": remote_session_id,
        "raw": sanitize_error_body(text.as_str()),
    });
    append_harness_log_line(
        harness_raw_log_path().as_path(),
        payload.to_string().as_str(),
    );
}

fn maybe_log_normalized_harness_event(
    enabled: bool,
    backend_kind: BackendKind,
    remote_session_id: &str,
    terminal: bool,
    event: &BackendEvent,
) {
    if !enabled {
        return;
    }
    let payload = match event {
        BackendEvent::Output(output) => json!({
            "backend": format!("{:?}", backend_kind),
            "remote_session_id": remote_session_id,
            "terminal": terminal,
            "event": "Output",
            "stream": format!("{:?}", output.stream),
            "text": String::from_utf8_lossy(&output.bytes).into_owned(),
        }),
        _ => json!({
            "backend": format!("{:?}", backend_kind),
            "remote_session_id": remote_session_id,
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
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| resolve_orchestrator_logs_dir().join(HARNESS_RAW_LOG_FILE_NAME))
}

fn harness_normalized_log_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
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

#[async_trait]
impl WorkerSessionControl for OpenCodeBackend {
    async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
        validate_command_binary_path(&self.config.binary, self.config.allow_unsafe_command_paths)?;
        let session_id = spec.session_id.clone();
        let base_url = self.ensure_server_base_url().await?;
        let instruction_prelude = spec.instruction_prelude.clone();
        let created = self
            .create_remote_session(&base_url, &spec, instruction_prelude)
            .await?;
        let (event_tx, _) = broadcast::channel(self.output_buffer());
        let session = Arc::new(OpenCodeSession {
            backend_kind: self.backend_kind.clone(),
            remote_session_id: created.session_id.clone(),
            harness_log_raw_events: self.config.harness_log_raw_events,
            harness_log_normalized_events: self.config.harness_log_normalized_events,
            relay_task: AsyncMutex::new(None),
            event_tx: event_tx.clone(),
            terminal_event_sent: AtomicBool::new(false),
        });

        {
            let mut sessions = self.sessions.lock().await;
            if sessions.contains_key(&session_id) {
                return Err(RuntimeError::Process(format!(
                    "worker session already exists: {}",
                    session_id.as_str()
                )));
            }
            sessions.insert(session_id.clone(), Arc::clone(&session));
        }

        let relay_task = Self::spawn_event_relay_task(
            self.client.clone(),
            self.backend_kind.clone(),
            base_url,
            created.session_id,
            Arc::clone(&session),
        );
        {
            let mut task_slot = session.relay_task.lock().await;
            *task_slot = Some(relay_task);
        }

        Ok(SessionHandle {
            session_id,
            backend: self.backend_kind.clone(),
        })
    }

    async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
        validate_session_backend(session, self.backend_kind.clone())?;
        let base_url = self.ensure_server_base_url().await?;
        let session = self.remove_session(&session.session_id).await?;
        let mut relay_task = session.relay_task.lock().await;
        if let Some(task) = relay_task.take() {
            task.abort();
        }
        drop(relay_task);
        self.close_remote_session(&base_url, &session.remote_session_id)
            .await
    }

    async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
        validate_session_backend(session, self.backend_kind.clone())?;
        let base_url = self.ensure_server_base_url().await?;
        let session = self.session(&session.session_id).await?;
        self.send_remote_input(&base_url, &session.remote_session_id, input)
            .await
    }

    async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()> {
        validate_session_backend(session, self.backend_kind.clone())?;
        let _ = session;
        let _ = (cols, rows);
        Ok(())
    }
}

fn is_bare_command_name(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn validate_command_binary_path(
    binary: &Path,
    allow_unsafe_command_paths: bool,
) -> RuntimeResult<()> {
    if allow_unsafe_command_paths || is_bare_command_name(binary) {
        return Ok(());
    }

    Err(RuntimeError::Configuration(format!(
        "OpenCode binary path '{}' is treated as unsafe by default. Use a bare command name or enable allow_unsafe_command_paths in config.toml to allow explicit paths.",
        binary.display()
    )))
}

#[async_trait]
impl WorkerSessionStreamSource for OpenCodeBackend {
    async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
        validate_session_backend(session, self.backend_kind.clone())?;
        let session = self.session(&session.session_id).await?;
        let output = session.event_tx.subscribe();
        Ok(Box::new(OpenCodeEventSubscription { output }))
    }
}

#[async_trait]
impl WorkerBackendInfo for OpenCodeBackend {
    fn kind(&self) -> BackendKind {
        self.backend_kind.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.backend_capabilities.clone()
    }

    async fn health_check(&self) -> RuntimeResult<()> {
        validate_command_binary_path(&self.config.binary, self.config.allow_unsafe_command_paths)?;
        let base_url = self.ensure_server_base_url().await?;
        self.wait_for_server_health(&base_url).await
    }
}

struct OpenCodeEventSubscription {
    output: broadcast::Receiver<BackendEvent>,
}

#[async_trait]
impl WorkerEventSubscription for OpenCodeEventSubscription {
    async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
        match self.output.recv().await {
            Ok(event) => Ok(Some(event)),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => Ok(None),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                Err(RuntimeError::Internal(format!(
                    "opencode backend stream lagged and dropped {skipped} events"
                )))
            }
        }
    }
}

fn validate_session_backend(session: &SessionHandle, expected: BackendKind) -> RuntimeResult<()> {
    if session.backend == expected {
        Ok(())
    } else {
        Err(RuntimeError::Protocol(format!(
            "received backend mismatch: expected {:?}, got {:?}",
            expected, session.backend
        )))
    }
}

fn parse_artifact_kind(kind: &str) -> BackendArtifactKind {
    let normalized = kind.trim();
    match normalized.to_ascii_lowercase().as_str() {
        "diff" => BackendArtifactKind::Diff,
        "pr" | "pull_request" | "pull-request" => BackendArtifactKind::PullRequest,
        "test" | "testrun" | "test_run" | "test-run" => BackendArtifactKind::TestRun,
        "log" | "log_snippet" | "log-snippet" => BackendArtifactKind::LogSnippet,
        "link" => BackendArtifactKind::Link,
        "session_export" | "session-export" => BackendArtifactKind::SessionExport,
        _ => BackendArtifactKind::Other(normalized.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_create_session_response_accepts_canonical_shape() {
        let body = r#"{"session_id":"sess-1","thread_id":"thread-1"}"#;
        let parsed = parse_create_session_response_body(body).expect("parse session response");
        assert_eq!(parsed.session_id, "sess-1");
    }

    #[test]
    fn parse_create_session_response_accepts_nested_codex_shape() {
        let body = r#"{"data":{"thread":{"id":"thread-abc"}}}"#;
        let parsed = parse_create_session_response_body(body).expect("parse nested response");
        assert_eq!(parsed.session_id, "thread-abc");
    }

    #[test]
    fn parse_create_session_response_accepts_plain_identifier_body() {
        let parsed =
            parse_create_session_response_body("thread-plain").expect("parse plain identifier");
        assert_eq!(parsed.session_id, "thread-plain");
    }

    #[test]
    fn parse_create_session_response_rejects_unsupported_shape() {
        assert!(parse_create_session_response_body(r#"{"ok":true}"#).is_none());
    }
}
