use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use orchestrator_runtime::{
    BackendArtifactEvent, BackendArtifactKind, BackendBlockedEvent, BackendCapabilities,
    BackendCheckpointEvent, BackendDoneEvent, BackendEvent, BackendKind, BackendNeedsInputEvent,
    BackendOutputEvent, BackendOutputStream, PtyManager, PtyOutputSubscription, PtyRenderPolicy,
    PtySpawnSpec, RuntimeArtifactId, RuntimeError, RuntimeResult, SessionHandle, SpawnSpec,
    TerminalSize, TerminalSnapshot, WorkerBackend, WorkerEventStream, WorkerEventSubscription,
};
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_OPENCODE_BINARY: &str = "opencode";
const DEFAULT_OUTPUT_BUFFER: usize = 256;
const DEFAULT_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const TAG_CHECKPOINT: &str = "@@checkpoint";
const TAG_NEEDS_INPUT: &str = "@@needs_input";
const TAG_BLOCKED: &str = "@@blocked";
const TAG_ARTIFACT: &str = "@@artifact";
const TAG_DONE: &str = "@@done";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeBackendConfig {
    pub binary: PathBuf,
    pub base_args: Vec<String>,
    pub model_flag: Option<String>,
    pub output_buffer: usize,
    pub terminal_size: TerminalSize,
    pub render_policy: PtyRenderPolicy,
    pub health_check_timeout: Duration,
}

impl Default for OpenCodeBackendConfig {
    fn default() -> Self {
        Self {
            binary: std::env::var_os("ORCHESTRATOR_OPENCODE_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_OPENCODE_BINARY)),
            base_args: Vec::new(),
            model_flag: Some("--model".to_owned()),
            output_buffer: DEFAULT_OUTPUT_BUFFER,
            terminal_size: TerminalSize::default(),
            render_policy: PtyRenderPolicy::default(),
            health_check_timeout: DEFAULT_HEALTH_CHECK_TIMEOUT,
        }
    }
}

#[derive(Clone)]
pub struct OpenCodeBackend {
    config: OpenCodeBackendConfig,
    pty_manager: PtyManager,
}

impl OpenCodeBackend {
    pub fn new(config: OpenCodeBackendConfig) -> Self {
        let pty_manager =
            PtyManager::with_render_policy(config.output_buffer, config.render_policy);
        Self {
            config,
            pty_manager,
        }
    }

    fn spawn_program(&self) -> String {
        self.config.binary.to_string_lossy().into_owned()
    }

    fn spawn_args(&self, spec: &SpawnSpec) -> Vec<String> {
        let mut args = self.config.base_args.clone();
        if let (Some(model), Some(model_flag)) = (&spec.model, &self.config.model_flag) {
            args.push(model_flag.clone());
            args.push(model.clone());
        }
        args
    }

    fn spawn_environment(&self, spec: &SpawnSpec) -> Vec<(String, String)> {
        let mut environment = spec.environment.clone();
        if let Some(instruction_prelude) = &spec.instruction_prelude {
            if !instruction_prelude.trim().is_empty() {
                environment.push((
                    "ORCHESTRATOR_INSTRUCTION_PRELUDE".to_owned(),
                    instruction_prelude.clone(),
                ));
            }
        }
        environment
    }

    async fn health_check_command(&self, args: &[OsString]) -> RuntimeResult<()> {
        let mut command = Command::new(&self.config.binary);
        command.args(args);
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());

        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RuntimeError::DependencyUnavailable(format!(
                    "OpenCode CLI `{}` was not found in PATH. Install it or set ORCHESTRATOR_OPENCODE_BIN.",
                    self.config.binary.display()
                ))
            } else {
                RuntimeError::DependencyUnavailable(format!(
                    "failed to start OpenCode CLI `{}`: {error}",
                    self.config.binary.display()
                ))
            }
        })?;

        match timeout(self.config.health_check_timeout, child.wait()).await {
            Ok(wait_result) => {
                let status = wait_result.map_err(|error| {
                    RuntimeError::DependencyUnavailable(format!(
                        "OpenCode CLI `{}` failed health check: {error}",
                        self.config.binary.display()
                    ))
                })?;
                if !status.success() {
                    return Err(RuntimeError::DependencyUnavailable(format!(
                        "OpenCode CLI `{}` exited with status {status} during health check.",
                        self.config.binary.display()
                    )));
                }
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(RuntimeError::DependencyUnavailable(format!(
                    "OpenCode CLI health check timed out after {:?} while running `{}`.",
                    self.config.health_check_timeout,
                    self.config.binary.display()
                )));
            }
        };

        Ok(())
    }
}

