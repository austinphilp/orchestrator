use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::time::Duration;

use backend_codex::{CodexBackend, CodexBackendConfig};
use orchestrator_runtime::{
    BackendEvent, BackendKind, RuntimeError, RuntimeResult, RuntimeSessionId, SessionLifecycle,
    SpawnSpec, WorkerBackend, WorkerEventStream,
};
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

fn backend_with_binary(binary: PathBuf) -> CodexBackend {
    CodexBackend::new(CodexBackendConfig {
        binary,
        base_args: Vec::new(),
        server_startup_timeout: Duration::from_secs(1),
        legacy_server_base_url: None,
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

fn fake_codex_binary() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "orchestrator-fake-codex-{}-{nanos}.py",
        std::process::id()
    ));
    let script = r#"#!/usr/bin/env python3
import json
import sys

if len(sys.argv) < 2 or sys.argv[1] != "app-server":
    sys.exit(2)

thread_counter = 0
turn_counter = 0

def send(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()

for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    req_id = msg.get("id")
    params = msg.get("params") or {}

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"protocolVersion": 1}})
    elif method == "initialized":
        continue
    elif method == "thread/start":
        thread_counter += 1
        thread_id = f"thread-{thread_counter}"
        send({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "approvalPolicy": "never",
                "cwd": params.get("cwd", "."),
                "model": params.get("model", "gpt-5-codex"),
                "modelProvider": "openai",
                "sandbox": {"mode": "danger-full-access", "network_access": True},
                "thread": {"id": thread_id}
            }
        })
        send({"jsonrpc": "2.0", "method": "thread/started", "params": {"thread": {"id": thread_id}}})
    elif method == "turn/start":
        turn_counter += 1
        turn_id = f"turn-{turn_counter}"
        thread_id = params.get("threadId", "thread-missing")
        input_items = params.get("input") or []
        text = ""
        if input_items and isinstance(input_items[0], dict):
            text = input_items[0].get("text", "")
        send({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {"turn": {"id": turn_id, "status": "inProgress"}}
        })
        send({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": {"threadId": thread_id, "turnId": turn_id, "itemId": "item-1", "delta": f"assistant:{text}"}
        })
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {"threadId": thread_id, "turn": {"id": turn_id, "status": "completed"}}
        })
    elif method == "thread/archive":
        send({"jsonrpc": "2.0", "id": req_id, "result": None})
    elif req_id is not None:
        send({
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": -32601, "message": f"unknown method {method}"}
        })
"#;
    fs::write(&path, script).expect("write fake codex binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)
            .expect("fake codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("set fake codex mode");
    }
    path
}

#[tokio::test]
async fn codex_backend_uses_app_server_stdio_contract() {
    let backend = backend_with_binary(fake_codex_binary());
    backend.health_check().await.expect("health check");

    let handle = backend
        .spawn(spawn_spec("sess-codex-app-server"))
        .await
        .expect("spawn codex session");
    assert_eq!(handle.backend, BackendKind::Codex);

    let stream = backend
        .subscribe(&handle)
        .await
        .expect("subscribe codex session");

    backend
        .send_input(&handle, b"hello codex\n")
        .await
        .expect("send codex input");

    let events = collect_until_terminal_event(stream)
    .await
    .expect("collect codex events");
    assert!(events.iter().any(|event| {
        matches!(
            event,
            BackendEvent::Output(output)
                if String::from_utf8_lossy(&output.bytes).contains("assistant:hello codex")
        )
    }));

    backend.kill(&handle).await.expect("kill codex session");
}

#[tokio::test]
async fn codex_backend_rejects_legacy_http_base_url_config() {
    let backend = CodexBackend::new(CodexBackendConfig {
        binary: "codex".into(),
        base_args: Vec::new(),
        server_startup_timeout: Duration::from_secs(1),
        legacy_server_base_url: Some("http://127.0.0.1:8788".to_owned()),
    });

    let error = backend.health_check().await.expect_err("legacy URL should fail");
    match error {
        RuntimeError::Configuration(message) => {
            assert!(message.contains("ORCHESTRATOR_CODEX_SERVER_BASE_URL"));
        }
        other => panic!("expected configuration error, got {other:?}"),
    }
}
