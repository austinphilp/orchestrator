use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use orchestrator_app::composition::runtime_config_slices;
use orchestrator_app::frontend::ui_boundary::{
    CreateTicketFromPickerRequest, InboxItemId, InboxItemKind,
    InboxPublishRequest as FrontendInboxPublishRequest,
    InboxResolveRequest as FrontendInboxResolveRequest, MergeQueueCommandKind, MergeQueueEvent,
    SupervisorCommandContext, SupervisorCommandDispatcher, TicketCreateSubmitMode,
    TicketPickerProvider, UntypedCommandInvocation,
};
use orchestrator_app::{
    load_app_config_from_env, App, AppConfig, AppError, AppFrontendController,
    AppTicketPickerProvider, FrontendCommandIntent, FrontendController, FrontendEvent,
    FrontendIntent, FrontendSnapshot, FrontendTerminalEvent, WorkerManagerBackend,
};
use orchestrator_domain::{BackendNeedsInputAnswer, CoreError, WorkerBackend, WorkerSessionId};
use orchestrator_harness::{
    build_provider_with_config, CodexHarnessProviderConfig, HarnessProviderFactoryConfig,
    HarnessProviderFactoryOutput, HarnessProviderKind, HarnessRuntimeProvider,
    OpenCodeHarnessProviderConfig,
};
use orchestrator_supervisor::OpenRouterSupervisor;
use orchestrator_ticketing::{
    build_provider_with_config as build_ticketing_provider_with_config, LinearConfig,
    LinearRuntimeSettings, LinearTicketingProvider, ShortcutConfig, TicketSummary,
    TicketingProvider, TicketingProviderFactoryConfig, TicketingProviderFactoryOutput,
    TicketingProviderKind, WorkflowStateMapSetting,
};
use orchestrator_vcs::{
    build_provider_with_config as build_vcs_provider_with_config, GitCliVcsProviderConfig,
    VcsProvider, VcsProviderFactoryConfig, VcsProviderFactoryOutput, VcsProviderKind,
};
use orchestrator_vcs_repos::{
    build_provider_with_config as build_vcs_repo_provider_with_config, GitHubGhCliRepoProvider,
    GitHubGhCliRepoProviderConfig, VcsRepoProviderFactoryConfig, VcsRepoProviderFactoryOutput,
    VcsRepoProviderKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::{Any, CorsLayer};

const ENV_OPENROUTER_API_KEY: &str = "OPENROUTER_API_KEY";
const ENV_LINEAR_API_KEY: &str = "LINEAR_API_KEY";
const ENV_SHORTCUT_API_KEY: &str = "ORCHESTRATOR_SHORTCUT_API_KEY";
const WEBSOCKET_OUTBOUND_BUFFER: usize = 256;
const MERGE_EVENT_CHANNEL_CAPACITY: usize = 128;
const WEB_INDEX_HTML: &str = include_str!("../static/index.html");
const WEB_APP_JS: &str = include_str!("../static/app.js");
const WEB_APP_CSS: &str = include_str!("../static/app.css");

static WS_EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct WebServerState {
    frontend_controller: Arc<dyn FrontendController>,
    ticket_picker_provider: Arc<dyn TicketPickerProvider>,
    supervisor_dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    merge_events: broadcast::Sender<MergeQueueNotification>,
    ws_heartbeat_secs: u64,
    max_ws_message_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MergeQueueNotification {
    Completed {
        session_id: WorkerSessionId,
        command_kind: String,
        completed: bool,
        merge_conflict: bool,
        base_branch: Option<String>,
        head_branch: Option<String>,
        ci_failures: Vec<String>,
        ci_has_failures: bool,
        ci_status_error: Option<String>,
        error: Option<String>,
    },
    SessionFinalized {
        session_id: WorkerSessionId,
        warning: Option<String>,
        event: orchestrator_app::StoredEventEnvelope,
    },
    SessionFinalizeFailed {
        session_id: WorkerSessionId,
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct WsClientEnvelope {
    request_id: Option<String>,
    #[serde(rename = "type")]
    message_type: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Serialize)]
struct WsServerEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<String>,
    #[serde(rename = "type")]
    message_type: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct FrontendCommandRequest {
    command_id: String,
}

#[derive(Debug, Deserialize)]
struct TerminalInputRequest {
    session_id: WorkerSessionId,
    input: String,
}

#[derive(Debug, Deserialize)]
struct NeedsInputResponseRequest {
    session_id: WorkerSessionId,
    prompt_id: String,
    answers: Vec<BackendNeedsInputAnswer>,
}

#[derive(Debug, Deserialize)]
struct SupervisorQueryStartRequest {
    invocation: UntypedCommandInvocation,
    #[serde(default)]
    context: SupervisorCommandContext,
}

#[derive(Debug, Deserialize)]
struct SupervisorQueryCancelRequest {
    stream_id: String,
}

#[derive(Debug, Deserialize)]
struct TicketStartRequest {
    ticket: TicketSummary,
    #[serde(default)]
    repository_override: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TicketCreateRequest {
    brief: String,
    #[serde(default)]
    selected_project: Option<String>,
    submit_mode: String,
}

#[derive(Debug, Deserialize)]
struct TicketArchiveRequest {
    ticket: TicketSummary,
}

#[derive(Debug, Deserialize)]
struct SessionIdRequest {
    session_id: WorkerSessionId,
}

#[derive(Debug, Deserialize)]
struct InboxPublishRequest {
    work_item_id: orchestrator_app::WorkItemId,
    #[serde(default)]
    session_id: Option<WorkerSessionId>,
    kind: InboxItemKind,
    title: String,
    coalesce_key: String,
}

#[derive(Debug, Deserialize)]
struct InboxResolveRequest {
    inbox_item_id: InboxItemId,
    work_item_id: orchestrator_app::WorkItemId,
}

#[derive(Debug, Serialize)]
struct CommandsResponse {
    frontend_command_ids: Vec<String>,
    websocket_request_types: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct KeymapManifest {
    modes: Vec<KeymapModeManifest>,
}

#[derive(Debug, Serialize)]
struct KeymapModeManifest {
    mode: &'static str,
    bindings: Vec<KeyBindingManifest>,
    prefixes: Vec<KeyPrefixManifest>,
}

#[derive(Debug, Serialize)]
struct KeyBindingManifest {
    keys: Vec<&'static str>,
    command_id: &'static str,
}

#[derive(Debug, Serialize)]
struct KeyPrefixManifest {
    keys: Vec<&'static str>,
    label: &'static str,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = load_app_config_from_env()?;
    let runtime_config = runtime_config_slices(&config);
    if !config.web.enabled {
        return Err(anyhow::anyhow!(
            "web server is disabled in config; set [web].enabled = true"
        ));
    }

    let openrouter_api_key = required_env(ENV_OPENROUTER_API_KEY)?;
    let supervisor = OpenRouterSupervisor::with_base_url(
        openrouter_api_key,
        runtime_config.supervisor.openrouter_base_url.clone(),
    )?;
    let vcs = build_vcs_provider(&config, &config.vcs_provider)?;
    let github = build_vcs_repo_provider(&config, &config.vcs_repo_provider)?;
    let (ticketing, linear_ticketing) =
        build_ticketing_provider(&config, &config.ticketing_provider)?;
    let raw_worker_backend = build_harness_provider(&config, &config.harness_provider)?;
    ticketing.health_check().await?;
    raw_worker_backend.health_check().await?;

    let worker_backend: Arc<dyn WorkerBackend + Send + Sync> = Arc::new(
        WorkerManagerBackend::from_harness_provider(raw_worker_backend),
    );

    let app = Arc::new(App {
        config,
        ticketing: ticketing.clone(),
        supervisor,
        github,
    });

    let startup = app.startup_state().await?;
    let frontend_controller = Arc::new(AppFrontendController::from_startup_state(
        app.clone(),
        Some(worker_backend.clone()),
        &startup,
    ));
    frontend_controller.start().await?;

    let ticket_picker_provider = Arc::new(AppTicketPickerProvider::new(
        app.clone(),
        ticketing,
        vcs,
        worker_backend,
    ));

    let (merge_queue_sender, merge_queue_receiver) = mpsc::channel(MERGE_EVENT_CHANNEL_CAPACITY);
    ticket_picker_provider
        .start_pr_pipeline_polling(merge_queue_sender)
        .await?;
    let (merge_event_tx, _) = broadcast::channel(256);
    tokio::spawn(run_merge_event_processor(
        ticket_picker_provider.clone(),
        merge_queue_receiver,
        merge_event_tx.clone(),
    ));

    app.start_linear_polling(linear_ticketing.as_deref())
        .await?;

    let web_state = Arc::new(WebServerState {
        frontend_controller: frontend_controller.clone(),
        ticket_picker_provider: ticket_picker_provider.clone(),
        supervisor_dispatcher: app.clone(),
        merge_events: merge_event_tx,
        ws_heartbeat_secs: config_from_app(&app).web.ws_heartbeat_secs,
        max_ws_message_bytes: config_from_app(&app).web.max_ws_message_bytes,
    });

    let cors = build_cors_layer(config_from_app(&app).web.cors_allowed_origins.as_slice())?;
    let router = Router::new()
        .route("/", get(index_page))
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/healthz", get(healthz))
        .route("/v1/snapshot", get(get_snapshot))
        .route("/v1/commands", get(get_commands))
        .route("/v1/keymap", get(get_keymap))
        .route("/v1/ws", get(ws_handler))
        .layer(cors)
        .with_state(web_state);

    let bind_addr = format!(
        "{}:{}",
        config_from_app(&app).web.bind_host,
        config_from_app(&app).web.port
    );
    let listener = tokio::net::TcpListener::bind(bind_addr.as_str()).await?;
    tracing::info!(bind = %bind_addr, "orchestrator-web listening");

    let server = axum::serve(listener, router);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    server.with_graceful_shutdown(shutdown).await?;

    ticket_picker_provider.stop_pr_pipeline_polling().await?;
    app.stop_linear_polling(linear_ticketing.as_deref()).await?;
    frontend_controller.stop().await?;

    Ok(())
}

fn config_from_app<S, G>(app: &App<S, G>) -> &AppConfig
where
    S: orchestrator_supervisor::Supervisor,
    G: orchestrator_vcs_repos::GithubClient,
{
    &app.config
}

async fn run_merge_event_processor(
    ticket_picker_provider: Arc<dyn TicketPickerProvider>,
    mut receiver: mpsc::Receiver<MergeQueueEvent>,
    event_bus: broadcast::Sender<MergeQueueNotification>,
) {
    while let Some(event) = receiver.recv().await {
        match event {
            MergeQueueEvent::Completed {
                session_id,
                kind,
                completed,
                merge_conflict,
                base_branch,
                head_branch,
                ci_checks: _,
                ci_failures,
                ci_has_failures,
                ci_status_error,
                error,
            } => {
                let kind_label = match kind {
                    MergeQueueCommandKind::Reconcile => "reconcile",
                    MergeQueueCommandKind::Merge => "merge",
                };
                let _ = event_bus.send(MergeQueueNotification::Completed {
                    session_id: session_id.clone(),
                    command_kind: kind_label.to_owned(),
                    completed,
                    merge_conflict,
                    base_branch,
                    head_branch,
                    ci_failures,
                    ci_has_failures,
                    ci_status_error,
                    error,
                });

                if completed && matches!(kind, MergeQueueCommandKind::Merge) {
                    match ticket_picker_provider
                        .complete_session_after_merge(session_id.clone())
                        .await
                    {
                        Ok(outcome) => {
                            let _ = event_bus.send(MergeQueueNotification::SessionFinalized {
                                session_id,
                                warning: None,
                                event: outcome.event,
                            });
                        }
                        Err(error) => {
                            let _ = event_bus.send(MergeQueueNotification::SessionFinalizeFailed {
                                session_id,
                                message: error.to_string(),
                            });
                        }
                    }
                }
            }
            MergeQueueEvent::SessionFinalized { session_id, event } => {
                let _ = event_bus.send(MergeQueueNotification::SessionFinalized {
                    session_id,
                    warning: None,
                    event,
                });
            }
            MergeQueueEvent::SessionFinalizeFailed {
                session_id,
                message,
            } => {
                let _ = event_bus.send(MergeQueueNotification::SessionFinalizeFailed {
                    session_id,
                    message,
                });
            }
        }
    }
}

fn build_cors_layer(origins: &[String]) -> Result<CorsLayer> {
    if origins.iter().any(|origin| origin.trim() == "*") {
        return Ok(CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(Any));
    }

    let mut header_values = Vec::new();
    for origin in origins {
        let trimmed = origin.trim();
        if trimmed.is_empty() {
            continue;
        }
        header_values.push(HeaderValue::from_str(trimmed)?);
    }

    if header_values.is_empty() {
        return Ok(CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(Any));
    }

    Ok(CorsLayer::new()
        .allow_origin(header_values)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any))
}

async fn index_page() -> Html<&'static str> {
    Html(WEB_INDEX_HTML)
}

async fn app_js() -> Response {
    (
        [("content-type", "application/javascript; charset=utf-8")],
        WEB_APP_JS,
    )
        .into_response()
}

async fn app_css() -> Response {
    ([("content-type", "text/css; charset=utf-8")], WEB_APP_CSS).into_response()
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "ok": true })))
}

