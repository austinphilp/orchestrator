use crate::*;

fn sample_event(event_id: &str, payload: OrchestrationEventPayload) -> NewEventEnvelope {
    NewEventEnvelope {
        event_id: event_id.to_owned(),
        occurred_at: "2026-02-15T14:00:00Z".to_owned(),
        work_item_id: Some(WorkItemId::new("wi-1")),
        session_id: Some(WorkerSessionId::new("sess-1")),
        payload,
        schema_version: 1,
    }
}

#[test]
fn event_envelope_serialization_round_trip() {
    let event = StoredEventEnvelope::from((
        1,
        sample_event(
            "evt-1",
            OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                session_id: WorkerSessionId::new("sess-1"),
                artifact_id: ArtifactId::new("artifact-1"),
                summary: "checkpoint text only".to_owned(),
            }),
        ),
    ));

    let json = serde_json::to_string(&event).expect("serialize event");
    let parsed: StoredEventEnvelope = serde_json::from_str(&json).expect("deserialize event");
    assert_eq!(parsed, event);
    assert_eq!(parsed.schema_version, 1);
}

#[test]
fn append_assigns_monotonic_sequence_and_ordered_reads() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let first = store
        .append(sample_event(
            "evt-1",
            OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                work_item_id: WorkItemId::new("wi-1"),
                ticket_id: TicketId::from("linear:123"),
                project_id: ProjectId::new("proj-1"),
            }),
        ))
        .expect("append first");
    let second = store
        .append(sample_event(
            "evt-2",
            OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                work_item_id: WorkItemId::new("wi-1"),
                from: WorkflowState::Planning,
                to: WorkflowState::Implementing,
            }),
        ))
        .expect("append second");

    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, 2);

    let ordered = store.read_ordered().expect("read ordered");
    assert_eq!(ordered[0].sequence, 1);
    assert_eq!(ordered[1].sequence, 2);
}

#[test]
fn deterministic_replay_produces_identical_projection_state() {
    let events = vec![
        StoredEventEnvelope::from((
            1,
            sample_event(
                "evt-1",
                OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                    work_item_id: WorkItemId::new("wi-1"),
                    ticket_id: TicketId::from("linear:123"),
                    project_id: ProjectId::new("proj-1"),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            2,
            sample_event(
                "evt-2",
                OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    work_item_id: WorkItemId::new("wi-1"),
                    model: "gpt-5.2-codex".to_owned(),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            3,
            sample_event(
                "evt-3",
                OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                    work_item_id: WorkItemId::new("wi-1"),
                    from: WorkflowState::Planning,
                    to: WorkflowState::Implementing,
                }),
            ),
        )),
    ];

    let first = rebuild_projection(&events);
    let second = rebuild_projection(&events);

    assert_eq!(first, second);
}

#[test]
fn durable_restart_replay_matches_pre_restart_state() {
    let path = std::env::temp_dir().join(format!("orchestrator-events-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut writer = SqliteEventStore::open(&path).expect("open writer store");
    writer
        .append(sample_event(
            "evt-1",
            OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                work_item_id: WorkItemId::new("wi-1"),
                ticket_id: TicketId::from("linear:123"),
                project_id: ProjectId::new("proj-1"),
            }),
        ))
        .expect("append work item");
    writer
        .append(sample_event(
            "evt-2",
            OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                inbox_item_id: InboxItemId::new("inbox-1"),
                work_item_id: WorkItemId::new("wi-1"),
                kind: InboxItemKind::NeedsDecision,
                title: "Need choice".to_owned(),
            }),
        ))
        .expect("append inbox");

    let pre_restart = rebuild_projection(&writer.read_ordered().expect("read writer events"));

    let reader = SqliteEventStore::open(&path).expect("open reader store");
    let post_restart = rebuild_projection(&reader.read_ordered().expect("read reader events"));

    assert_eq!(pre_restart, post_restart);
    let _ = std::fs::remove_file(path);
}

#[test]
fn scoped_retrieval_filters_by_work_item_and_session() {
    let events = vec![
        StoredEventEnvelope {
            event_id: "evt-1".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-15T14:00:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-1")),
            session_id: Some(WorkerSessionId::new("sess-1")),
            event_type: OrchestrationEventType::SessionSpawned,
            payload: OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                session_id: WorkerSessionId::new("sess-1"),
                work_item_id: WorkItemId::new("wi-1"),
                model: "gpt".to_owned(),
            }),
            schema_version: 1,
        },
        StoredEventEnvelope {
            event_id: "evt-2".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-15T14:01:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-2")),
            session_id: Some(WorkerSessionId::new("sess-2")),
            event_type: OrchestrationEventType::SessionSpawned,
            payload: OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                session_id: WorkerSessionId::new("sess-2"),
                work_item_id: WorkItemId::new("wi-2"),
                model: "gpt".to_owned(),
            }),
            schema_version: 1,
        },
    ];
    let state = rebuild_projection(&events);

    let work_item_events = retrieve_events(&state, RetrievalScope::WorkItem(WorkItemId::new("wi-1")), 10);
    assert_eq!(work_item_events.len(), 1);
    assert_eq!(work_item_events[0].event_id, "evt-1");

    let session_events = retrieve_events(
        &state,
        RetrievalScope::Session(WorkerSessionId::new("sess-2")),
        10,
    );
    assert_eq!(session_events.len(), 1);
    assert_eq!(session_events[0].event_id, "evt-2");
}

#[test]
fn retrieval_orders_newest_first_with_limit() {
    let events = (1..=5)
        .map(|seq| StoredEventEnvelope {
            event_id: format!("evt-{seq}"),
            sequence: seq,
            occurred_at: "2026-02-15T14:00:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-1")),
            session_id: Some(WorkerSessionId::new("sess-1")),
            event_type: OrchestrationEventType::UserResponded,
            payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(WorkerSessionId::new("sess-1")),
                work_item_id: Some(WorkItemId::new("wi-1")),
                message: "ok".to_owned(),
            }),
            schema_version: 1,
        })
        .collect::<Vec<_>>();
    let state = rebuild_projection(&events);

    let top3 = retrieve_events(&state, RetrievalScope::Global, 3);
    assert_eq!(top3.len(), 3);
    assert_eq!(top3[0].sequence, 5);
    assert_eq!(top3[2].sequence, 3);
}

#[test]
fn artifact_payload_uses_reference_metadata_not_embedded_blob() {
    let payload = ArtifactCreatedPayload {
        artifact_id: ArtifactId::new("artifact-1"),
        work_item_id: WorkItemId::new("wi-1"),
        kind: ArtifactKind::LogSnippet,
        label: "worker logs".to_owned(),
        uri: "artifact://logs/worker-1".to_owned(),
    };
    let serialized = serde_json::to_string(&payload).expect("serialize artifact payload");

    assert!(serialized.contains("artifact://logs/worker-1"));
    assert!(!serialized.contains("BEGIN RAW LOG CONTENT"));
}
