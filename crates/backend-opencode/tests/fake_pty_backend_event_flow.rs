use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use backend_opencode::{OpenCodeBackend, OpenCodeBackendConfig};
use orchestrator_core::{
    attention_inbox_snapshot, normalize_backend_event, rebuild_projection, ArtifactKind,
    AttentionBatchKind, AttentionEngineConfig, BackendEventNormalizationContext, InboxItemKind,
    OrchestrationEventPayload, ProjectionState, SqliteEventStore, TicketId, TicketProvider,
    TicketRecord, TicketWorkItemMapping, WorkItemId, WorkerSessionId,
};
use orchestrator_runtime::{
    BackendBlockedEvent, BackendCheckpointEvent, BackendDoneEvent, BackendEvent,
    ManagedSessionStatus, PtyRenderPolicy, RuntimeSessionId, SessionLifecycle, SessionVisibility,
    SpawnSpec, TerminalSize, WorkerBackend, WorkerEventStream, WorkerManager, WorkerManagerConfig,
    WorkerManagerEventSubscription,
};
use tokio::time::{sleep, timeout};

const TEST_TIMEOUT: Duration = Duration::from_secs(8);
const TEST_EVENT_TIMESTAMP: &str = "2026-02-16T12:00:00Z";

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
fn nominal_fixture_script() -> &'static str {
    r#"printf '@@checkpoint {"stage":"implementing","detail":"updating backend event handling","files":["src/lib.rs"]}\n';
printf '@@needs_input {"id":"choice-1","question":"Choose rollout strategy","options":["safe","fast"],"default":"safe"}\n';
printf '@@artifact {"kind":"pr","id":"artifact-pr-118","url":"https://github.com/example/orchestrator/pull/118","label":"Draft PR"}\n';
printf '@@blocked {"reason":"tests failing","hint":"run cargo test -p orchestrator-core","log_ref":"artifact://logs/nominal-blocked"}\n';
printf '@@done {"summary":"nominal fixture complete"}\n'"#
}

#[cfg(windows)]
fn nominal_fixture_script() -> &'static str {
    "echo @@checkpoint {\"stage\":\"implementing\",\"detail\":\"updating backend event handling\",\"files\":[\"src/lib.rs\"]} && echo @@needs_input {\"id\":\"choice-1\",\"question\":\"Choose rollout strategy\",\"options\":[\"safe\",\"fast\"],\"default\":\"safe\"} && echo @@artifact {\"kind\":\"pr\",\"id\":\"artifact-pr-118\",\"url\":\"https://github.com/example/orchestrator/pull/118\",\"label\":\"Draft PR\"} && echo @@blocked {\"reason\":\"tests failing\",\"hint\":\"run cargo test -p orchestrator-core\",\"log_ref\":\"artifact://logs/nominal-blocked\"} && echo @@done {\"summary\":\"nominal fixture complete\"}"
}

#[cfg(unix)]
fn malformed_fixture_script() -> &'static str {
    r#"printf '@@checkpoint {"stage":"investigating","detail":"malformed fixture replay","files":["src/parser.rs"]}\n';
printf '@@needs_input {"id":"broken","question":"missing brace"\n';
printf '@@artifact {"kind":1,"url":"https://github.com/example/orchestrator/pull/999"}\n';
printf '@@blocked {"reason":"malformed fixture blocked","hint":"inspect parser fallback","log_ref":"artifact://logs/malformed-fixture"}\n';
printf '@@done {"summary":"malformed fixture replay complete"}\n'"#
}

#[cfg(windows)]
fn malformed_fixture_script() -> &'static str {
    "echo @@checkpoint {\"stage\":\"investigating\",\"detail\":\"malformed fixture replay\",\"files\":[\"src/parser.rs\"]} && echo @@needs_input {\"id\":\"broken\",\"question\":\"missing brace\" && echo @@artifact {\"kind\":1,\"url\":\"https://github.com/example/orchestrator/pull/999\"} && echo @@blocked {\"reason\":\"malformed fixture blocked\",\"hint\":\"inspect parser fallback\",\"log_ref\":\"artifact://logs/malformed-fixture\"} && echo @@done {\"summary\":\"malformed fixture replay complete\"}"
}