async fn get_snapshot(
    State(state): State<Arc<WebServerState>>,
) -> Result<Json<FrontendSnapshot>, (StatusCode, String)> {
    state
        .frontend_controller
        .snapshot()
        .await
        .map(Json)
        .map_err(core_error_response)
}

async fn get_commands() -> impl IntoResponse {
    let frontend_command_ids = FrontendCommandIntent::ALL
        .iter()
        .map(|intent| intent.command_id().to_owned())
        .collect::<Vec<_>>();

    let websocket_request_types = vec![
        "session.init",
        "frontend.intent.command",
        "frontend.intent.terminal_input",
        "frontend.intent.needs_input_response",
        "supervisor.query.start",
        "supervisor.query.cancel",
        "ticket.list",
        "ticket.projects.list",
        "ticket.start_or_resume",
        "ticket.create",
        "ticket.archive",
        "session.workflow.advance",
        "session.merge.enqueue",
        "session.archive",
        "session.diff.fetch",
        "inbox.publish",
        "inbox.resolve",
    ];

    Json(CommandsResponse {
        frontend_command_ids,
        websocket_request_types,
    })
}

async fn get_keymap() -> impl IntoResponse {
    Json(default_keymap_manifest())
}

fn default_keymap_manifest() -> KeymapManifest {
    KeymapManifest {
        modes: vec![
            KeymapModeManifest {
                mode: "normal",
                bindings: vec![
                    binding(&["q"], "ui.shell.quit"),
                    binding(&["down"], "ui.focus_next_inbox"),
                    binding(&["j"], "ui.focus_next_inbox"),
                    binding(&["up"], "ui.focus_previous_inbox"),
                    binding(&["k"], "ui.focus_previous_inbox"),
                    binding(&["]"], "ui.cycle_batch_next"),
                    binding(&["["], "ui.cycle_batch_previous"),
                    binding(&["g"], "ui.jump_first_inbox"),
                    binding(&["G"], "ui.jump_last_inbox"),
                    binding(&["1"], "ui.jump_batch.decide_or_unblock"),
                    binding(&["2"], "ui.jump_batch.approvals"),
                    binding(&["3"], "ui.jump_batch.review_ready"),
                    binding(&["4"], "ui.jump_batch.fyi_digest"),
                    binding(&["s"], "ui.ticket_picker.open"),
                    binding(&["c"], "ui.supervisor_chat.toggle"),
                    binding(&["i"], "ui.mode.insert"),
                    binding(&["I"], "ui.open_terminal_for_selected"),
                    binding(&["o"], "ui.open_session_output_for_selected_inbox"),
                    binding(&["D"], "ui.worktree.diff.toggle"),
                    binding(&["w", "n"], "ui.terminal.workflow.advance"),
                    binding(&["x"], "ui.terminal.archive_selected_session"),
                    binding(&["z", "1"], "ui.jump_batch.decide_or_unblock"),
                    binding(&["z", "2"], "ui.jump_batch.approvals"),
                    binding(&["z", "3"], "ui.jump_batch.review_ready"),
                    binding(&["z", "4"], "ui.jump_batch.fyi_digest"),
                    binding(&["v", "d"], "ui.open_diff_inspector_for_selected"),
                    binding(&["v", "t"], "ui.open_test_inspector_for_selected"),
                    binding(&["v", "p"], "ui.open_pr_inspector_for_selected"),
                    binding(&["v", "c"], "ui.open_chat_inspector_for_selected"),
                ],
                prefixes: vec![
                    prefix(&["z"], "Batch jumps"),
                    prefix(&["v"], "Artifact inspectors"),
                    prefix(&["w"], "Workflow Actions"),
                ],
            },
            KeymapModeManifest {
                mode: "insert",
                bindings: Vec::new(),
                prefixes: Vec::new(),
            },
        ],
    }
}