#[async_trait]
impl orchestrator_runtime::SessionLifecycle for OpenCodeBackend {
    async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
        let session_id = spec.session_id.clone();
        let pty_spec = PtySpawnSpec {
            session_id: session_id.clone(),
            program: self.spawn_program(),
            args: self.spawn_args(&spec),
            workdir: spec.workdir.clone(),
            environment: self.spawn_environment(&spec),
            size: self.config.terminal_size,
        };

        self.pty_manager.spawn(pty_spec).await?;

        if let Some(instruction_prelude) = spec.instruction_prelude {
            if !instruction_prelude.trim().is_empty() {
                let mut bytes = instruction_prelude.into_bytes();
                bytes.push(b'\n');
                if let Err(error) = self.pty_manager.write(&session_id, &bytes).await {
                    let _ = self.pty_manager.kill(&session_id).await;
                    return Err(error);
                }
            }
        }

        Ok(SessionHandle {
            session_id,
            backend: BackendKind::OpenCode,
        })
    }

    async fn kill(&self, session: &SessionHandle) -> RuntimeResult<()> {
        self.pty_manager.kill(&session.session_id).await
    }

    async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> RuntimeResult<()> {
        self.pty_manager.write(&session.session_id, input).await
    }

    async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> RuntimeResult<()> {
        self.pty_manager
            .resize(&session.session_id, cols, rows)
            .await
    }
}

#[async_trait]
impl WorkerBackend for OpenCodeBackend {
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
        let args = [OsString::from("--version")];
        self.health_check_command(&args).await
    }

    async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
        let output = self
            .pty_manager
            .subscribe_output(&session.session_id)
            .await?;
        Ok(Box::new(OpenCodeEventSubscription {
            output,
            parser: SupervisorProtocolParser::default(),
            pending: VecDeque::new(),
            stream_ended: false,
        }))
    }

    async fn snapshot(&self, session: &SessionHandle) -> RuntimeResult<TerminalSnapshot> {
        self.pty_manager.snapshot(&session.session_id).await
    }
}

struct OpenCodeEventSubscription {
    output: PtyOutputSubscription,
    parser: SupervisorProtocolParser,
    pending: VecDeque<BackendEvent>,
    stream_ended: bool,
}

#[async_trait]
impl WorkerEventSubscription for OpenCodeEventSubscription {
    async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        if self.stream_ended {
            return Ok(None);
        }

        match self.output.next_chunk().await? {
            Some(chunk) => {
                self.pending
                    .push_back(BackendEvent::Output(BackendOutputEvent {
                        stream: BackendOutputStream::Stdout,
                        bytes: chunk.clone(),
                    }));
                self.pending.extend(self.parser.ingest(&chunk));
                Ok(self.pending.pop_front())
            }
            None => {
                self.stream_ended = true;
                self.pending.extend(self.parser.finish());
                Ok(self.pending.pop_front())
            }
        }
    }
}

#[derive(Default)]
struct SupervisorProtocolParser {
    line_buffer: Vec<u8>,
}

impl SupervisorProtocolParser {
    fn ingest(&mut self, chunk: &[u8]) -> Vec<BackendEvent> {
        if chunk.is_empty() {
            return Vec::new();
        }

        self.line_buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(newline_index) = self.line_buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.line_buffer.drain(..=newline_index).collect::<Vec<_>>();
            if matches!(line.last(), Some(b'\n')) {
                line.pop();
            }
            if matches!(line.last(), Some(b'\r')) {
                line.pop();
            }
            if let Some(event) = parse_protocol_line(&line) {
                events.push(event);
            }
        }

