use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use backend_opencode::{OpenCodeBackend, OpenCodeBackendConfig};
use orchestrator_runtime::{
    BackendEvent, BackendKind, RuntimeResult, RuntimeSessionId, SessionLifecycle, SessionHandle,
    SpawnSpec, WorkerBackend, WorkerEventStream,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Default)]
struct MockState {
    created_sessions: Arc<Mutex<Vec<String>>>,
    sent_inputs: Arc<Mutex<Vec<(String, String)>>>,
    closed_sessions: Arc<Mutex<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    runtime_session_id: String,
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: String,
    thread_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendInputRequest {
    input: String,
}

async fn health() -> &'static str {
    "ok"
}

async fn create_session(
    State(state): State<MockState>,
    Json(request): Json<CreateSessionRequest>,
) -> Json<CreateSessionResponse> {
    let remote_session_id = format!("remote-{}", request.runtime_session_id);
    state
        .created_sessions
        .lock()
        .expect("created sessions lock")
        .push(remote_session_id.clone());
    Json(CreateSessionResponse {
        session_id: remote_session_id,
        thread_id: Some("thread-1".to_owned()),
    })
}

async fn send_input(
    State(state): State<MockState>,
    Path(session_id): Path<String>,
    Json(request): Json<SendInputRequest>,
) -> StatusCode {
    state
        .sent_inputs
        .lock()
        .expect("sent inputs lock")
        .push((session_id, request.input));
    StatusCode::OK
}

async fn close_session(
    State(state): State<MockState>,
    Path(session_id): Path<String>,
) -> StatusCode {
    state
        .closed_sessions
        .lock()
        .expect("closed sessions lock")
        .push(session_id);
    StatusCode::OK
}

async fn events(Path(session_id): Path<String>) -> (StatusCode, Body) {
    let payload = format!(
        "{{\"type\":\"output\",\"stream\":\"stdout\",\"text\":\"hello-{session_id}\"}}\n{{\"type\":\"done\",\"summary\":\"done-{session_id}\"}}\n"
    );
    (StatusCode::OK, Body::from(payload))
}

async fn spawn_mock_server() -> (String, MockState, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let state = MockState::default();
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/sessions", post(create_session))
        .route("/v1/sessions/{session_id}/input", post(send_input))
        .route("/v1/sessions/{session_id}", delete(close_session))
        .route("/v1/sessions/{session_id}/events", get(events))
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server listener");
    let address: SocketAddr = listener.local_addr().expect("mock listener local addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        });
        server.await.expect("run mock server");
    });
    (format!("http://{address}"), state, shutdown_tx, handle)
}

fn backend_with_server(base_url: String) -> OpenCodeBackend {
    OpenCodeBackend::new(OpenCodeBackendConfig {
        binary: "opencode".into(),
        base_args: Vec::new(),
        output_buffer: 128,
        server_base_url: Some(base_url),
        server_startup_timeout: Duration::from_secs(1),
    })
}

fn spawn_spec(session_id: &str) -> SpawnSpec {
    SpawnSpec {
        session_id: RuntimeSessionId::new(session_id),
        workdir: std::env::current_dir().expect("current dir"),
        model: None,
        instruction_prelude: None,
        environment: Vec::new(),
    }
}

async fn collect_until_terminal_event(mut stream: WorkerEventStream) -> RuntimeResult<Vec<BackendEvent>> {
    timeout(TEST_TIMEOUT, async {
        let mut events = Vec::new();
        loop {
            match stream.next_event().await? {
                Some(event) => {
                    let terminal = matches!(event, BackendEvent::Done(_) | BackendEvent::Crashed(_));
                    events.push(event);
                    if terminal {
                        return Ok(events);
                    }
                }
                None => return Ok(events),
            }
        }
    })
    .await
    .expect("collect events timeout")
}

#[tokio::test]
async fn opencode_backend_uses_server_lifecycle_endpoints() {
    let (base_url, state, shutdown_tx, server_task) = spawn_mock_server().await;
    let backend = backend_with_server(base_url);
    backend.health_check().await.expect("health check");

    let handle: SessionHandle = backend
        .spawn(spawn_spec("sess-server-lifecycle"))
        .await
        .expect("spawn session");
    assert_eq!(handle.backend, BackendKind::OpenCode);

    let stream = backend.subscribe(&handle).await.expect("subscribe");
    let events = collect_until_terminal_event(stream)
        .await
        .expect("collect stream events");
    assert!(events.iter().any(|event| {
        matches!(
            event,
            BackendEvent::Output(output)
                if String::from_utf8_lossy(&output.bytes).contains("hello-remote-sess-server-lifecycle")
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            BackendEvent::Done(done)
                if done.summary.as_deref() == Some("done-remote-sess-server-lifecycle")
        )
    }));

    backend
        .send_input(&handle, b"apply patch\n")
        .await
        .expect("send input");
    backend.kill(&handle).await.expect("kill session");

    let sent_inputs = state.sent_inputs.lock().expect("sent inputs lock").clone();
    assert!(sent_inputs.iter().any(|(session_id, input)| {
        session_id == "remote-sess-server-lifecycle" && input == "apply patch\n"
    }));

    let closed_sessions = state
        .closed_sessions
        .lock()
        .expect("closed sessions lock")
        .clone();
    assert!(closed_sessions
        .iter()
        .any(|session_id| session_id == "remote-sess-server-lifecycle"));

    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}

#[tokio::test]
async fn opencode_backend_streams_are_isolated_per_session() {
    let (base_url, _state, shutdown_tx, server_task) = spawn_mock_server().await;
    let backend = backend_with_server(base_url);

    let session_a = backend
        .spawn(spawn_spec("sess-server-a"))
        .await
        .expect("spawn session a");
    let session_b = backend
        .spawn(spawn_spec("sess-server-b"))
        .await
        .expect("spawn session b");

    let events_a = collect_until_terminal_event(
        backend
            .subscribe(&session_a)
            .await
            .expect("subscribe session a"),
    )
    .await
    .expect("collect events a");
    let events_b = collect_until_terminal_event(
        backend
            .subscribe(&session_b)
            .await
            .expect("subscribe session b"),
    )
    .await
    .expect("collect events b");

    let output_a = events_a
        .iter()
        .filter_map(|event| match event {
            BackendEvent::Output(output) => Some(String::from_utf8_lossy(&output.bytes).to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let output_b = events_b
        .iter()
        .filter_map(|event| match event {
            BackendEvent::Output(output) => Some(String::from_utf8_lossy(&output.bytes).to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert!(output_a.contains("hello-remote-sess-server-a"));
    assert!(output_b.contains("hello-remote-sess-server-b"));
    assert!(!output_a.contains("sess-server-b"));
    assert!(!output_b.contains("sess-server-a"));

    backend.kill(&session_a).await.expect("kill a");
    backend.kill(&session_b).await.expect("kill b");
    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