#[cfg(unix)]
fn background_fixture_script() -> &'static str {
    r#"printf '@@checkpoint {"stage":"implementing","detail":"bg-step-1","files":["src/runtime.rs"]}\n';
sleep 1;
printf '@@needs_input {"id":"bg-choice","question":"bg-step-2","options":["yes","no"],"default":"yes"}\n';
sleep 1;
printf '@@blocked {"reason":"bg-step-3","hint":"inspect background worker","log_ref":"artifact://logs/background-flow"}\n';
sleep 1;
printf '@@done {"summary":"bg-finish"}\n'"#
}

#[cfg(windows)]
fn background_fixture_script() -> &'static str {
    "echo @@checkpoint {\"stage\":\"implementing\",\"detail\":\"bg-step-1\",\"files\":[\"src/runtime.rs\"]} && ping -n 2 127.0.0.1 >nul && echo @@needs_input {\"id\":\"bg-choice\",\"question\":\"bg-step-2\",\"options\":[\"yes\",\"no\"],\"default\":\"yes\"} && ping -n 2 127.0.0.1 >nul && echo @@blocked {\"reason\":\"bg-step-3\",\"hint\":\"inspect background worker\",\"log_ref\":\"artifact://logs/background-flow\"} && ping -n 2 127.0.0.1 >nul && echo @@done {\"summary\":\"bg-finish\"}"
}

fn test_backend(script: &str) -> OpenCodeBackend {
    OpenCodeBackend::new(OpenCodeBackendConfig {
        binary: PathBuf::from(shell_program()),
        base_args: shell_args(script),
        model_flag: None,
        output_buffer: 128,
        terminal_size: TerminalSize::default(),
        render_policy: PtyRenderPolicy::default(),
        health_check_timeout: Duration::from_secs(1),
        protocol_guidance: None,
    })
}

fn spawn_spec(session_id: &str) -> SpawnSpec {
    SpawnSpec {
        session_id: RuntimeSessionId::new(session_id),
        workdir: std::env::current_dir().expect("resolve workdir"),
        model: None,
        instruction_prelude: None,
        environment: Vec::new(),
    }
}

async fn collect_until_terminal_event(mut stream: WorkerEventStream) -> Vec<BackendEvent> {
    timeout(TEST_TIMEOUT, async {
        let mut events = Vec::new();
        loop {
            match stream.next_event().await.expect("read backend event") {
                Some(event) => {
                    let terminal =
                        matches!(event, BackendEvent::Done(_) | BackendEvent::Crashed(_));
                    events.push(event);
                    if terminal {
                        return events;
                    }
                }
                None => return events,
            }
        }
    })
    .await
    .expect("timed out collecting backend events")
}

async fn collect_global_events_until_terminal(
    subscription: &mut WorkerManagerEventSubscription,
    session_id: &RuntimeSessionId,
) -> Vec<BackendEvent> {
    timeout(TEST_TIMEOUT, async {
        let mut events = Vec::new();
        loop {
            match subscription
                .next_event()
                .await
                .expect("read worker-manager event")
            {
                Some(manager_event) => {
                    if &manager_event.session_id != session_id {
                        continue;
                    }
                    let terminal = matches!(
                        manager_event.event,
                        BackendEvent::Done(_) | BackendEvent::Crashed(_)
                    );
                    events.push(manager_event.event);
                    if terminal {
                        return events;
                    }
                }
                None => return events,
            }
        }
    })
    .await
    .expect("timed out collecting runtime events")
}

fn setup_store(work_item_id: &WorkItemId, ticket_number: &str) -> SqliteEventStore {
    let ticket_identifier = format!("AP-{ticket_number}");
    let store = SqliteEventStore::in_memory().expect("create in-memory event store");
    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, ticket_number),
        provider: TicketProvider::Linear,
        provider_ticket_id: ticket_number.to_owned(),
        identifier: ticket_identifier.clone(),
        title: format!("{ticket_identifier} fake PTY integration fixture"),
        state: "in_progress".to_owned(),
        updated_at: TEST_EVENT_TIMESTAMP.to_owned(),
    };
    store.upsert_ticket(&ticket).expect("upsert ticket");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket.ticket_id.clone(),
            work_item_id: work_item_id.clone(),
        })
        .expect("map ticket to work item");
    store
}