        events
    }

    fn finish(&mut self) -> Vec<BackendEvent> {
        if self.line_buffer.is_empty() {
            return Vec::new();
        }

        let mut line = std::mem::take(&mut self.line_buffer);
        if matches!(line.last(), Some(b'\r')) {
            line.pop();
        }
        parse_protocol_line(&line).into_iter().collect()
    }
}

fn parse_protocol_line(line: &[u8]) -> Option<BackendEvent> {
    let line = sanitize_protocol_line(protocol_candidate_bytes(line));
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    if let Some(payload) = protocol_payload(line, TAG_CHECKPOINT) {
        if payload.is_empty() {
            return None;
        }
        let payload: CheckpointPayload = serde_json::from_str(payload).ok()?;
        let summary = payload.summary.unwrap_or_else(|| "checkpoint".to_owned());
        return Some(BackendEvent::Checkpoint(BackendCheckpointEvent {
            summary,
            detail: payload.detail,
            file_refs: payload.files,
        }));
    }

    if let Some(payload) = protocol_payload(line, TAG_NEEDS_INPUT) {
        if payload.is_empty() {
            return None;
        }
        let payload: NeedsInputPayload = serde_json::from_str(payload).ok()?;
        return Some(BackendEvent::NeedsInput(BackendNeedsInputEvent {
            prompt_id: payload.id,
            question: payload.question,
            options: payload.options,
            default_option: payload.default,
        }));
    }

    if let Some(payload) = protocol_payload(line, TAG_BLOCKED) {
        if payload.is_empty() {
            return None;
        }
        let payload: BlockedPayload = serde_json::from_str(payload).ok()?;
        return Some(BackendEvent::Blocked(BackendBlockedEvent {
            reason: payload.reason,
            hint: payload.hint,
            log_ref: payload.log_ref,
        }));
    }

    if let Some(payload) = protocol_payload(line, TAG_ARTIFACT) {
        if payload.is_empty() {
            return None;
        }
        let payload: ArtifactPayload = serde_json::from_str(payload).ok()?;
        return Some(BackendEvent::Artifact(BackendArtifactEvent {
            kind: parse_artifact_kind(&payload.kind),
            artifact_id: payload.artifact_id.map(RuntimeArtifactId::new),
            label: payload.label,
            uri: payload.uri,
        }));
    }

    if let Some(payload) = protocol_payload(line, TAG_DONE) {
        if payload.is_empty() {
            return Some(BackendEvent::Done(BackendDoneEvent { summary: None }));
        }
        let payload: DonePayload = serde_json::from_str(payload).ok()?;
        return Some(BackendEvent::Done(BackendDoneEvent {
            summary: payload.summary,
        }));
    }

    None
}

fn protocol_candidate_bytes(line: &[u8]) -> &[u8] {
    match line.iter().rposition(|byte| *byte == b'\r') {
        Some(last_carriage_return) => &line[last_carriage_return + 1..],
        None => line,
    }
}