fn binding(keys: &'static [&'static str], command_id: &'static str) -> KeyBindingManifest {
    KeyBindingManifest {
        keys: keys.to_vec(),
        command_id,
    }
}

fn prefix(keys: &'static [&'static str], label: &'static str) -> KeyPrefixManifest {
    KeyPrefixManifest {
        keys: keys.to_vec(),
        label,
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebServerState>>,
) -> impl IntoResponse {
    ws.max_message_size(state.max_ws_message_bytes)
        .on_upgrade(move |socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(stream: WebSocket, state: Arc<WebServerState>) {
    let (mut writer, mut reader) = stream.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Message>(WEBSOCKET_OUTBOUND_BUFFER);

    let writer_task = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if writer.send(message).await.is_err() {
                break;
            }
        }
    });

    if let Ok(snapshot) = state.frontend_controller.snapshot().await {
        let envelope = event_envelope("frontend.snapshot.updated", json!(snapshot));
        let _ = send_json_message(&outbound_tx, &envelope).await;
    }

    let frontend_stream_task = tokio::spawn(relay_frontend_events(
        state.frontend_controller.clone(),
        outbound_tx.clone(),
    ));

    let merge_stream_task = tokio::spawn(relay_merge_events(
        state.merge_events.subscribe(),
        outbound_tx.clone(),
    ));

    let heartbeat_task = tokio::spawn(relay_heartbeat(
        outbound_tx.clone(),
        state.ws_heartbeat_secs,
    ));

    while let Some(message_result) = reader.next().await {
        let message = match message_result {
            Ok(message) => message,
            Err(_) => break,
        };

        match message {
            Message::Text(text) => {
                let parsed = match serde_json::from_str::<WsClientEnvelope>(&text) {
                    Ok(payload) => payload,
                    Err(error) => {
                        let _ = send_json_message(
                            &outbound_tx,
                            &event_envelope(
                                "error",
                                json!({
                                    "message": format!("invalid websocket payload: {error}"),
                                }),
                            ),
                        )
                        .await;
                        continue;
                    }
                };

                let dispatch_state = state.clone();
                let dispatch_outbound = outbound_tx.clone();
                tokio::spawn(async move {
                    dispatch_ws_message(dispatch_state, dispatch_outbound, parsed).await;
                });
            }
            Message::Ping(payload) => {
                let _ = outbound_tx.send(Message::Pong(payload)).await;
            }
            Message::Close(_) => break,
            Message::Binary(_) | Message::Pong(_) => {}
        }
    }

    frontend_stream_task.abort();
    merge_stream_task.abort();
    heartbeat_task.abort();
    drop(outbound_tx);
    let _ = writer_task.await;
}

