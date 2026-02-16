use std::collections::{hash_map::Entry, HashMap};
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task;

use crate::{
    terminal_emulator::TerminalEmulator, RuntimeError, RuntimeResult, RuntimeSessionId,
    TerminalSnapshot,
};

const DEFAULT_OUTPUT_BUFFER: usize = 256;
const DEFAULT_SCROLLBACK_LIMIT: usize = 4_000;
const MAX_SCROLLBACK_LIMIT: usize = 128_000;
const DEFAULT_OUTPUT_COALESCE_WINDOW_MS: u64 = 16;
const DEFAULT_OUTPUT_COALESCE_MAX_BYTES: usize = 64 * 1024;
const READ_CHUNK_SIZE: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySpawnSpec {
    pub session_id: RuntimeSessionId,
    pub program: String,
    pub args: Vec<String>,
    pub workdir: PathBuf,
    pub environment: Vec<(String, String)>,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyRenderPolicy {
    pub scrollback_limit: usize,
    pub output_coalesce_window_ms: u64,
    pub output_coalesce_max_bytes: usize,
}

impl Default for PtyRenderPolicy {
    fn default() -> Self {
        Self {
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            output_coalesce_window_ms: DEFAULT_OUTPUT_COALESCE_WINDOW_MS,
            output_coalesce_max_bytes: DEFAULT_OUTPUT_COALESCE_MAX_BYTES,
        }
    }
}

impl PtyRenderPolicy {
    fn normalized(self) -> Self {
        Self {
            scrollback_limit: self.scrollback_limit.clamp(1, MAX_SCROLLBACK_LIMIT),
            output_coalesce_window_ms: self.output_coalesce_window_ms,
            output_coalesce_max_bytes: self.output_coalesce_max_bytes.max(1),
        }
    }

    fn output_coalesce_window(self) -> Option<Duration> {
        if self.output_coalesce_window_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(self.output_coalesce_window_ms))
        }
    }
}

struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    emulator: Mutex<TerminalEmulator>,
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    output_tx: broadcast::Sender<Vec<u8>>,
}

enum SessionState {
    Starting,
    Running(Arc<PtySession>),
}

struct SpawnedPty {
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Clone)]
pub struct PtyManager {
    sessions: Arc<RwLock<HashMap<RuntimeSessionId, SessionState>>>,
    output_buffer: usize,
    render_policy: PtyRenderPolicy,
}

pub struct PtyOutputSubscription {
    receiver: broadcast::Receiver<Vec<u8>>,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self::new(DEFAULT_OUTPUT_BUFFER)
    }
}

impl PtyManager {
    pub fn new(output_buffer: usize) -> Self {
        Self::with_render_policy(output_buffer, PtyRenderPolicy::default())
    }

    pub fn with_scrollback(output_buffer: usize, scrollback_limit: usize) -> Self {
        Self::with_render_policy(
            output_buffer,
            PtyRenderPolicy {
                scrollback_limit,
                ..PtyRenderPolicy::default()
            },
        )
    }