fn sanitize_protocol_line(line: &[u8]) -> String {
    let mut out = Vec::with_capacity(line.len());
    let mut index = 0usize;

    while index < line.len() {
        match line[index] {
            0x1b => {
                index += strip_escape_sequence(&line[index..]);
            }
            byte if byte.is_ascii_control() && byte != b'\t' => {
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn strip_escape_sequence(remaining: &[u8]) -> usize {
    if remaining.len() < 2 {
        return 1;
    }

    match remaining[1] {
        b'[' => strip_csi_sequence(remaining),
        b']' => strip_osc_sequence(remaining),
        b'P' | b'X' | b'^' | b'_' => strip_string_sequence(remaining),
        _ => strip_fe_escape_sequence(remaining),
    }
}

fn strip_csi_sequence(remaining: &[u8]) -> usize {
    for (offset, byte) in remaining.iter().enumerate().skip(2) {
        if (0x40..=0x7e).contains(byte) {
            return offset + 1;
        }
    }
    remaining.len()
}

fn strip_osc_sequence(remaining: &[u8]) -> usize {
    strip_bel_or_st_terminated_sequence(remaining)
}

fn strip_string_sequence(remaining: &[u8]) -> usize {
    strip_bel_or_st_terminated_sequence(remaining)
}

fn strip_bel_or_st_terminated_sequence(remaining: &[u8]) -> usize {
    for (offset, byte) in remaining.iter().enumerate().skip(2) {
        if *byte == 0x07 {
            return offset + 1;
        }
        if *byte == 0x1b && remaining.get(offset + 1) == Some(&b'\\') {
            return offset + 2;
        }
    }
    remaining.len()
}

fn strip_fe_escape_sequence(remaining: &[u8]) -> usize {
    let mut offset = 1usize;
    while offset < remaining.len() && (0x20..=0x2f).contains(&remaining[offset]) {
        offset += 1;
    }

    if offset < remaining.len() && (0x30..=0x7e).contains(&remaining[offset]) {
        offset + 1
    } else {
        remaining.len()
    }
}

fn protocol_payload<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let remainder = line.strip_prefix(tag)?;
    if remainder.is_empty() {
        return Some(remainder);
    }

    let first = remainder.chars().next()?;
    if first.is_whitespace() {
        Some(remainder.trim_start())
    } else {
        None
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

#[derive(Debug, Deserialize)]
struct CheckpointPayload {
    #[serde(default, alias = "stage")]
    summary: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default, alias = "file_refs")]
    files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct NeedsInputPayload {
    #[serde(alias = "prompt_id")]
    id: String,
    #[serde(alias = "prompt")]
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default, alias = "default_option")]
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlockedPayload {
    reason: String,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    log_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArtifactPayload {
    kind: String,
    #[serde(default, alias = "id")]
    artifact_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default, alias = "url")]
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DonePayload {
    #[serde(default)]
    summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use orchestrator_runtime::{BackendEvent, RuntimeSessionId, SessionLifecycle};
    use tokio::time::timeout;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    #[cfg(unix)]
    fn shell_program() -> &'static str {
        "sh"
    }

    #[cfg(windows)]
    fn shell_program() -> &'static str {
        "cmd"
    }

    #[cfg(unix)]
    fn shell_args(script: &str) -> Vec<String> {
        vec!["-lc".to_owned(), script.to_owned()]
    }

    #[cfg(windows)]
    fn shell_args(script: &str) -> Vec<String> {
        vec!["/C".to_owned(), script.to_owned()]
    }

    #[cfg(unix)]
    fn interactive_echo_script() -> &'static str {
        "printf 'ready\\n'; read line; printf 'echo:%s\\n' \"$line\"; sleep 5"
    }

    #[cfg(windows)]
    fn interactive_echo_script() -> &'static str {
        "echo ready && set /p line= && echo echo:%line% && ping -n 6 127.0.0.1 >nul"
    }

    #[cfg(unix)]
    fn tagged_script() -> &'static str {
        r#"printf '@@checkpoint {"stage":"implementing","detail":"updating parser","files":["src/lib.rs"]}\n';