async fn relay_frontend_events(
    controller: Arc<dyn FrontendController>,
    outbound: mpsc::Sender<Message>,
) {
    let mut stream = match controller.subscribe().await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = send_json_message(
                &outbound,
                &event_envelope(
                    "error",
                    json!({ "message": format!("frontend subscription unavailable: {error}") }),
                ),
            )
            .await;
            return;
        }
    };

    loop {
        match stream.next_event().await {
            Ok(Some(event)) => {
                let envelope = frontend_event_to_ws(event);
                if send_json_message(&outbound, &envelope).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = send_json_message(
                    &outbound,
                    &event_envelope(
                        "error",
                        json!({ "message": format!("frontend subscription failed: {error}") }),
                    ),
                )
                .await;
                return;
            }
        }
    }
}

async fn relay_merge_events(
    mut merge_events: broadcast::Receiver<MergeQueueNotification>,
    outbound: mpsc::Sender<Message>,
) {
    loop {
        match merge_events.recv().await {
            Ok(event) => {
                let envelope = event_envelope("merge.queue.event", json!(event));
                if send_json_message(&outbound, &envelope).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn relay_heartbeat(outbound: mpsc::Sender<Message>, interval_secs: u64) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(5)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if outbound
            .send(Message::Ping(Vec::new().into()))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn frontend_event_to_ws(event: FrontendEvent) -> WsServerEnvelope {
    match event {
        FrontendEvent::SnapshotUpdated(snapshot) => {
            event_envelope("frontend.snapshot.updated", json!(snapshot))
        }
        FrontendEvent::Notification(notification) => {
            event_envelope("frontend.notification", json!(notification))
        }
        FrontendEvent::TerminalSession(terminal) => match terminal {
            FrontendTerminalEvent::Output { session_id, output } => event_envelope(
                "terminal.output",
                json!({ "session_id": session_id, "output": output }),
            ),
            FrontendTerminalEvent::TurnState {
                session_id,
                turn_state,
            } => event_envelope(
                "terminal.turn_state",
                json!({ "session_id": session_id, "turn_state": turn_state }),
            ),
            FrontendTerminalEvent::NeedsInput {
                session_id,
                needs_input,
            } => event_envelope(
                "terminal.needs_input",
                json!({ "session_id": session_id, "needs_input": needs_input }),
            ),
            FrontendTerminalEvent::StreamFailed {
                session_id,
                message,
                is_session_not_found,
            } => event_envelope(
                "terminal.stream_failed",
                json!({
                    "session_id": session_id,
                    "message": message,
                    "is_session_not_found": is_session_not_found,
                }),
            ),
            FrontendTerminalEvent::StreamEnded { session_id } => {
                event_envelope("terminal.stream_ended", json!({ "session_id": session_id }))
            }
        },
    }
}

async fn dispatch_ws_message(
    state: Arc<WebServerState>,
    outbound: mpsc::Sender<Message>,
    message: WsClientEnvelope,
) {
    let request_id = message.request_id.clone();

    let result: Result<Value, CoreError> = async {
        match message.message_type.as_str() {
        "session.init" => Ok(json!({
            "protocol_version": 1,
            "capabilities": {
                "frontend_stream": true,
                "terminal_stream": true,
                "supervisor_stream": true,
                "ticketing": true,
                "merge_queue": true,
            },
        })),
        "frontend.intent.command" => {
            let payload = parse_payload::<FrontendCommandRequest>(&message.payload)?;
            let intent = frontend_command_intent_from_id(payload.command_id.as_str()).ok_or_else(
                || CoreError::InvalidCommandArgs {
                    command_id: payload.command_id,
                    reason: "unknown frontend command id".to_owned(),
                },
            )?;
            state
                .frontend_controller
                .submit_intent(FrontendIntent::Command(intent))
                .await?;
            Ok(json!({ "accepted": true }))
        }
        "frontend.intent.terminal_input" => {
            let payload = parse_payload::<TerminalInputRequest>(&message.payload)?;
            state
                .frontend_controller
                .submit_intent(FrontendIntent::SendTerminalInput {
                    session_id: payload.session_id,
                    input: payload.input,
                })
                .await?;
            Ok(json!({ "accepted": true }))
        }
        "frontend.intent.needs_input_response" => {
            let payload = parse_payload::<NeedsInputResponseRequest>(&message.payload)?;
            state
                .frontend_controller
                .submit_intent(FrontendIntent::RespondToNeedsInput {
                    session_id: payload.session_id,
                    prompt_id: payload.prompt_id,
                    answers: payload.answers,
                })
                .await?;
            Ok(json!({ "accepted": true }))
        }
        "supervisor.query.start" => {
            let payload = parse_payload::<SupervisorQueryStartRequest>(&message.payload)?;
            let (stream_id, mut stream) = state
                .supervisor_dispatcher
                .dispatch_supervisor_command(payload.invocation, payload.context)
                .await?;

            let stream_id_for_events = stream_id.clone();
            let relay_outbound = outbound.clone();
            tokio::spawn(async move {
                let _ = send_json_message(
                    &relay_outbound,
                    &event_envelope(
                        "supervisor.started",
                        json!({ "stream_id": stream_id_for_events.as_str() }),
                    ),
                )
                .await;

                loop {
                    match stream.next_chunk().await {
                        Ok(Some(chunk)) => {
                            if !chunk.delta.is_empty() {
                                let _ = send_json_message(
                                    &relay_outbound,
                                    &event_envelope(
                                        "supervisor.delta",
                                        json!({ "stream_id": stream_id_for_events.as_str(), "text": chunk.delta }),
                                    ),
                                )
                                .await;
                            }
                            if let Some(rate_limit) = chunk.rate_limit {
                                let _ = send_json_message(
                                    &relay_outbound,
                                    &event_envelope(
                                        "supervisor.rate_limit",
                                        json!({ "stream_id": stream_id_for_events.as_str(), "state": rate_limit }),
                                    ),
                                )
                                .await;
                            }
                            if let Some(usage) = chunk.usage.clone() {
                                let _ = send_json_message(
                                    &relay_outbound,
                                    &event_envelope(
                                        "supervisor.usage",
                                        json!({ "stream_id": stream_id_for_events.as_str(), "usage": usage }),
                                    ),
                                )
                                .await;
                            }
                            if let Some(reason) = chunk.finish_reason {
                                let _ = send_json_message(
                                    &relay_outbound,
                                    &event_envelope(
                                        "supervisor.finished",
                                        json!({
                                            "stream_id": stream_id_for_events.as_str(),
                                            "reason": reason,
                                            "usage": chunk.usage,
                                        }),
                                    ),
                                )
                                .await;
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = send_json_message(
                                &relay_outbound,
                                &event_envelope(
                                    "supervisor.finished",
                                    json!({ "stream_id": stream_id_for_events.as_str(), "reason": "stop" }),
                                ),
                            )
                            .await;
                            return;
                        }
                        Err(error) => {
                            let _ = send_json_message(
                                &relay_outbound,
                                &event_envelope(
                                    "supervisor.failed",
                                    json!({ "stream_id": stream_id_for_events.as_str(), "message": error.to_string() }),
                                ),
                            )
                            .await;
                            return;
                        }
                    }
                }
            });

            Ok(json!({ "stream_id": stream_id }))
        }
        "supervisor.query.cancel" => {
            let payload = parse_payload::<SupervisorQueryCancelRequest>(&message.payload)?;
            state
                .supervisor_dispatcher
                .cancel_supervisor_command(payload.stream_id.as_str())
                .await?;
            Ok(json!({ "cancelled": true }))
        }
        "ticket.list" => {
            let tickets = state.ticket_picker_provider.list_unfinished_tickets().await?;
            Ok(json!({ "tickets": tickets }))
        }
        "ticket.projects.list" => {
            let projects = state.ticket_picker_provider.list_projects().await?;
            Ok(json!({ "projects": projects }))
        }
        "ticket.start_or_resume" => {
            let payload = parse_payload::<TicketStartRequest>(&message.payload)?;
            let repository_override = payload
                .repository_override
                .as_deref()
                .map(PathBuf::from);
            let result = state
                .ticket_picker_provider
                .start_or_resume_ticket(payload.ticket, repository_override)
                .await?;
            let action = match result.action {
                orchestrator_domain::SelectedTicketFlowAction::Started => "started",
                orchestrator_domain::SelectedTicketFlowAction::Resumed => "resumed",
            };
            Ok(json!({
                "action": action,
                "work_item_id": result.mapping.work_item_id,
                "session_id": result.mapping.session.session_id,
                "ticket_id": result.mapping.ticket.ticket_id,
                "ticket_identifier": result.mapping.ticket.identifier,
                "ticket_title": result.mapping.ticket.title,
                "worktree_path": result.mapping.worktree.path,
                "worktree_branch": result.mapping.worktree.branch,
            }))
        }
        "ticket.create" => {
            let payload = parse_payload::<TicketCreateRequest>(&message.payload)?;
            let submit_mode = parse_ticket_create_submit_mode(payload.submit_mode.as_str())?;
            let created = state
                .ticket_picker_provider
                .create_ticket_from_brief(CreateTicketFromPickerRequest {
                    brief: payload.brief,
                    selected_project: payload.selected_project,
                    submit_mode,
                })
                .await?;
            Ok(json!({ "ticket": created }))
        }
        "ticket.archive" => {
            let payload = parse_payload::<TicketArchiveRequest>(&message.payload)?;
            state
                .ticket_picker_provider
                .archive_ticket(payload.ticket)
                .await?;
            Ok(json!({ "archived": true }))
        }
        "session.workflow.advance" => {
            let payload = parse_payload::<SessionIdRequest>(&message.payload)?;
            let outcome = state
                .ticket_picker_provider
                .advance_session_workflow(payload.session_id)
                .await?;
            Ok(json!({
                "session_id": outcome.session_id,
                "work_item_id": outcome.work_item_id,
                "from": format!("{:?}", outcome.from),
                "to": format!("{:?}", outcome.to),
                "instruction": outcome.instruction,
                "event": outcome.event,
            }))
        }
        "session.merge.enqueue" => {
            let payload = parse_payload::<SessionIdRequest>(&message.payload)?;
            state
                .ticket_picker_provider
                .enqueue_pr_merge(payload.session_id)
                .await?;
            Ok(json!({ "enqueued": true }))
        }
        "session.archive" => {
            let payload = parse_payload::<SessionIdRequest>(&message.payload)?;
            let outcome = state
                .ticket_picker_provider
                .archive_session(payload.session_id)
                .await?;
            Ok(json!({ "warning": outcome.warning, "event": outcome.event }))
        }
        "session.diff.fetch" => {
            let payload = parse_payload::<SessionIdRequest>(&message.payload)?;
            let diff = state
                .ticket_picker_provider
                .session_worktree_diff(payload.session_id)
                .await?;
            Ok(json!({
                "session_id": diff.session_id,
                "base_branch": diff.base_branch,
                "diff": diff.diff,
            }))
        }
        "inbox.publish" => {
            let payload = parse_payload::<InboxPublishRequest>(&message.payload)?;
            let event = state
                .ticket_picker_provider
                .publish_inbox_item(FrontendInboxPublishRequest {
                    work_item_id: payload.work_item_id,
                    session_id: payload.session_id,
                    kind: payload.kind,
                    title: payload.title,
                    coalesce_key: payload.coalesce_key,
                })
                .await?;
            Ok(json!({ "event": event }))
        }
        "inbox.resolve" => {
            let payload = parse_payload::<InboxResolveRequest>(&message.payload)?;
            let event = state
                .ticket_picker_provider
                .resolve_inbox_item(FrontendInboxResolveRequest {
                    inbox_item_id: payload.inbox_item_id,
                    work_item_id: payload.work_item_id,
                })
                .await?;
            Ok(json!({ "event": event }))
        }
        other => Err(CoreError::InvalidCommandArgs {
            command_id: other.to_owned(),
            reason: "unknown websocket request type".to_owned(),
        }),
    }
    }
    .await;

    match result {
        Ok(payload) => {
            let envelope = response_ok_envelope(request_id, payload);
            let _ = send_json_message(&outbound, &envelope).await;
        }
        Err(error) => {
            let _ = send_dispatch_error(&outbound, request_id, core_error_payload(&error)).await;
        }
    }
}

fn parse_ticket_create_submit_mode(raw: &str) -> Result<TicketCreateSubmitMode, CoreError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "create_only" | "create-only" => Ok(TicketCreateSubmitMode::CreateOnly),
        "create_and_start" | "create-and-start" => Ok(TicketCreateSubmitMode::CreateAndStart),
        other => Err(CoreError::InvalidCommandArgs {
            command_id: "ticket.create".to_owned(),
            reason: format!(
                "invalid submit_mode '{other}', expected create_only or create_and_start"
            ),
        }),
    }
}

fn parse_payload<T>(value: &Value) -> Result<T, CoreError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(value.clone()).map_err(|error| CoreError::InvalidCommandArgs {
        command_id: "ws.payload".to_owned(),
        reason: format!("invalid payload: {error}"),
    })
}

fn frontend_command_intent_from_id(command_id: &str) -> Option<FrontendCommandIntent> {
    FrontendCommandIntent::ALL
        .iter()
        .copied()
        .find(|intent| intent.command_id() == command_id)
}

fn event_envelope(message_type: &str, payload: Value) -> WsServerEnvelope {
    WsServerEnvelope {
        request_id: None,
        event_id: Some(next_ws_event_id()),
        message_type: message_type.to_owned(),
        payload,
    }
}

fn response_ok_envelope(request_id: Option<String>, payload: Value) -> WsServerEnvelope {
    WsServerEnvelope {
        request_id,
        event_id: None,
        message_type: "response.ok".to_owned(),
        payload,
    }
}

fn response_error_envelope(request_id: Option<String>, payload: Value) -> WsServerEnvelope {
    WsServerEnvelope {
        request_id,
        event_id: None,
        message_type: "response.error".to_owned(),
        payload,
    }
}

async fn send_dispatch_error(
    outbound: &mpsc::Sender<Message>,
    request_id: Option<String>,
    payload: Value,
) -> Result<(), ()> {
    let envelope = response_error_envelope(request_id, payload);
    send_json_message(outbound, &envelope).await.map_err(|_| ())
}

fn core_error_payload(error: &CoreError) -> Value {
    match error {
        CoreError::MissingProjectRepositoryMapping {
            provider: _provider,
            project,
        } => json!({
            "message": error.to_string(),
            "code": "missing_project_repository_mapping",
            "project_id": project,
        }),
        CoreError::InvalidMappedRepository {
            provider: _provider,
            project,
            repository_path,
            reason: _reason,
        } => json!({
            "message": error.to_string(),
            "code": "invalid_mapped_repository",
            "project_id": project,
            "repository_path_hint": repository_path,
        }),
        _ => json!({ "message": error.to_string() }),
    }
}

async fn send_json_message(
    outbound: &mpsc::Sender<Message>,
    envelope: &WsServerEnvelope,
) -> Result<(), ()> {
    let rendered = match serde_json::to_string(envelope) {
        Ok(rendered) => rendered,
        Err(_) => return Err(()),
    };
    outbound
        .send(Message::Text(rendered.into()))
        .await
        .map_err(|_| ())
}

fn next_ws_event_id() -> String {
    let sequence = WS_EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("evt-{sequence}")
}

fn core_error_response(error: CoreError) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, error.to_string())
}