    pub fn with_render_policy(output_buffer: usize, render_policy: PtyRenderPolicy) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            output_buffer: output_buffer.max(1),
            render_policy: render_policy.normalized(),
        }
    }

    pub async fn spawn(&self, spec: PtySpawnSpec) -> RuntimeResult<()> {
        if spec.program.trim().is_empty() {
            return Err(RuntimeError::Configuration(
                "PTY spawn program must not be empty".to_owned(),
            ));
        }
        if spec.size.cols == 0 || spec.size.rows == 0 {
            return Err(RuntimeError::Configuration(
                "PTY size must have non-zero rows and columns".to_owned(),
            ));
        }

        let session_id = spec.session_id.clone();
        let initial_size = spec.size;
        self.reserve_session(&session_id)?;

        let spawned = match task::spawn_blocking(move || spawn_pty_process(spec)).await {
            Ok(Ok(spawned)) => spawned,
            Ok(Err(error)) => {
                let _ = self.remove_session_entry(&session_id);
                return Err(error);
            }
            Err(error) => {
                let _ = self.remove_session_entry(&session_id);
                return Err(RuntimeError::Internal(format!(
                    "PTY spawn task failed: {error}",
                )));
            }
        };

        let emulator = match TerminalEmulator::new(
            initial_size.cols,
            initial_size.rows,
            self.render_policy.scrollback_limit,
        ) {
            Ok(emulator) => emulator,
            Err(error) => {
                let _ = self.remove_session_entry(&session_id);
                terminate_child(spawned.child);
                return Err(error);
            }
        };
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel();
        let (output_tx, _) = broadcast::channel(self.output_buffer);
        let session = Arc::new(PtySession {
            master: Mutex::new(spawned.master),
            emulator: Mutex::new(emulator),
            stdin_tx,
            output_tx: output_tx.clone(),
        });

        if let Err(error) = self.install_session(session_id.clone(), Arc::clone(&session)) {
            let _ = self.remove_session_entry(&session_id);
            terminate_child(spawned.child);
            return Err(error);
        }

        spawn_read_loop(spawned.reader, output_tx, session, self.render_policy);
        spawn_write_loop(spawned.writer, stdin_rx);
        spawn_child_wait_loop(Arc::clone(&self.sessions), session_id, spawned.child);
        Ok(())
    }

    pub async fn subscribe_output(
        &self,
        session_id: &RuntimeSessionId,
    ) -> RuntimeResult<PtyOutputSubscription> {
        let session = self.session(session_id)?;
        Ok(PtyOutputSubscription {
            receiver: session.output_tx.subscribe(),
        })
    }

    pub async fn write(&self, session_id: &RuntimeSessionId, input: &[u8]) -> RuntimeResult<()> {
        let session = self.session(session_id)?;
        session.stdin_tx.send(input.to_vec()).map_err(|_| {
            RuntimeError::Process("PTY stdin writer is no longer available".to_owned())
        })
    }

    pub async fn resize(
        &self,
        session_id: &RuntimeSessionId,
        cols: u16,
        rows: u16,
    ) -> RuntimeResult<()> {
        if cols == 0 || rows == 0 {
            return Err(RuntimeError::Configuration(
                "PTY resize requires non-zero rows and columns".to_owned(),
            ));
        }

        let session = self.session(session_id)?;
        let master = session
            .master
            .lock()
            .map_err(|_| RuntimeError::Internal("PTY master lock poisoned".to_owned()))?;
        master
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(process_error)?;
        let mut emulator = session
            .emulator
            .lock()
            .map_err(|_| RuntimeError::Internal("terminal emulator lock poisoned".to_owned()))?;
        emulator.resize(cols, rows)
    }

    pub async fn snapshot(&self, session_id: &RuntimeSessionId) -> RuntimeResult<TerminalSnapshot> {
        let session = self.session(session_id)?;
        let emulator = session
            .emulator
            .lock()
            .map_err(|_| RuntimeError::Internal("terminal emulator lock poisoned".to_owned()))?;
        Ok(emulator.snapshot())
    }

    fn session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<Arc<PtySession>> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| RuntimeError::Internal("PTY session map lock poisoned".to_owned()))?;
        match sessions.get(session_id) {
            Some(SessionState::Running(session)) => Ok(Arc::clone(session)),
            Some(SessionState::Starting) => Err(RuntimeError::Process(format!(
                "PTY session is still starting: {}",
                session_id.as_str()
            ))),
            None => Err(RuntimeError::SessionNotFound(
                session_id.as_str().to_owned(),
            )),
        }
    }

    fn reserve_session(&self, session_id: &RuntimeSessionId) -> RuntimeResult<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| RuntimeError::Internal("PTY session map lock poisoned".to_owned()))?;

        match sessions.entry(session_id.clone()) {
            Entry::Occupied(_) => Err(RuntimeError::Configuration(format!(
                "PTY session already exists: {}",
                session_id.as_str()
            ))),
            Entry::Vacant(entry) => {
                entry.insert(SessionState::Starting);
                Ok(())
            }
        }
    }

    fn install_session(
        &self,
        session_id: RuntimeSessionId,
        session: Arc<PtySession>,
    ) -> RuntimeResult<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| RuntimeError::Internal("PTY session map lock poisoned".to_owned()))?;

        match sessions.entry(session_id) {
            Entry::Occupied(mut entry) => {
                if matches!(entry.get(), SessionState::Starting) {
                    entry.insert(SessionState::Running(session));
                    Ok(())
                } else {
                    Err(RuntimeError::Configuration(
                        "PTY session unexpectedly replaced while starting".to_owned(),
                    ))
                }
            }
            Entry::Vacant(_) => Err(RuntimeError::Internal(
                "PTY session reservation missing after spawn".to_owned(),
            )),
        }
    }

    fn remove_session_entry(&self, session_id: &RuntimeSessionId) -> RuntimeResult<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| RuntimeError::Internal("PTY session map lock poisoned".to_owned()))?;
        sessions.remove(session_id);
        Ok(())
    }
}