printf '@@needs_input {"id":"q1","question":"Choose API shape","options":["A","B"],"default":"A"}\n';
printf '@@artifact {"kind":"pr","url":"https://example.test/pulls/12","label":"Draft PR"}\n';
printf '@@blocked {"reason":"tests failing","hint":"run cargo test"}\n';
printf '@@done {"summary":"complete"}\n';
sleep 1"#
    }

    #[cfg(windows)]
    fn tagged_script() -> &'static str {
        "echo @@checkpoint {\"stage\":\"implementing\",\"detail\":\"updating parser\",\"files\":[\"src/lib.rs\"]} && echo @@needs_input {\"id\":\"q1\",\"question\":\"Choose API shape\",\"options\":[\"A\",\"B\"],\"default\":\"A\"} && echo @@artifact {\"kind\":\"pr\",\"url\":\"https://example.test/pulls/12\",\"label\":\"Draft PR\"} && echo @@blocked {\"reason\":\"tests failing\",\"hint\":\"run cargo test\"} && echo @@done {\"summary\":\"complete\"}"
    }

    #[cfg(unix)]
    fn long_running_script() -> &'static str {
        "sleep 5"
    }

    #[cfg(windows)]
    fn long_running_script() -> &'static str {
        "ping -n 6 127.0.0.1 >nul"
    }

    fn os_args(args: Vec<String>) -> Vec<OsString> {
        args.into_iter().map(OsString::from).collect()
    }

    fn test_backend(program: &str, args: Vec<String>) -> OpenCodeBackend {
        let config = OpenCodeBackendConfig {
            binary: PathBuf::from(program),
            base_args: args,
            model_flag: None,
            output_buffer: 128,
            terminal_size: TerminalSize::default(),
            render_policy: PtyRenderPolicy::default(),
            health_check_timeout: Duration::from_secs(1),
        };
        OpenCodeBackend::new(config)
    }

    fn spawn_spec(session_id: &str) -> SpawnSpec {
        SpawnSpec {
            session_id: RuntimeSessionId::new(session_id),
            workdir: std::env::current_dir().expect("resolve current dir"),
            model: None,
            instruction_prelude: None,
            environment: Vec::new(),
        }
    }

    async fn collect_output_until(
        stream: &mut WorkerEventStream,
        needle: &str,
    ) -> RuntimeResult<String> {
        timeout(TEST_TIMEOUT, async {
            let mut output = Vec::new();
            loop {
                match stream.next_event().await? {
                    Some(BackendEvent::Output(event)) => {
                        output.extend_from_slice(&event.bytes);
                        if String::from_utf8_lossy(&output).contains(needle) {
                            return Ok(String::from_utf8_lossy(&output).to_string());
                        }
                    }
                    Some(_) => {}
                    None => return Ok(String::from_utf8_lossy(&output).to_string()),
                }
            }
        })
        .await
        .map_err(|_| RuntimeError::Internal("timed out waiting for output".to_owned()))?
    }

    #[tokio::test]
    async fn parser_extracts_events_from_chunked_lines() {
        let mut parser = SupervisorProtocolParser::default();
        let events_one = parser.ingest(
            b"@@checkpoint {\"stage\":\"impl\",\"detail\":\"x\",\"files\":[\"a.rs\"]}\n@@needs_input ",
        );
        let events_two = parser.ingest(
            b"{\"id\":\"q1\",\"question\":\"pick\",\"options\":[\"A\",\"B\"],\"default\":\"B\"}\n",
        );
        let events_three = parser.finish();

        assert_eq!(events_one.len(), 1);
        assert!(matches!(events_one[0], BackendEvent::Checkpoint(_)));
        assert_eq!(events_two.len(), 1);
        assert!(matches!(events_two[0], BackendEvent::NeedsInput(_)));
        assert!(events_three.is_empty());
    }

    #[tokio::test]
    async fn parser_parses_all_supervisor_protocol_tags() {
        let checkpoint = parse_protocol_line(
            br#"@@checkpoint {"stage":"implementing","detail":"parser update","files":["src/lib.rs"]}"#,
        )
        .expect("checkpoint tag should parse");
        assert!(matches!(
            checkpoint,
            BackendEvent::Checkpoint(BackendCheckpointEvent {
                summary,
                detail: Some(detail),
                file_refs
            }) if summary == "implementing" && detail == "parser update" && file_refs == vec!["src/lib.rs"]
        ));

        let needs_input = parse_protocol_line(
            br#"@@needs_input {"id":"q1","question":"Choose A or B","options":["A","B"],"default":"A"}"#,
        )
        .expect("needs_input tag should parse");
        assert!(matches!(
            needs_input,
            BackendEvent::NeedsInput(BackendNeedsInputEvent {
                prompt_id,
                question,
                options,
                default_option: Some(default_option),
            }) if prompt_id == "q1" && question == "Choose A or B" && options == vec!["A", "B"] && default_option == "A"
        ));

        let blocked =
            parse_protocol_line(br#"@@blocked {"reason":"tests failing","hint":"rerun unit tests","log_ref":"artifact:log:1"}"#)
                .expect("blocked tag should parse");
        assert!(matches!(
            blocked,
            BackendEvent::Blocked(BackendBlockedEvent {
                reason,
                hint: Some(hint),
                log_ref: Some(log_ref),
            }) if reason == "tests failing" && hint == "rerun unit tests" && log_ref == "artifact:log:1"
        ));

        let artifact = parse_protocol_line(
            br#"@@artifact {"kind":"pr","id":"artifact-1","label":"Draft PR","url":"https://example.test/pulls/1"}"#,
        )
        .expect("artifact tag should parse");
        assert!(matches!(
            artifact,
            BackendEvent::Artifact(BackendArtifactEvent {
                kind: BackendArtifactKind::PullRequest,
                artifact_id: Some(ref artifact_id),
                label: Some(ref label),
                uri: Some(ref uri),
            }) if artifact_id.as_str() == "artifact-1" && label == "Draft PR" && uri == "https://example.test/pulls/1"
        ));

        let done = parse_protocol_line(br#"@@done {"summary":"all complete"}"#)
            .expect("done tag should parse");
        assert!(matches!(
            done,
            BackendEvent::Done(BackendDoneEvent {
                summary: Some(summary)
            }) if summary == "all complete"
        ));
    }

    #[tokio::test]
    async fn parser_accepts_done_tag_without_payload() {
        let event = parse_protocol_line(b"@@done").expect("done tag without payload");
        assert!(matches!(
            event,
            BackendEvent::Done(BackendDoneEvent { summary: None })
        ));
    }

    #[tokio::test]
    async fn parser_handles_ansi_wrapped_protocol_line() {
        let line = b"\x1b[32m@@checkpoint {\"stage\":\"implementing\",\"detail\":\"ansi\",\"files\":[]}\x1b[0m";
        let event = parse_protocol_line(line).expect("checkpoint tag wrapped in ANSI should parse");
        assert!(matches!(
            event,
            BackendEvent::Checkpoint(BackendCheckpointEvent { summary, .. }) if summary == "implementing"
        ));
    }

    #[tokio::test]
    async fn parser_handles_carriage_return_rewrite_before_protocol_tag() {
        let line = b"progress 25%\r@@checkpoint {\"stage\":\"implementing\",\"detail\":\"rewrite\",\"files\":[]}";
        let event =
            parse_protocol_line(line).expect("checkpoint tag after carriage return should parse");
        assert!(matches!(
            event,
            BackendEvent::Checkpoint(BackendCheckpointEvent { summary, .. }) if summary == "implementing"
        ));
    }

    #[tokio::test]
    async fn parser_handles_escape_sequence_with_intermediate_bytes() {
        let line = b"\x1b(B@@done {\"summary\":\"complete\"}";
        let event = parse_protocol_line(line).expect("done tag after ESC sequence should parse");
        assert!(matches!(
            event,
            BackendEvent::Done(BackendDoneEvent { summary: Some(summary) }) if summary == "complete"
        ));
    }

    #[tokio::test]
    async fn parser_ignores_invalid_json_payloads() {
        assert!(parse_protocol_line(br#"@@checkpoint {"stage":"x""#).is_none());
        assert!(parse_protocol_line(br#"@@needs_input not-json"#).is_none());
        assert!(parse_protocol_line(br#"@@artifact {"kind":1}"#).is_none());
        assert!(parse_protocol_line(br#"@@blocked {"reason":true}"#).is_none());
        assert!(parse_protocol_line(br#"@@done [1,2,3]"#).is_none());
    }

    #[tokio::test]
    async fn parser_normalizes_unknown_artifact_kind_values() {
        let event = parse_protocol_line(
            br#"@@artifact {"kind":"  custom-kind  ","id":"artifact-1","url":"https://example.test"}"#,
        )
        .expect("artifact tag should parse");

        assert!(matches!(
            event,
            BackendEvent::Artifact(BackendArtifactEvent {
                kind: BackendArtifactKind::Other(kind),
                ..
            }) if kind == "custom-kind"
        ));
    }

    #[tokio::test]
    async fn health_check_reports_missing_binary() {
        let backend = test_backend("orchestrator-definitely-missing-opencode", Vec::new());
        let error = backend
            .health_check()
            .await
            .expect_err("missing binary should fail health check");
        assert!(matches!(
            error,
            RuntimeError::DependencyUnavailable(message) if message.contains("not found")
        ));
    }

    #[tokio::test]
    async fn health_check_rejects_non_zero_exit_status() {
        let backend = test_backend(shell_program(), Vec::new());
        let args = os_args(shell_args("exit 7"));
        let error = backend
            .health_check_command(&args)
            .await
            .expect_err("non-zero process should fail health check");
        assert!(matches!(
            error,
            RuntimeError::DependencyUnavailable(message) if message.contains("status")
        ));
    }

    #[tokio::test]
    async fn health_check_times_out_long_running_process() {
        let config = OpenCodeBackendConfig {
            binary: PathBuf::from(shell_program()),
            base_args: Vec::new(),
            model_flag: None,
            output_buffer: 128,
            terminal_size: TerminalSize::default(),
            render_policy: PtyRenderPolicy::default(),
            health_check_timeout: Duration::from_millis(50),
        };
        let backend = OpenCodeBackend::new(config);
        let args = os_args(shell_args(long_running_script()));
        let error = backend
            .health_check_command(&args)
            .await
            .expect_err("long-running process should time out health check");
        assert!(matches!(
            error,
            RuntimeError::DependencyUnavailable(message) if message.contains("timed out")
        ));
    }

    #[tokio::test]
    async fn spawn_send_input_snapshot_and_kill_use_pty_primitives() {
        let backend = test_backend(shell_program(), shell_args(interactive_echo_script()));
        let handle = backend
            .spawn(spawn_spec("session-opencode-lifecycle"))
            .await
            .expect("spawn backend session");
        assert_eq!(handle.backend, BackendKind::OpenCode);

        let mut stream = backend
            .subscribe(&handle)
            .await
            .expect("subscribe to session");
        let initial = collect_output_until(&mut stream, "ready")
            .await
            .expect("wait for ready output");
        assert!(initial.contains("ready"));

        backend
            .send_input(&handle, b"hello\n")
            .await
            .expect("send input");
        let echoed = collect_output_until(&mut stream, "echo:hello")
            .await
            .expect("wait for echoed output");
        assert!(echoed.contains("echo:hello"));

        backend.resize(&handle, 120, 40).await.expect("resize pty");

        let snapshot = backend.snapshot(&handle).await.expect("take snapshot");
        assert!(snapshot
            .lines
            .iter()
            .any(|line| line.contains("echo:hello")));

        backend.kill(&handle).await.expect("kill session");
        let error = backend
            .snapshot(&handle)
            .await
            .expect_err("killed session should not be available");
        assert!(matches!(error, RuntimeError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn subscribe_parses_supervisor_protocol_tags() {
        let backend = test_backend(shell_program(), shell_args(tagged_script()));
        let handle = backend
            .spawn(spawn_spec("session-opencode-tags"))
            .await
            .expect("spawn tagged session");
        let mut stream = backend.subscribe(&handle).await.expect("subscribe to tags");

        let events = timeout(TEST_TIMEOUT, async {
            let mut events = Vec::new();
            loop {
                match stream.next_event().await.expect("read backend event") {
                    Some(event) => {
                        let stop = matches!(event, BackendEvent::Done(_));
                        events.push(event);
                        if stop {
                            return events;
                        }
                    }
                    None => return events,
                }
            }
        })
        .await
        .expect("collect protocol events");

        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::Checkpoint(BackendCheckpointEvent { summary, .. }) if summary == "implementing"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::NeedsInput(BackendNeedsInputEvent { prompt_id, .. }) if prompt_id == "q1"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::Artifact(BackendArtifactEvent {
                kind: BackendArtifactKind::PullRequest,
                ..
            })
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::Blocked(BackendBlockedEvent { reason, .. }) if reason == "tests failing"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::Done(BackendDoneEvent { summary: Some(value) }) if value == "complete"
        )));
    }
}