fn required_env(name: &str) -> Result<String, CoreError> {
    let value = std::env::var(name).map_err(|_| {
        CoreError::Configuration(format!(
            "{name} is not set. Export a valid value before starting orchestrator-web."
        ))
    })?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(CoreError::Configuration(format!(
            "{name} is empty. Provide a non-empty value."
        )));
    }
    Ok(value)
}

fn resolve_ticketing_provider_key(provider: &str) -> Result<&'static str, CoreError> {
    TicketingProviderKind::from_key(provider).map_or_else(
        || {
            Err(CoreError::Configuration(format!(
                "Unknown ticketing provider '{provider}'. Expected 'ticketing.linear' or 'ticketing.shortcut'."
            )))
        },
        |kind| Ok(kind.as_key()),
    )
}

fn resolve_harness_provider_key(provider: &str) -> Result<&'static str, CoreError> {
    HarnessProviderKind::from_key(provider).map_or_else(
        || {
            Err(CoreError::Configuration(format!(
                "Unknown harness provider '{provider}'. Expected 'harness.opencode' or 'harness.codex'."
            )))
        },
        |kind| Ok(kind.as_key()),
    )
}

fn resolve_vcs_provider_key(provider: &str) -> Result<&'static str, CoreError> {
    VcsProviderKind::from_key(provider).map_or_else(
        || {
            Err(CoreError::Configuration(format!(
                "Unknown VCS provider '{provider}'. Expected 'vcs.git_cli'."
            )))
        },
        |kind| Ok(kind.as_key()),
    )
}