impl PtyOutputSubscription {
    pub async fn next_chunk(&mut self) -> RuntimeResult<Option<Vec<u8>>> {
        match self.receiver.recv().await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(broadcast::error::RecvError::Closed) => Ok(None),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Err(RuntimeError::Process(
                format!("PTY output subscriber lagged; dropped {skipped} chunks"),
            )),
        }
    }
}

fn to_pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        cols: size.cols,
        rows: size.rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn process_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Process(error.to_string())
}

fn spawn_pty_process(spec: PtySpawnSpec) -> RuntimeResult<SpawnedPty> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(to_pty_size(spec.size))
        .map_err(process_error)?;

    let mut command = CommandBuilder::new(spec.program);
    command.cwd(spec.workdir);
    for arg in spec.args {
        command.arg(arg);
    }
    for (key, value) in spec.environment {
        command.env(key, value);
    }

    let child = pair.slave.spawn_command(command).map_err(process_error)?;
    drop(pair.slave);

    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(child);
            return Err(process_error(error));
        }
    };

    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(error) => {
            terminate_child(child);
            return Err(process_error(error));
        }
    };

    Ok(SpawnedPty {
        master: pair.master,
        reader,
        writer,
        child,
    })
}

fn terminate_child(mut child: Box<dyn Child + Send + Sync>) {
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_read_loop(
    mut reader: Box<dyn Read + Send>,
    output_tx: broadcast::Sender<Vec<u8>>,
    session: Arc<PtySession>,
    policy: PtyRenderPolicy,
) {
    let (coalesce_tx, coalesce_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || run_output_coalescer(coalesce_rx, output_tx, policy));

    std::thread::spawn(move || {
        let mut buffer = [0_u8; READ_CHUNK_SIZE];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if let Ok(mut emulator) = session.emulator.lock() {
                        emulator.process(&buffer[..read]);
                    }
                    if coalesce_tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

fn run_output_coalescer(
    output_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    output_tx: broadcast::Sender<Vec<u8>>,
    policy: PtyRenderPolicy,
) {
    let policy = policy.normalized();
    let Some(window) = policy.output_coalesce_window() else {
        while let Ok(chunk) = output_rx.recv() {
            if !chunk.is_empty() {
                let _ = output_tx.send(chunk);
            }
        }
        return;
    };

    let mut pending = Vec::new();
    loop {
        if pending.is_empty() {
            match output_rx.recv() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    pending.extend_from_slice(&chunk);
                    if pending.len() >= policy.output_coalesce_max_bytes {
                        let _ = output_tx.send(std::mem::take(&mut pending));
                        continue;
                    }
                }
                Err(_) => break,
            }
        }

        let now = Instant::now();
        let Some(window_deadline) = now.checked_add(window) else {
            let _ = output_tx.send(std::mem::take(&mut pending));
            continue;
        };
        loop {
            if pending.len() >= policy.output_coalesce_max_bytes {
                let _ = output_tx.send(std::mem::take(&mut pending));
                break;
            }

            let now = Instant::now();
            if now >= window_deadline {
                let _ = output_tx.send(std::mem::take(&mut pending));
                break;
            }

            let remaining = window_deadline.saturating_duration_since(now);
            match output_rx.recv_timeout(remaining) {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    pending.extend_from_slice(&chunk);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let _ = output_tx.send(std::mem::take(&mut pending));
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if !pending.is_empty() {
                        let _ = output_tx.send(pending);
                    }
                    return;
                }
            }
        }
    }
}

fn spawn_write_loop(
    mut writer: Box<dyn Write + Send>,
    mut stdin_rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    std::thread::spawn(move || {
        while let Some(input) = stdin_rx.blocking_recv() {
            if input.is_empty() {
                continue;
            }
            if writer.write_all(&input).is_err() {
                break;
            }
            if writer.flush().is_err() {
                break;
            }
        }
    });
}

fn spawn_child_wait_loop(
    sessions: Arc<RwLock<HashMap<RuntimeSessionId, SessionState>>>,
    session_id: RuntimeSessionId,
    mut child: Box<dyn Child + Send + Sync>,
) {
    std::thread::spawn(move || {
        let _ = child.wait();
        if let Ok(mut guard) = sessions.write() {
            guard.remove(&session_id);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::{sleep, timeout};

    use super::*;

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
        "printf 'ready\\n'; read line; printf 'echo:%s\\n' \"$line\"; sleep 1"
    }

    #[cfg(windows)]
    fn interactive_echo_script() -> &'static str {
        "echo ready && set /p line= && echo echo:%line% && ping -n 2 127.0.0.1 >nul"
    }

    #[cfg(unix)]
    fn hold_script() -> &'static str {
        "sleep 1"
    }

    #[cfg(windows)]
    fn hold_script() -> &'static str {
        "ping -n 2 127.0.0.1 >nul"
    }

    fn shell_spec(session_id: &str, script: &str) -> PtySpawnSpec {
        PtySpawnSpec {
            session_id: RuntimeSessionId::new(session_id),
            program: shell_program().to_owned(),
            args: shell_args(script),
            workdir: std::env::current_dir().expect("resolve current dir"),
            environment: Vec::new(),
            size: TerminalSize::default(),
        }
    }

    async fn collect_until(
        subscription: &mut PtyOutputSubscription,
        needle: &str,
    ) -> RuntimeResult<String> {
        let output = timeout(Duration::from_secs(5), async {
            let mut collected = Vec::new();
            loop {
                match subscription.next_chunk().await? {
                    Some(chunk) => {
                        collected.extend_from_slice(&chunk);
                        if String::from_utf8_lossy(&collected).contains(needle) {
                            return Ok::<Vec<u8>, RuntimeError>(collected);
                        }
                    }
                    None => return Ok::<Vec<u8>, RuntimeError>(collected),
                }
            }
        })
        .await
        .map_err(|_| RuntimeError::Internal("timed out waiting for PTY output".to_owned()))??;

        Ok(String::from_utf8_lossy(&output).to_string())
    }

    async fn snapshot_until(
        manager: &PtyManager,
        session_id: &RuntimeSessionId,
        needle: &str,
    ) -> RuntimeResult<TerminalSnapshot> {
        timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = manager.snapshot(session_id).await?;
                if snapshot.lines.iter().any(|line| line.contains(needle)) {
                    return Ok::<TerminalSnapshot, RuntimeError>(snapshot);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| RuntimeError::Internal("timed out waiting for terminal snapshot".to_owned()))?
    }

    fn coalescer_subscription(
        policy: PtyRenderPolicy,
    ) -> (
        std::sync::mpsc::Sender<Vec<u8>>,
        PtyOutputSubscription,
        std::thread::JoinHandle<()>,
    ) {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (output_tx, output_rx) = broadcast::channel(16);
        let handle = std::thread::spawn(move || run_output_coalescer(raw_rx, output_tx, policy));

        (
            raw_tx,
            PtyOutputSubscription {
                receiver: output_rx,
            },
            handle,
        )
    }

    async fn collect_coalesced_chunks(
        subscription: &mut PtyOutputSubscription,
    ) -> RuntimeResult<Vec<Vec<u8>>> {
        let mut chunks = Vec::new();
        loop {
            match subscription.next_chunk().await? {
                Some(chunk) => chunks.push(chunk),
                None => return Ok(chunks),
            }
        }
    }

    #[test]
    fn render_policy_normalizes_bounds() {
        let manager = PtyManager::with_render_policy(
            0,
            PtyRenderPolicy {
                scrollback_limit: 0,
                output_coalesce_window_ms: 32,
                output_coalesce_max_bytes: 0,
            },
        );

        assert_eq!(manager.output_buffer, 1);
        assert_eq!(manager.render_policy.scrollback_limit, 1);
        assert_eq!(manager.render_policy.output_coalesce_max_bytes, 1);

        let manager = PtyManager::with_render_policy(
            8,
            PtyRenderPolicy {
                scrollback_limit: usize::MAX,
                output_coalesce_window_ms: 32,
                output_coalesce_max_bytes: 8,
            },
        );
        assert_eq!(manager.render_policy.scrollback_limit, MAX_SCROLLBACK_LIMIT);
    }

    #[tokio::test]
    async fn output_coalescing_merges_burst_chunks() {
        let (raw_tx, mut subscription, join_handle) = coalescer_subscription(PtyRenderPolicy {
            scrollback_limit: 16,
            output_coalesce_window_ms: 40,
            output_coalesce_max_bytes: 512,
        });

        raw_tx.send(b"hello".to_vec()).expect("send first chunk");
        raw_tx.send(b"-".to_vec()).expect("send second chunk");
        raw_tx.send(b"world".to_vec()).expect("send third chunk");
        drop(raw_tx);

        join_handle.join().expect("join coalescer");

        let combined = subscription
            .next_chunk()
            .await
            .expect("receive combined chunk")
            .expect("combined output should exist");
        assert_eq!(combined, b"hello-world");

        let stream_end = subscription
            .next_chunk()
            .await
            .expect("stream should close after combined chunk");
        assert!(stream_end.is_none());
    }

    #[tokio::test]
    async fn output_coalescing_can_be_disabled() {
        let (raw_tx, mut subscription, join_handle) = coalescer_subscription(PtyRenderPolicy {
            scrollback_limit: 16,
            output_coalesce_window_ms: 0,
            output_coalesce_max_bytes: 512,
        });

        raw_tx.send(b"one".to_vec()).expect("send first chunk");
        raw_tx.send(b"two".to_vec()).expect("send second chunk");
        drop(raw_tx);

        join_handle.join().expect("join coalescer");

        let first = subscription
            .next_chunk()
            .await
            .expect("read first chunk")
            .expect("first chunk should exist");
        let second = subscription
            .next_chunk()
            .await
            .expect("read second chunk")
            .expect("second chunk should exist");
        assert_eq!(first, b"one");
        assert_eq!(second, b"two");
    }

    #[tokio::test]
    async fn output_coalescing_flushes_continuous_stream_by_time_window() {
        let (raw_tx, mut subscription, join_handle) = coalescer_subscription(PtyRenderPolicy {
            scrollback_limit: 16,
            output_coalesce_window_ms: 20,
            output_coalesce_max_bytes: 1_024,
        });

        let sender = std::thread::spawn(move || {
            for _ in 0..25 {
                raw_tx.send(vec![b'x']).expect("send chunk");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        sender.join().expect("join sender");

        join_handle.join().expect("join coalescer");
        let chunks = collect_coalesced_chunks(&mut subscription)
            .await
            .expect("collect coalesced chunks");

        assert!(
            chunks.len() > 1,
            "continuous stream should be flushed by time window"
        );
        let total_bytes = chunks.iter().map(Vec::len).sum::<usize>();
        assert_eq!(total_bytes, 25);
    }

    #[tokio::test]
    async fn pty_manager_supports_spawn_resize_and_stdio() {
        let manager = PtyManager::default();
        let spec = shell_spec("session-pty-1", interactive_echo_script());
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn PTY process");
        let mut stream = manager
            .subscribe_output(&session_id)
            .await
            .expect("subscribe to PTY output");

        let first = collect_until(&mut stream, "ready")
            .await
            .expect("collect initial output");
        assert!(first.contains("ready"));

        manager
            .resize(&session_id, 120, 40)
            .await
            .expect("resize PTY");

        manager
            .write(&session_id, b"hello\n")
            .await
            .expect("send stdin");

        let echoed = collect_until(&mut stream, "echo:hello")
            .await
            .expect("collect echoed output");
        assert!(echoed.contains("echo:hello"));

        let snapshot = snapshot_until(&manager, &session_id, "echo:hello")
            .await
            .expect("wait for snapshot");
        assert!(snapshot
            .lines
            .iter()
            .any(|line| line.contains("echo:hello")));
    }

    #[tokio::test]
    async fn spawn_rejects_duplicate_session_ids() {
        let manager = PtyManager::default();
        let spec = shell_spec("session-pty-dup", hold_script());
        let duplicate = shell_spec("session-pty-dup", "printf 'second\\n'");

        manager.spawn(spec).await.expect("spawn first session");
        let error = manager
            .spawn(duplicate)
            .await
            .expect_err("duplicate session id should fail");

        assert!(matches!(error, RuntimeError::Configuration(_)));
    }

    #[tokio::test]
    async fn resize_rejects_zero_dimensions() {
        let manager = PtyManager::default();
        let spec = shell_spec("session-pty-resize", hold_script());
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        let error = manager
            .resize(&session_id, 0, 24)
            .await
            .expect_err("zero column resize should fail");

        assert!(matches!(error, RuntimeError::Configuration(_)));
    }

    #[tokio::test]
    async fn snapshot_reflects_resize_dimensions() {
        let manager = PtyManager::default();
        let spec = shell_spec("session-pty-snapshot-resize", hold_script());
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");
        manager
            .resize(&session_id, 132, 48)
            .await
            .expect("resize session");

        let snapshot = manager
            .snapshot(&session_id)
            .await
            .expect("capture snapshot");
        assert_eq!(snapshot.cols, 132);
        assert_eq!(snapshot.rows, 48);
    }

    #[tokio::test]
    async fn spawn_rejects_empty_program() {
        let manager = PtyManager::default();
        let mut spec = shell_spec("session-pty-empty-program", hold_script());
        spec.program = "  ".to_owned();

        let error = manager
            .spawn(spec)
            .await
            .expect_err("empty spawn program should fail");

        assert!(matches!(error, RuntimeError::Configuration(_)));
    }

    #[tokio::test]
    async fn spawn_rejects_zero_size() {
        let manager = PtyManager::default();
        let mut spec = shell_spec("session-pty-zero-size", hold_script());
        spec.size = TerminalSize { cols: 0, rows: 24 };

        let error = manager
            .spawn(spec)
            .await
            .expect_err("zero-size spawn should fail");

        assert!(matches!(error, RuntimeError::Configuration(_)));
    }

    #[tokio::test]
    async fn write_rejects_unknown_session() {
        let manager = PtyManager::default();

        let error = manager
            .write(&RuntimeSessionId::new("missing-session"), b"noop")
            .await
            .expect_err("write to missing session should fail");

        assert!(matches!(error, RuntimeError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn resize_rejects_unknown_session() {
        let manager = PtyManager::default();

        let error = manager
            .resize(&RuntimeSessionId::new("missing-session"), 80, 24)
            .await
            .expect_err("resize for missing session should fail");

        assert!(matches!(error, RuntimeError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn subscribe_rejects_unknown_session() {
        let manager = PtyManager::default();

        let result = manager
            .subscribe_output(&RuntimeSessionId::new("missing-session"))
            .await;

        assert!(matches!(result, Err(RuntimeError::SessionNotFound(_))));
    }

    #[tokio::test]
    async fn snapshot_rejects_unknown_session() {
        let manager = PtyManager::default();

        let error = manager
            .snapshot(&RuntimeSessionId::new("missing-session"))
            .await
            .expect_err("snapshot for missing session should fail");

        assert!(matches!(error, RuntimeError::SessionNotFound(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_renders_ansi_sequences() {
        let manager = PtyManager::default();
        let spec = shell_spec("session-pty-ansi", "printf 'hello\\033[2DXY\\n'; sleep 1");
        let session_id = spec.session_id.clone();

        manager.spawn(spec).await.expect("spawn session");

        let snapshot = snapshot_until(&manager, &session_id, "helXY")
            .await
            .expect("snapshot should contain ansi-rendered output");
        assert!(snapshot.lines.iter().any(|line| line.contains("helXY")));
    }

    #[tokio::test]
    async fn failed_spawn_clears_session_reservation() {
        let manager = PtyManager::default();
        let session_id = RuntimeSessionId::new("session-pty-retry");
        let bad_spec = PtySpawnSpec {
            session_id: session_id.clone(),
            program: "orchestrator-definitely-missing-command".to_owned(),
            args: Vec::new(),
            workdir: std::env::current_dir().expect("resolve current dir"),
            environment: Vec::new(),
            size: TerminalSize::default(),
        };

        let error = manager
            .spawn(bad_spec)
            .await
            .expect_err("spawn with missing command should fail");
        assert!(matches!(error, RuntimeError::Process(_)));

        let retry = shell_spec(session_id.as_str(), hold_script());
        manager
            .spawn(retry)
            .await
            .expect("session id should be reusable after spawn failure");
    }
}