fn persist_normalized_events(
    store: &mut SqliteEventStore,
    work_item_id: &WorkItemId,
    session_id: &WorkerSessionId,
    backend_events: &[BackendEvent],
) {
    for (index, backend_event) in backend_events.iter().enumerate() {
        let normalized = normalize_backend_event(
            &BackendEventNormalizationContext::new(
                TEST_EVENT_TIMESTAMP,
                work_item_id.clone(),
                session_id.clone(),
                index as u64 + 1,
            ),
            backend_event,
        );

        for artifact in &normalized.artifacts {
            store
                .create_artifact(artifact)
                .expect("persist normalized artifact");
        }
        for domain_event in &normalized.events {
            store
                .append_event(domain_event.event.clone(), &domain_event.artifact_refs)
                .expect("persist normalized event");
        }
    }
}

fn collect_raw_output(events: &[BackendEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            BackendEvent::Output(output) => {
                Some(String::from_utf8_lossy(&output.bytes).into_owned())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn non_output_events(events: &[BackendEvent]) -> Vec<&BackendEvent> {
    events
        .iter()
        .filter(|event| !matches!(event, BackendEvent::Output(_)))
        .collect()
}

fn assert_decide_or_unblock_attention_counts(
    projection: &ProjectionState,
    expected_needs_decision: usize,
    expected_blocked: usize,
    expected_unresolved: usize,
) {
    assert_eq!(
        projection
            .inbox_items
            .values()
            .filter(|item| item.kind == InboxItemKind::NeedsDecision)
            .count(),
        expected_needs_decision
    );
    assert_eq!(
        projection
            .inbox_items
            .values()
            .filter(|item| item.kind == InboxItemKind::Blocked)
            .count(),
        expected_blocked
    );

    let attention = attention_inbox_snapshot(projection, &AttentionEngineConfig::default(), &[]);
    let decide_or_unblock_batch = attention
        .batch_surfaces
        .iter()
        .find(|batch| batch.kind == AttentionBatchKind::DecideOrUnblock)
        .expect("decide/unblock batch");
    assert_eq!(
        decide_or_unblock_batch.unresolved_count,
        expected_unresolved
    );
}

async fn snapshot_until_contains(
    manager: &WorkerManager,
    session_id: &RuntimeSessionId,
    needle: &str,
) {
    timeout(TEST_TIMEOUT, async {
        loop {
            let snapshot = manager
                .snapshot(session_id)
                .await
                .expect("capture background snapshot");
            if snapshot.lines.iter().any(|line| line.contains(needle)) {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for background snapshot content");
}

#[tokio::test]
async fn fake_pty_nominal_fixture_round_trips_from_backend_stream_to_persisted_domain_events() {
    let work_item_id = WorkItemId::new("wi-ap-118-nominal");
    let session_id = WorkerSessionId::new("sess-ap-118-nominal");
    let backend = test_backend(nominal_fixture_script());
    let handle = backend
        .spawn(spawn_spec(session_id.as_str()))
        .await
        .expect("spawn fake PTY fixture");
    let stream = backend
        .subscribe(&handle)
        .await
        .expect("subscribe to backend stream");
    let events = collect_until_terminal_event(stream).await;
    let _ = backend.kill(&handle).await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, BackendEvent::Output(_))),
        "fixture should include raw output chunks"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| !matches!(event, BackendEvent::Output(_)))
            .count(),
        5,
        "nominal fixture should emit one structured event for each protocol tag"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::NeedsInput(payload) if payload.prompt_id == "choice-1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Blocked(BackendBlockedEvent { reason, .. }) if reason == "tests failing"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Done(BackendDoneEvent {
            summary: Some(summary)
        }) if summary == "nominal fixture complete"
    )));

    let mut store = setup_store(&work_item_id, "118");
    persist_normalized_events(&mut store, &work_item_id, &session_id, &events);

    let stored = store
        .read_event_with_artifacts(&work_item_id)
        .expect("read stored normalized events");

    let mut blocked_log_artifact_id = None;
    let mut pr_artifact_id = None;
    let mut saw_needs_input = false;
    let mut saw_needs_decision_inbox = false;
    let mut saw_completed = false;

    for stored_event in &stored {
        match &stored_event.event.payload {
            OrchestrationEventPayload::SessionNeedsInput(payload) => {
                saw_needs_input = true;
                assert_eq!(payload.prompt_id.as_deref(), Some("choice-1"));
                assert_eq!(payload.options, vec!["safe", "fast"]);
                assert_eq!(payload.default_option.as_deref(), Some("safe"));
            }
            OrchestrationEventPayload::SessionBlocked(payload)
                if payload.reason == "tests failing" =>
            {
                assert_eq!(
                    payload.hint.as_deref(),
                    Some("run cargo test -p orchestrator-core")
                );
                assert_eq!(
                    payload.log_ref.as_deref(),
                    Some("artifact://logs/nominal-blocked")
                );
                assert_eq!(stored_event.artifact_ids.len(), 1);
                blocked_log_artifact_id = stored_event.artifact_ids.first().cloned();
            }
            OrchestrationEventPayload::ArtifactCreated(payload)
                if payload.kind == ArtifactKind::PR =>
            {
                assert_eq!(
                    payload.uri,
                    "https://github.com/example/orchestrator/pull/118"
                );
                pr_artifact_id = Some(payload.artifact_id.clone());
                assert!(
                    stored_event.artifact_ids.contains(&payload.artifact_id),
                    "artifact-created event should reference persisted artifact id"
                );
            }
            OrchestrationEventPayload::InboxItemCreated(payload)
                if payload.kind == InboxItemKind::NeedsDecision =>
            {
                saw_needs_decision_inbox = true;
            }
            OrchestrationEventPayload::SessionCompleted(_) => {
                saw_completed = true;
            }
            _ => {}
        }
    }

    assert!(
        saw_needs_input,
        "needs-input event should be normalized and persisted"
    );
    assert!(
        saw_needs_decision_inbox,
        "needs-input should also create a needs-decision inbox item"
    );
    assert!(
        saw_completed,
        "done event should create a completed-session domain event"
    );

    let blocked_log_artifact_id = blocked_log_artifact_id.expect("blocked log artifact id");
    let blocked_artifact = store
        .get_artifact(&blocked_log_artifact_id)
        .expect("lookup blocked artifact")
        .expect("blocked artifact should exist");
    assert_eq!(
        blocked_artifact.storage_ref,
        "artifact://logs/nominal-blocked"
    );

    let pr_artifact_id = pr_artifact_id.expect("pull request artifact id");
    let pr_artifact = store
        .get_artifact(&pr_artifact_id)
        .expect("lookup pr artifact")
        .expect("pull request artifact should exist");
    assert_eq!(
        pr_artifact.storage_ref,
        "https://github.com/example/orchestrator/pull/118"
    );

    let projection = rebuild_projection(
        &store
            .read_events_for_work_item(&work_item_id)
            .expect("read events for projection"),
    );
    assert_eq!(projection.events.len(), stored.len());
    let work_item_projection = projection
        .work_items
        .get(&work_item_id)
        .expect("work item projection should exist");
    assert_eq!(work_item_projection.inbox_items.len(), 3);
    assert_decide_or_unblock_attention_counts(&projection, 1, 1, 2);
}