fn resolve_vcs_repo_provider_key(provider: &str) -> Result<&'static str, CoreError> {
    VcsRepoProviderKind::from_key(provider).map_or_else(
        || {
            Err(CoreError::Configuration(format!(
                "Unknown VCS repo provider '{provider}'. Expected 'vcs_repos.github_gh_cli'."
            )))
        },
        |kind| Ok(kind.as_key()),
    )
}

fn build_ticketing_provider(
    config: &AppConfig,
    provider: &str,
) -> Result<
    (
        Arc<dyn TicketingProvider + Send + Sync>,
        Option<Arc<LinearTicketingProvider>>,
    ),
    AppError,
> {
    match resolve_ticketing_provider_key(provider)? {
        "ticketing.linear" => {
            let api_key = required_env(ENV_LINEAR_API_KEY)?;
            let settings = LinearRuntimeSettings {
                api_url: config.linear.api_url.clone(),
                sync_interval_secs: config.linear.sync_interval_secs,
                fetch_limit: config.linear.fetch_limit,
                sync_assigned_to_me: config.linear.sync_assigned_to_me,
                sync_states: config.linear.sync_states.clone(),
                workflow_state_map: config
                    .linear
                    .workflow_state_map
                    .iter()
                    .map(|entry| WorkflowStateMapSetting {
                        workflow_state: entry.workflow_state.clone(),
                        linear_state: entry.linear_state.clone(),
                    })
                    .collect(),
                workflow_comment_summaries: config.linear.workflow_comment_summaries,
                workflow_attach_pr_links: config.linear.workflow_attach_pr_links,
            };
            let linear_config = LinearConfig::from_settings(api_key, settings)?;
            let provider = build_ticketing_provider_with_config(
                "ticketing.linear",
                TicketingProviderFactoryConfig {
                    linear: linear_config,
                    ..TicketingProviderFactoryConfig::default()
                },
            )?;
            let TicketingProviderFactoryOutput::Linear(provider) = provider else {
                return Err(AppError::configuration(
                    "ticketing provider factory returned a non-linear provider for ticketing.linear"
                        .to_owned(),
                ));
            };
            let provider = Arc::new(provider);
            let linear_ticketing = Arc::clone(&provider);
            let ticketing: Arc<dyn TicketingProvider + Send + Sync> = provider;
            Ok((ticketing, Some(linear_ticketing)))
        }
        "ticketing.shortcut" => {
            let api_key = required_env(ENV_SHORTCUT_API_KEY)?;
            let shortcut_config = ShortcutConfig::from_settings(
                api_key,
                config.shortcut.api_url.clone(),
                config.shortcut.fetch_limit,
            )?;
            let provider = build_ticketing_provider_with_config(
                "ticketing.shortcut",
                TicketingProviderFactoryConfig {
                    shortcut: shortcut_config,
                    ..TicketingProviderFactoryConfig::default()
                },
            )?;
            let TicketingProviderFactoryOutput::Shortcut(provider) = provider else {
                return Err(AppError::configuration(
                    "ticketing provider factory returned a non-shortcut provider for ticketing.shortcut"
                        .to_owned(),
                ));
            };
            let provider = Arc::new(provider);
            let ticketing: Arc<dyn TicketingProvider + Send + Sync> = provider;
            Ok((ticketing, None))
        }
        _ => unreachable!("ticketing provider key resolver returned unsupported key"),
    }
}