#[tokio::test]
async fn fake_pty_malformed_fixture_ignores_invalid_tags_but_persists_valid_follow_up_events() {
    let work_item_id = WorkItemId::new("wi-ap-118-malformed");
    let session_id = WorkerSessionId::new("sess-ap-118-malformed");
    let backend = test_backend(malformed_fixture_script());
    let handle = backend
        .spawn(spawn_spec(session_id.as_str()))
        .await
        .expect("spawn malformed fake PTY fixture");
    let stream = backend
        .subscribe(&handle)
        .await
        .expect("subscribe to backend stream");
    let events = collect_until_terminal_event(stream).await;
    let _ = backend.kill(&handle).await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, BackendEvent::Output(_))),
        "malformed fixture should include raw output chunks"
    );
    let raw_output = collect_raw_output(&events);
    assert!(
        raw_output.contains("@@needs_input {\"id\":\"broken\",\"question\":\"missing brace\""),
        "raw output should preserve malformed protocol lines for debugging"
    );

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, BackendEvent::NeedsInput(_))),
        "invalid @@needs_input payload should not produce a structured event"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, BackendEvent::Artifact(_))),
        "invalid @@artifact payload should not produce any structured artifact event"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Blocked(BackendBlockedEvent { reason, .. }) if reason == "malformed fixture blocked"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Done(BackendDoneEvent {
            summary: Some(summary)
        }) if summary == "malformed fixture replay complete"
    )));

    let mut store = setup_store(&work_item_id, "118");
    persist_normalized_events(&mut store, &work_item_id, &session_id, &events);

    let stored = store
        .read_event_with_artifacts(&work_item_id)
        .expect("read stored malformed fixture events");
    assert!(
        !stored.iter().any(|event| matches!(
            event.event.payload,
            OrchestrationEventPayload::SessionNeedsInput(_)
        )),
        "invalid needs-input payload should not persist as session-needs-input domain event"
    );
    assert!(
        !stored.iter().any(|event| matches!(
            event.event.payload,
            OrchestrationEventPayload::ArtifactCreated(ref payload) if payload.kind == ArtifactKind::PR
        )),
        "invalid artifact payload should not persist as PR artifact event"
    );

    let blocked_event = stored
        .iter()
        .find(|event| {
            matches!(
                &event.event.payload,
                OrchestrationEventPayload::SessionBlocked(payload)
                    if payload.reason == "malformed fixture blocked"
            )
        })
        .expect("blocked event should persist from valid follow-up tag");
    assert_eq!(blocked_event.artifact_ids.len(), 1);

    let blocked_artifact = store
        .get_artifact(&blocked_event.artifact_ids[0])
        .expect("lookup blocked artifact")
        .expect("blocked artifact should exist");
    assert_eq!(
        blocked_artifact.storage_ref,
        "artifact://logs/malformed-fixture"
    );
}

#[tokio::test]
async fn fake_pty_background_session_continues_through_runtime_to_inbox_and_attention() {
    let work_item_id = WorkItemId::new("wi-ap-137-background");
    let runtime_session_id = RuntimeSessionId::new("sess-ap-137-background");
    let worker_session_id = WorkerSessionId::new(runtime_session_id.as_str());
    let backend: Arc<dyn WorkerBackend> = Arc::new(test_backend(background_fixture_script()));
    let manager = WorkerManager::with_config(
        backend,
        WorkerManagerConfig {
            session_event_buffer: 1_024,
            global_event_buffer: 4_096,
            checkpoint_prompt_interval: None,
            background_snapshot_interval: Duration::from_millis(80),
            ..WorkerManagerConfig::default()
        },
    );
    let mut global_events = manager.subscribe_all();
    manager
        .spawn(spawn_spec(runtime_session_id.as_str()))
        .await
        .expect("spawn runtime-managed background session");

    let session = manager
        .list_sessions()
        .await
        .into_iter()
        .find(|session| session.session_id == runtime_session_id)
        .expect("session summary should exist");
    assert_eq!(session.visibility, SessionVisibility::Background);

    snapshot_until_contains(&manager, &runtime_session_id, "bg-step-1").await;
    snapshot_until_contains(&manager, &runtime_session_id, "bg-step-2").await;

    let events =
        collect_global_events_until_terminal(&mut global_events, &runtime_session_id).await;
    let structured = non_output_events(&events);
    assert_eq!(
        structured.len(),
        4,
        "background fixture should emit exactly checkpoint -> needs-input -> blocked -> done"
    );
    assert!(matches!(
        structured[0],
        BackendEvent::Checkpoint(BackendCheckpointEvent { detail: Some(detail), .. }) if detail == "bg-step-1"
    ));
    assert!(matches!(
        structured[1],
        BackendEvent::NeedsInput(payload) if payload.prompt_id == "bg-choice"
    ));
    assert!(matches!(
        structured[2],
        BackendEvent::Blocked(BackendBlockedEvent { reason, .. }) if reason == "bg-step-3"
    ));
    assert!(matches!(
        structured[3],
        BackendEvent::Done(BackendDoneEvent {
            summary: Some(summary)
        }) if summary == "bg-finish"
    ));
    assert_eq!(
        manager
            .session_status(&runtime_session_id)
            .await
            .expect("session status"),
        ManagedSessionStatus::Done
    );

    let mut store = setup_store(&work_item_id, "137");
    persist_normalized_events(&mut store, &work_item_id, &worker_session_id, &events);

    let projection = rebuild_projection(
        &store
            .read_events_for_work_item(&work_item_id)
            .expect("read persisted runtime events"),
    );
    assert_decide_or_unblock_attention_counts(&projection, 1, 1, 2);
}