fn build_harness_provider(
    config: &AppConfig,
    provider: &str,
) -> Result<Arc<dyn HarnessRuntimeProvider + Send + Sync>, AppError> {
    let provider_key = resolve_harness_provider_key(provider)?;

    let provider = build_provider_with_config(
        provider_key,
        HarnessProviderFactoryConfig {
            opencode: OpenCodeHarnessProviderConfig {
                binary: PathBuf::from(config.runtime.opencode_binary.as_str()),
                base_args: Vec::new(),
                output_buffer: 256,
                server_base_url: Some(config.runtime.opencode_server_base_url.clone()),
                server_startup_timeout: std::time::Duration::from_secs(
                    config.runtime.harness_server_startup_timeout_secs,
                ),
                allow_unsafe_command_paths: config.runtime.allow_unsafe_command_paths,
                harness_log_raw_events: config.runtime.harness_log_raw_events,
                harness_log_normalized_events: config.runtime.harness_log_normalized_events,
            },
            codex: CodexHarnessProviderConfig {
                binary: PathBuf::from(config.runtime.codex_binary.as_str()),
                base_args: Vec::new(),
                server_startup_timeout: std::time::Duration::from_secs(
                    config.runtime.harness_server_startup_timeout_secs,
                ),
                legacy_server_base_url: None,
                harness_log_raw_events: config.runtime.harness_log_raw_events,
                harness_log_normalized_events: config.runtime.harness_log_normalized_events,
            },
        },
    )?;

    match provider {
        HarnessProviderFactoryOutput::OpenCode(provider) => {
            let provider: Arc<dyn HarnessRuntimeProvider + Send + Sync> = Arc::new(provider);
            Ok(provider)
        }
        HarnessProviderFactoryOutput::Codex(provider) => {
            let provider: Arc<dyn HarnessRuntimeProvider + Send + Sync> = Arc::new(provider);
            Ok(provider)
        }
    }
}

fn build_vcs_provider(
    config: &AppConfig,
    provider: &str,
) -> Result<Arc<dyn VcsProvider + Send + Sync>, AppError> {
    let provider_key = resolve_vcs_provider_key(provider)?;

    let provider = build_vcs_provider_with_config(
        provider_key,
        VcsProviderFactoryConfig {
            git_cli: GitCliVcsProviderConfig {
                binary: PathBuf::from(config.git.binary.as_str()),
                allow_destructive_automation: config.git.allow_destructive_automation,
                allow_force_push: config.git.allow_force_push,
                allow_delete_unmerged_branches: config.git.allow_delete_unmerged_branches,
                allow_unsafe_command_paths: config.runtime.allow_unsafe_command_paths,
            },
        },
    )?;

    let VcsProviderFactoryOutput::GitCli(provider) = provider;
    let provider: Arc<dyn VcsProvider + Send + Sync> = Arc::new(provider);
    Ok(provider)
}

fn build_vcs_repo_provider(
    config: &AppConfig,
    provider: &str,
) -> Result<GitHubGhCliRepoProvider, AppError> {
    let provider_key = resolve_vcs_repo_provider_key(provider)?;
    let provider = build_vcs_repo_provider_with_config(
        provider_key,
        VcsRepoProviderFactoryConfig {
            github_gh_cli: GitHubGhCliRepoProviderConfig {
                binary: PathBuf::from(config.github.binary.as_str()),
                allow_unsafe_command_paths: config.runtime.allow_unsafe_command_paths,
            },
        },
    )?;

    let VcsRepoProviderFactoryOutput::GitHubGhCli(provider) = provider;
    Ok(provider)
}
