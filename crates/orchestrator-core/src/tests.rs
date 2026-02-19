use async_trait::async_trait;
use rusqlite::OptionalExtension;

use crate::test_support::TestDbPath;
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
fn session_needs_input_and_blocked_payloads_remain_backward_compatible() {
    let needs_input_json = r#"{"type":"SessionNeedsInput","data":{"session_id":"sess-legacy","prompt":"Choose path A or B"}}"#;
    let needs_input: OrchestrationEventPayload =
        serde_json::from_str(needs_input_json).expect("deserialize legacy needs_input payload");
    match needs_input {
        OrchestrationEventPayload::SessionNeedsInput(payload) => {
            assert_eq!(payload.session_id, WorkerSessionId::new("sess-legacy"));
            assert_eq!(payload.prompt, "Choose path A or B");
            assert!(payload.prompt_id.is_none());
            assert!(payload.options.is_empty());
            assert!(payload.default_option.is_none());
        }
        other => panic!("expected SessionNeedsInput payload, got {other:?}"),
    }

    let blocked_json =
        r#"{"type":"SessionBlocked","data":{"session_id":"sess-legacy","reason":"tests failing"}}"#;
    let blocked: OrchestrationEventPayload =
        serde_json::from_str(blocked_json).expect("deserialize legacy blocked payload");
    match blocked {
        OrchestrationEventPayload::SessionBlocked(payload) => {
            assert_eq!(payload.session_id, WorkerSessionId::new("sess-legacy"));
            assert_eq!(payload.reason, "tests failing");
            assert!(payload.hint.is_none());
            assert!(payload.log_ref.is_none());
        }
        other => panic!("expected SessionBlocked payload, got {other:?}"),
    }
}

#[test]
fn workflow_transition_reason_field_remains_backward_compatible() {
    let workflow_json = r#"{"type":"WorkflowTransition","data":{"work_item_id":"wi-legacy","from":"Planning","to":"Implementing"}}"#;
    let workflow_payload: OrchestrationEventPayload =
        serde_json::from_str(workflow_json).expect("deserialize legacy workflow transition");

    match workflow_payload {
        OrchestrationEventPayload::WorkflowTransition(payload) => {
            assert_eq!(payload.work_item_id, WorkItemId::new("wi-legacy"));
            assert_eq!(payload.from, WorkflowState::Planning);
            assert_eq!(payload.to, WorkflowState::Implementing);
            assert!(payload.reason.is_none());
        }
        other => panic!("expected WorkflowTransition payload, got {other:?}"),
    }
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
                reason: None,
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
                    reason: None,
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
    let db = unique_db("durable-restart");

    let mut writer = SqliteEventStore::open(db.path()).expect("open writer store");
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

    let reader = SqliteEventStore::open(db.path()).expect("open reader store");
    let post_restart = rebuild_projection(&reader.read_ordered().expect("read reader events"));

    assert_eq!(pre_restart, post_restart);
}

#[test]
fn sqlite_count_events_honors_scope() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    store
        .append(sample_event(
            "evt-1",
            OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(WorkerSessionId::new("sess-1")),
                work_item_id: Some(WorkItemId::new("wi-1")),
                message: "first".to_owned(),
            }),
        ))
        .expect("append first");

    store
        .append(NewEventEnvelope {
            event_id: "evt-2".to_owned(),
            occurred_at: "2026-02-15T14:01:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-2")),
            session_id: Some(WorkerSessionId::new("sess-2")),
            payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(WorkerSessionId::new("sess-2")),
                work_item_id: Some(WorkItemId::new("wi-2")),
                message: "second".to_owned(),
            }),
            schema_version: 1,
        })
        .expect("append second");

    assert_eq!(
        store
            .count_events(RetrievalScope::Global)
            .expect("count global"),
        2
    );
    assert_eq!(
        store
            .count_events(RetrievalScope::WorkItem(WorkItemId::new("wi-1")))
            .expect("count work item"),
        1
    );
    assert_eq!(
        store
            .count_events(RetrievalScope::Session(WorkerSessionId::new("sess-2")))
            .expect("count session"),
        1
    );
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

    let work_item_events = retrieve_events(
        &state,
        RetrievalScope::WorkItem(WorkItemId::new("wi-1")),
        10,
    );
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
fn projection_updates_session_status_for_completed_and_crashed_signals() {
    let events = vec![
        StoredEventEnvelope::from((
            1,
            sample_event(
                "evt-session-spawned",
                OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    work_item_id: WorkItemId::new("wi-1"),
                    model: "gpt".to_owned(),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            2,
            sample_event(
                "evt-session-completed",
                OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    summary: Some("complete".to_owned()),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            3,
            sample_event(
                "evt-session-crashed",
                OrchestrationEventPayload::SessionCrashed(SessionCrashedPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    reason: "panic".to_owned(),
                }),
            ),
        )),
    ];

    let projection = rebuild_projection(&events);
    let session = projection
        .sessions
        .get(&WorkerSessionId::new("sess-1"))
        .expect("session exists");
    assert_eq!(session.status, Some(WorkerSessionStatus::Crashed));
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

fn unique_db(tag: &str) -> TestDbPath {
    TestDbPath::new(&format!("core-tests-{tag}"))
}

#[test]
fn initialization_creates_required_schema_and_version() {
    let db = unique_db("init");

    let store = SqliteEventStore::open(db.path()).expect("open store");
    assert_eq!(store.schema_version().expect("schema version"), 5);

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite for inspection");
    let tables = [
        "schema_migrations",
        "tickets",
        "work_items",
        "worktrees",
        "sessions",
        "artifacts",
        "events",
        "event_artifact_refs",
        "project_repositories",
        "harness_session_bindings",
    ];
    for table in tables {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .optional()
            .expect("query table existence");
        assert_eq!(exists, Some(1), "missing table {table}");
    }

    let indexes = [
        "idx_events_work_item_sequence",
        "idx_events_session",
        "idx_tickets_provider_lookup",
        "idx_event_artifact_refs_event",
        "idx_worktrees_work_item_lookup",
        "idx_worktrees_path_unique",
        "idx_sessions_work_item_lookup",
        "idx_sessions_workdir_unique",
        "idx_sessions_status_lookup",
    ];
    for index in indexes {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
                rusqlite::params![index],
                |row| row.get(0),
            )
            .optional()
            .expect("query index existence");
        assert_eq!(exists, Some(1), "missing index {index}");
    }

    let applied_migrations: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(applied_migrations, 5);

    drop(store);
}

#[test]
fn startup_is_idempotent_and_does_not_duplicate_migrations() {
    let db = unique_db("idempotent");

    let first = SqliteEventStore::open(db.path()).expect("first open");
    assert_eq!(first.schema_version().expect("schema version"), 5);
    drop(first);

    let second = SqliteEventStore::open(db.path()).expect("second open");
    assert_eq!(second.schema_version().expect("schema version"), 5);
    drop(second);

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite for inspection");
    let migration_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(migration_count, 5);
}

#[test]
fn startup_adopts_legacy_events_schema_without_recreating_events_table() {
    let db = unique_db("legacy-schema");

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite");
    conn.execute_batch(
        "
        CREATE TABLE events (
            event_id TEXT PRIMARY KEY,
            sequence INTEGER NOT NULL UNIQUE,
            occurred_at TEXT NOT NULL,
            work_item_id TEXT NULL,
            session_id TEXT NULL,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL,
            schema_version INTEGER NOT NULL
        );
        CREATE INDEX idx_events_sequence ON events(sequence);
        CREATE INDEX idx_events_work_item ON events(work_item_id, sequence DESC);
        CREATE INDEX idx_events_session ON events(session_id, sequence DESC);
        INSERT INTO events (
            event_id, sequence, occurred_at, work_item_id, session_id, event_type, payload, schema_version
        ) VALUES (
            'evt-legacy-1',
            1,
            '2026-02-16T03:00:00Z',
            'wi-legacy-1',
            'sess-legacy-1',
            '\"WorkItemCreated\"',
            '{\"type\":\"WorkItemCreated\",\"data\":{\"work_item_id\":\"wi-legacy-1\",\"ticket_id\":\"linear:legacy\",\"project_id\":\"proj-legacy\"}}',
            1
        );
        ",
    )
    .expect("seed legacy schema");
    drop(conn);

    let store = SqliteEventStore::open(db.path()).expect("open store");
    assert_eq!(store.schema_version().expect("schema version"), 5);

    let events = store.read_ordered().expect("read ordered");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, "evt-legacy-1");
    drop(store);

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite for inspection");
    let tables = [
        "schema_migrations",
        "tickets",
        "work_items",
        "worktrees",
        "sessions",
        "artifacts",
        "event_artifact_refs",
        "project_repositories",
        "harness_session_bindings",
    ];
    for table in tables {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .optional()
            .expect("query table existence");
        assert_eq!(exists, Some(1), "missing table {table}");
    }

    let migration_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(migration_count, 1);
}

#[test]
fn mapping_ticket_and_work_item_round_trip_in_both_directions() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "123"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "123".to_owned(),
        identifier: "ORCH-123".to_owned(),
        title: "Implement persistence".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T00:00:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket).expect("upsert ticket");

    let mapping = TicketWorkItemMapping {
        ticket_id: ticket.ticket_id.clone(),
        work_item_id: WorkItemId::new("wi-map-1"),
    };
    store
        .map_ticket_to_work_item(&mapping)
        .expect("map work item");

    let work_item = store
        .find_work_item_by_ticket(&TicketProvider::Linear, "123")
        .expect("lookup by ticket")
        .expect("work item present");
    assert_eq!(work_item, WorkItemId::new("wi-map-1"));

    let found_ticket = store
        .find_ticket_by_work_item(&WorkItemId::new("wi-map-1"))
        .expect("lookup by work item")
        .expect("ticket present");
    assert_eq!(found_ticket.identifier, "ORCH-123");
    assert_eq!(found_ticket.provider_ticket_id, "123");
}

#[test]
fn map_ticket_to_work_item_rejects_rebinding_existing_ticket_or_work_item() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket_a = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "800"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "800".to_owned(),
        identifier: "AP-800".to_owned(),
        title: "Ticket A".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T07:00:00Z".to_owned(),
    };
    let ticket_b = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "801"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "801".to_owned(),
        identifier: "AP-801".to_owned(),
        title: "Ticket B".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T07:01:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket_a).expect("upsert ticket a");
    store.upsert_ticket(&ticket_b).expect("upsert ticket b");

    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_a.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-a"),
        })
        .expect("map ticket a");

    let rebind_ticket = store.map_ticket_to_work_item(&TicketWorkItemMapping {
        ticket_id: ticket_a.ticket_id.clone(),
        work_item_id: WorkItemId::new("wi-other"),
    });
    match rebind_ticket {
        Ok(_) => panic!("expected rebinding ticket to fail"),
        Err(CoreError::Persistence(message)) => {
            assert!(message.contains("ticket_id 'linear:800' is already mapped"))
        }
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }

    let rebind_work_item = store.map_ticket_to_work_item(&TicketWorkItemMapping {
        ticket_id: ticket_b.ticket_id.clone(),
        work_item_id: WorkItemId::new("wi-a"),
    });
    match rebind_work_item {
        Ok(_) => panic!("expected rebinding work item to fail"),
        Err(CoreError::Persistence(message)) => {
            assert!(message.contains("work_item_id 'wi-a' is already mapped"))
        }
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn upsert_worktree_rejects_reusing_a_worktree_path_for_another_work_item() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket_a = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "910"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "910".to_owned(),
        identifier: "AP-910".to_owned(),
        title: "Ticket A".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:10:00Z".to_owned(),
    };
    let ticket_b = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "911"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "911".to_owned(),
        identifier: "AP-911".to_owned(),
        title: "Ticket B".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:11:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket_a).expect("upsert ticket a");
    store.upsert_ticket(&ticket_b).expect("upsert ticket b");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_a.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-910"),
        })
        .expect("map ticket a");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_b.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-911"),
        })
        .expect("map ticket b");

    store
        .upsert_worktree(&WorktreeRecord {
            worktree_id: WorktreeId::new("wt-910"),
            work_item_id: WorkItemId::new("wi-910"),
            path: "/tmp/orchestrator/shared-worktree".to_owned(),
            branch: "ap/AP-910".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T12:12:00Z".to_owned(),
        })
        .expect("insert first worktree");

    let err = match store.upsert_worktree(&WorktreeRecord {
        worktree_id: WorktreeId::new("wt-911"),
        work_item_id: WorkItemId::new("wi-911"),
        path: "/tmp/orchestrator/shared-worktree".to_owned(),
        branch: "ap/AP-911".to_owned(),
        base_branch: "main".to_owned(),
        created_at: "2026-02-16T12:13:00Z".to_owned(),
    }) {
        Ok(_) => panic!("expected duplicate worktree path to fail"),
        Err(err) => err,
    };

    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("worktree path '/tmp/orchestrator/shared-worktree'"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn upsert_worktree_rejects_reusing_a_worktree_id_for_another_work_item() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket_a = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "914"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "914".to_owned(),
        identifier: "AP-914".to_owned(),
        title: "Ticket A".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:20:00Z".to_owned(),
    };
    let ticket_b = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "915"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "915".to_owned(),
        identifier: "AP-915".to_owned(),
        title: "Ticket B".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:21:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket_a).expect("upsert ticket a");
    store.upsert_ticket(&ticket_b).expect("upsert ticket b");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_a.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-914"),
        })
        .expect("map ticket a");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_b.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-915"),
        })
        .expect("map ticket b");

    store
        .upsert_worktree(&WorktreeRecord {
            worktree_id: WorktreeId::new("wt-shared"),
            work_item_id: WorkItemId::new("wi-914"),
            path: "/tmp/orchestrator/worktree-914".to_owned(),
            branch: "ap/AP-914".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T12:22:00Z".to_owned(),
        })
        .expect("insert first worktree");

    let err = match store.upsert_worktree(&WorktreeRecord {
        worktree_id: WorktreeId::new("wt-shared"),
        work_item_id: WorkItemId::new("wi-915"),
        path: "/tmp/orchestrator/worktree-915".to_owned(),
        branch: "ap/AP-915".to_owned(),
        base_branch: "main".to_owned(),
        created_at: "2026-02-16T12:23:00Z".to_owned(),
    }) {
        Ok(_) => panic!("expected duplicate worktree id to fail"),
        Err(err) => err,
    };

    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("worktree_id 'wt-shared'"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn upsert_session_rejects_reusing_a_workdir_for_another_work_item() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket_a = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "912"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "912".to_owned(),
        identifier: "AP-912".to_owned(),
        title: "Ticket A".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:14:00Z".to_owned(),
    };
    let ticket_b = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "913"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "913".to_owned(),
        identifier: "AP-913".to_owned(),
        title: "Ticket B".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:15:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket_a).expect("upsert ticket a");
    store.upsert_ticket(&ticket_b).expect("upsert ticket b");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_a.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-912"),
        })
        .expect("map ticket a");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket_b.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-913"),
        })
        .expect("map ticket b");

    store
        .upsert_session(&SessionRecord {
            session_id: WorkerSessionId::new("sess-912"),
            work_item_id: WorkItemId::new("wi-912"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/shared-workdir".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T12:16:00Z".to_owned(),
            updated_at: "2026-02-16T12:16:30Z".to_owned(),
        })
        .expect("insert first session");

    let err = match store.upsert_session(&SessionRecord {
        session_id: WorkerSessionId::new("sess-913"),
        work_item_id: WorkItemId::new("wi-913"),
        backend_kind: BackendKind::OpenCode,
        workdir: "/tmp/orchestrator/shared-workdir".to_owned(),
        model: Some("gpt-5-codex".to_owned()),
        status: WorkerSessionStatus::Running,
        created_at: "2026-02-16T12:17:00Z".to_owned(),
        updated_at: "2026-02-16T12:17:30Z".to_owned(),
    }) {
        Ok(_) => panic!("expected duplicate workdir to fail"),
        Err(err) => err,
    };

    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("session workdir '/tmp/orchestrator/shared-workdir'"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn upsert_worktree_rejects_path_that_disagrees_with_existing_session_workdir() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "916"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "916".to_owned(),
        identifier: "AP-916".to_owned(),
        title: "Ticket mismatch".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:24:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket).expect("upsert ticket");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-916"),
        })
        .expect("map ticket");

    store
        .upsert_session(&SessionRecord {
            session_id: WorkerSessionId::new("sess-916"),
            work_item_id: WorkItemId::new("wi-916"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-916-session".to_owned(),
            model: None,
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T12:24:10Z".to_owned(),
            updated_at: "2026-02-16T12:24:20Z".to_owned(),
        })
        .expect("insert session");

    let err = match store.upsert_worktree(&WorktreeRecord {
        worktree_id: WorktreeId::new("wt-916"),
        work_item_id: WorkItemId::new("wi-916"),
        path: "/tmp/orchestrator/wt-916-worktree".to_owned(),
        branch: "ap/AP-916".to_owned(),
        base_branch: "main".to_owned(),
        created_at: "2026-02-16T12:24:30Z".to_owned(),
    }) {
        Ok(_) => panic!("expected worktree/session path mismatch to fail"),
        Err(err) => err,
    };

    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("does not match existing session workdir"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn upsert_session_rejects_workdir_that_disagrees_with_existing_worktree_path() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "917"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "917".to_owned(),
        identifier: "AP-917".to_owned(),
        title: "Ticket mismatch".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T12:25:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket).expect("upsert ticket");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket.ticket_id.clone(),
            work_item_id: WorkItemId::new("wi-917"),
        })
        .expect("map ticket");

    store
        .upsert_worktree(&WorktreeRecord {
            worktree_id: WorktreeId::new("wt-917"),
            work_item_id: WorkItemId::new("wi-917"),
            path: "/tmp/orchestrator/wt-917-worktree".to_owned(),
            branch: "ap/AP-917".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T12:25:10Z".to_owned(),
        })
        .expect("insert worktree");

    let err = match store.upsert_session(&SessionRecord {
        session_id: WorkerSessionId::new("sess-917"),
        work_item_id: WorkItemId::new("wi-917"),
        backend_kind: BackendKind::OpenCode,
        workdir: "/tmp/orchestrator/wt-917-session".to_owned(),
        model: None,
        status: WorkerSessionStatus::Running,
        created_at: "2026-02-16T12:25:20Z".to_owned(),
        updated_at: "2026-02-16T12:25:30Z".to_owned(),
    }) {
        Ok(_) => panic!("expected session/worktree path mismatch to fail"),
        Err(err) => err,
    };

    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("does not match existing worktree path"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn upsert_ticket_rejects_non_canonical_ticket_identity() {
    let store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket = TicketRecord {
        ticket_id: TicketId::from("linear:not-900"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "900".to_owned(),
        identifier: "AP-900".to_owned(),
        title: "Bad identity".to_owned(),
        state: "todo".to_owned(),
        updated_at: "2026-02-16T07:10:00Z".to_owned(),
    };

    let err = match store.upsert_ticket(&ticket) {
        Ok(_) => panic!("expected canonical identity validation failure"),
        Err(err) => err,
    };
    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("does not match canonical id 'linear:900'"))
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn runtime_mapping_upsert_rejects_mismatched_session_workdir_without_partial_write() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let mapping = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "904"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "904".to_owned(),
            identifier: "AP-904".to_owned(),
            title: "Mismatched path".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-16T11:30:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-mismatch"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-mismatch"),
            work_item_id: WorkItemId::new("wi-mismatch"),
            path: "/tmp/orchestrator/wt-mismatch".to_owned(),
            branch: "ap/AP-904-mismatch".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T11:30:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-mismatch"),
            work_item_id: WorkItemId::new("wi-mismatch"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/DIFFERENT".to_owned(),
            model: None,
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T11:30:20Z".to_owned(),
            updated_at: "2026-02-16T11:30:30Z".to_owned(),
        },
    };

    let err = match store.upsert_runtime_mapping(&mapping) {
        Ok(_) => panic!("expected workdir/path mismatch failure"),
        Err(err) => err,
    };
    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("session.workdir does not match worktree.path"))
        }
        other => panic!("unexpected error variant: {other:?}"),
    }

    let persisted = store
        .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "904")
        .expect("lookup mapping");
    assert!(
        persisted.is_none(),
        "failed write should not persist partial rows"
    );
}

#[test]
fn runtime_mapping_round_trip_survives_restart_and_prevents_duplicate_resume_entries() {
    let db = unique_db("runtime-resume");

    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "900"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "900".to_owned(),
        identifier: "AP-98".to_owned(),
        title: "Persist runtime mapping".to_owned(),
        state: "in_progress".to_owned(),
        updated_at: "2026-02-16T08:00:00Z".to_owned(),
    };
    let mapping = RuntimeMappingRecord {
        ticket: ticket.clone(),
        work_item_id: WorkItemId::new("wi-resume-1"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-resume-1"),
            work_item_id: WorkItemId::new("wi-resume-1"),
            path: "/tmp/orchestrator/wt-resume-1".to_owned(),
            branch: "ap/AP-98-persist-mapping".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T08:01:00Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-resume-1"),
            work_item_id: WorkItemId::new("wi-resume-1"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-resume-1".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T08:01:10Z".to_owned(),
            updated_at: "2026-02-16T08:02:00Z".to_owned(),
        },
    };

    let mut writer = SqliteEventStore::open(db.path()).expect("open store");
    writer
        .upsert_runtime_mapping(&mapping)
        .expect("insert runtime mapping");
    writer
        .upsert_runtime_mapping(&mapping)
        .expect("idempotent re-upsert should not duplicate");
    drop(writer);

    let reopened = SqliteEventStore::open(db.path()).expect("reopen store");
    let resolved = reopened
        .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "900")
        .expect("lookup runtime mapping")
        .expect("runtime mapping exists");
    assert_eq!(resolved, mapping);
    let resumed = reopened
        .find_inflight_runtime_mapping_by_ticket(&TicketProvider::Linear, "900")
        .expect("lookup inflight runtime mapping")
        .expect("inflight runtime mapping exists");
    assert_eq!(resumed, mapping);

    let all = reopened
        .list_runtime_mappings()
        .expect("list runtime mappings");
    assert_eq!(all, vec![mapping.clone()]);
    let inflight = reopened
        .list_inflight_runtime_mappings()
        .expect("list inflight runtime mappings");
    assert_eq!(inflight, vec![mapping.clone()]);

    let worktree = reopened
        .find_worktree_by_work_item(&WorkItemId::new("wi-resume-1"))
        .expect("lookup worktree")
        .expect("worktree exists");
    assert_eq!(worktree.path, "/tmp/orchestrator/wt-resume-1");

    let session = reopened
        .find_session_by_work_item(&WorkItemId::new("wi-resume-1"))
        .expect("lookup session")
        .expect("session exists");
    assert_eq!(session.status, WorkerSessionStatus::Running);
}

#[test]
fn migration_from_schema_v1_adds_runtime_mapping_tables() {
    let db = unique_db("schema-v1-upgrade");

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite");
    conn.execute_batch(
        "
        CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );

        CREATE TABLE tickets (
            ticket_id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            provider_ticket_id TEXT NOT NULL,
            identifier TEXT NOT NULL,
            title TEXT NOT NULL,
            state TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(provider, provider_ticket_id)
        );

        CREATE TABLE work_items (
            work_item_id TEXT PRIMARY KEY,
            ticket_id TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            FOREIGN KEY(ticket_id) REFERENCES tickets(ticket_id)
        );

        CREATE TABLE events (
            event_id TEXT PRIMARY KEY,
            sequence INTEGER NOT NULL UNIQUE,
            occurred_at TEXT NOT NULL,
            work_item_id TEXT,
            session_id TEXT,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL,
            schema_version INTEGER NOT NULL
        );

        CREATE TABLE artifacts (
            artifact_id TEXT PRIMARY KEY,
            work_item_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            storage_ref TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(work_item_id) REFERENCES work_items(work_item_id)
        );

        CREATE TABLE event_artifact_refs (
            event_id TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            PRIMARY KEY (event_id, artifact_id),
            FOREIGN KEY(event_id) REFERENCES events(event_id),
            FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
        );

        INSERT INTO schema_migrations (version, applied_at) VALUES (1, '2026-02-16T00:00:00Z');
        ",
    )
    .expect("seed schema v1");
    drop(conn);

    let store = SqliteEventStore::open(db.path()).expect("open and migrate");
    assert_eq!(store.schema_version().expect("schema version"), 5);
    drop(store);

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite for inspection");
    let tables = ["worktrees", "sessions", "project_repositories", "harness_session_bindings"];
    for table in tables {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .optional()
            .expect("query table existence");
        assert_eq!(exists, Some(1), "missing table {table}");
    }

    let migration_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(migration_count, 5);

    let indexes = ["idx_worktrees_path_unique", "idx_sessions_workdir_unique"];
    for index in indexes {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
                rusqlite::params![index],
                |row| row.get(0),
            )
            .optional()
            .expect("query index existence");
        assert_eq!(exists, Some(1), "missing index {index}");
    }
}

#[test]
fn inflight_runtime_mapping_queries_exclude_terminal_sessions() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let running = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "901"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "901".to_owned(),
            identifier: "AP-901".to_owned(),
            title: "Running ticket".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-16T09:00:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-running"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-running"),
            work_item_id: WorkItemId::new("wi-running"),
            path: "/tmp/orchestrator/wt-running".to_owned(),
            branch: "ap/AP-901-running".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T09:00:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-running"),
            work_item_id: WorkItemId::new("wi-running"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-running".to_owned(),
            model: None,
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T09:00:20Z".to_owned(),
            updated_at: "2026-02-16T09:00:30Z".to_owned(),
        },
    };
    let done = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "902"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "902".to_owned(),
            identifier: "AP-902".to_owned(),
            title: "Done ticket".to_owned(),
            state: "done".to_owned(),
            updated_at: "2026-02-16T10:00:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-done"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-done"),
            work_item_id: WorkItemId::new("wi-done"),
            path: "/tmp/orchestrator/wt-done".to_owned(),
            branch: "ap/AP-902-done".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T10:00:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-done"),
            work_item_id: WorkItemId::new("wi-done"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-done".to_owned(),
            model: None,
            status: WorkerSessionStatus::Done,
            created_at: "2026-02-16T10:00:20Z".to_owned(),
            updated_at: "2026-02-16T10:00:30Z".to_owned(),
        },
    };

    store
        .upsert_runtime_mapping(&running)
        .expect("upsert running mapping");
    store
        .upsert_runtime_mapping(&done)
        .expect("upsert done mapping");

    let inflight = store
        .list_inflight_runtime_mappings()
        .expect("list inflight mappings");
    assert_eq!(inflight, vec![running.clone()]);

    let done_lookup = store
        .find_inflight_runtime_mapping_by_ticket(&TicketProvider::Linear, "902")
        .expect("lookup done mapping as inflight");
    assert!(
        done_lookup.is_none(),
        "done session should not be resumable"
    );
}

#[test]
fn runtime_mapping_upsert_rejects_reassigning_session_or_worktree_for_existing_work_item() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let original = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "903"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "903".to_owned(),
            identifier: "AP-903".to_owned(),
            title: "Original mapping".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-replace"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-original"),
            work_item_id: WorkItemId::new("wi-replace"),
            path: "/tmp/orchestrator/wt-original".to_owned(),
            branch: "ap/AP-903-original".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T11:00:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-original"),
            work_item_id: WorkItemId::new("wi-replace"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-original".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T11:00:20Z".to_owned(),
            updated_at: "2026-02-16T11:01:00Z".to_owned(),
        },
    };
    store
        .upsert_runtime_mapping(&original)
        .expect("upsert original mapping");

    let replacement = RuntimeMappingRecord {
        ticket: TicketRecord {
            state: "in_progress".to_owned(),
            updated_at: "2026-02-16T11:05:00Z".to_owned(),
            ..original.ticket.clone()
        },
        work_item_id: original.work_item_id.clone(),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-replacement"),
            work_item_id: original.work_item_id.clone(),
            path: "/tmp/orchestrator/wt-replacement".to_owned(),
            branch: "ap/AP-903-replacement".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T11:04:00Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-replacement"),
            work_item_id: original.work_item_id.clone(),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-replacement".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T11:04:10Z".to_owned(),
            updated_at: "2026-02-16T11:05:00Z".to_owned(),
        },
    };

    let err = match store.upsert_runtime_mapping(&replacement) {
        Ok(_) => panic!("expected reassignment for existing mapping to fail"),
        Err(err) => err,
    };
    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("refusing reassignment"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }

    let found = store
        .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "903")
        .expect("lookup mapping")
        .expect("mapping exists");
    assert_eq!(found, original);

    let all = store
        .list_runtime_mappings()
        .expect("list runtime mappings");
    assert_eq!(all.len(), 1, "reassignment should not create duplicates");
}

#[test]
fn runtime_mapping_upsert_allows_status_and_ticket_metadata_updates_without_rebinding() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let original = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "903a"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "903a".to_owned(),
            identifier: "AP-903A".to_owned(),
            title: "Original mapping".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-stable-updates"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-stable-updates"),
            work_item_id: WorkItemId::new("wi-stable-updates"),
            path: "/tmp/orchestrator/wt-stable-updates".to_owned(),
            branch: "ap/AP-903A-stable-updates".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T11:00:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-stable-updates"),
            work_item_id: WorkItemId::new("wi-stable-updates"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-stable-updates".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T11:00:20Z".to_owned(),
            updated_at: "2026-02-16T11:01:00Z".to_owned(),
        },
    };
    store
        .upsert_runtime_mapping(&original)
        .expect("upsert original mapping");

    let updated = RuntimeMappingRecord {
        ticket: TicketRecord {
            title: "Original mapping (renamed)".to_owned(),
            state: "blocked".to_owned(),
            updated_at: "2026-02-16T11:10:00Z".to_owned(),
            ..original.ticket.clone()
        },
        work_item_id: original.work_item_id.clone(),
        worktree: original.worktree.clone(),
        session: SessionRecord {
            status: WorkerSessionStatus::WaitingForUser,
            updated_at: "2026-02-16T11:10:10Z".to_owned(),
            ..original.session.clone()
        },
    };
    store
        .upsert_runtime_mapping(&updated)
        .expect("status/metadata-only update should succeed");

    let found = store
        .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "903a")
        .expect("lookup mapping")
        .expect("mapping exists");
    assert_eq!(found, updated);
}

#[test]
fn runtime_mapping_upsert_rejects_rebinding_ticket_to_new_work_item() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let first = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "905"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "905".to_owned(),
            identifier: "AP-905".to_owned(),
            title: "Stable mapping".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-16T11:40:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-stable"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-stable"),
            work_item_id: WorkItemId::new("wi-stable"),
            path: "/tmp/orchestrator/wt-stable".to_owned(),
            branch: "ap/AP-905-stable".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T11:40:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-stable"),
            work_item_id: WorkItemId::new("wi-stable"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-stable".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T11:40:20Z".to_owned(),
            updated_at: "2026-02-16T11:41:00Z".to_owned(),
        },
    };
    store
        .upsert_runtime_mapping(&first)
        .expect("upsert first mapping");

    let conflicting = RuntimeMappingRecord {
        ticket: first.ticket.clone(),
        work_item_id: WorkItemId::new("wi-conflict"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-conflict"),
            work_item_id: WorkItemId::new("wi-conflict"),
            path: "/tmp/orchestrator/wt-conflict".to_owned(),
            branch: "ap/AP-905-conflict".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-16T11:41:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-conflict"),
            work_item_id: WorkItemId::new("wi-conflict"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-conflict".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-16T11:41:20Z".to_owned(),
            updated_at: "2026-02-16T11:42:00Z".to_owned(),
        },
    };

    let err = match store.upsert_runtime_mapping(&conflicting) {
        Ok(_) => panic!("expected ticket/work item rebinding failure"),
        Err(err) => err,
    };
    match err {
        CoreError::Persistence(message) => {
            assert!(message.contains("ticket_id 'linear:905' is already mapped"))
        }
        other => panic!("unexpected error variant: {other:?}"),
    }

    let found = store
        .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "905")
        .expect("lookup first mapping")
        .expect("first mapping still exists");
    assert_eq!(found, first);
}

#[test]
fn event_artifact_references_persist_and_resolve_after_restart() {
    let db = unique_db("artifact-refs");

    let mut store = SqliteEventStore::open(db.path()).expect("open store");
    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "456"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "456".to_owned(),
        identifier: "ORCH-456".to_owned(),
        title: "Artifact refs".to_owned(),
        state: "todo".to_owned(),
        updated_at: "2026-02-16T01:00:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket).expect("upsert ticket");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket.ticket_id,
            work_item_id: WorkItemId::new("wi-art-1"),
        })
        .expect("map work item");

    let artifact_z = ArtifactRecord {
        artifact_id: ArtifactId::new("artifact-z"),
        work_item_id: WorkItemId::new("wi-art-1"),
        kind: ArtifactKind::Diff,
        metadata: serde_json::json!({"name": "patch.diff"}),
        storage_ref: "artifact://patches/z".to_owned(),
        created_at: "2026-02-16T01:01:00Z".to_owned(),
    };
    let artifact_a = ArtifactRecord {
        artifact_id: ArtifactId::new("artifact-a"),
        work_item_id: WorkItemId::new("wi-art-1"),
        kind: ArtifactKind::LogSnippet,
        metadata: serde_json::json!({"lines": 20}),
        storage_ref: "artifact://logs/a".to_owned(),
        created_at: "2026-02-16T01:01:30Z".to_owned(),
    };
    store
        .create_artifact(&artifact_z)
        .expect("create artifact z");
    store
        .create_artifact(&artifact_a)
        .expect("create artifact a");

    store
        .append_event(
            NewEventEnvelope {
                event_id: "evt-art-1".to_owned(),
                occurred_at: "2026-02-16T01:02:00Z".to_owned(),
                work_item_id: Some(WorkItemId::new("wi-art-1")),
                session_id: Some(WorkerSessionId::new("sess-art-1")),
                payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                    artifact_id: ArtifactId::new("artifact-z"),
                    work_item_id: WorkItemId::new("wi-art-1"),
                    kind: ArtifactKind::Diff,
                    label: "patch z".to_owned(),
                    uri: "artifact://patches/z".to_owned(),
                }),
                schema_version: 1,
            },
            &[ArtifactId::new("artifact-z"), ArtifactId::new("artifact-a")],
        )
        .expect("append event with refs");

    drop(store);

    let reopened = SqliteEventStore::open(db.path()).expect("reopen store");
    let events = reopened
        .read_event_with_artifacts(&WorkItemId::new("wi-art-1"))
        .expect("read event with refs");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].artifact_ids,
        vec![ArtifactId::new("artifact-z"), ArtifactId::new("artifact-a")]
    );

    let restored_z = reopened
        .get_artifact(&ArtifactId::new("artifact-z"))
        .expect("get artifact")
        .expect("artifact exists");
    assert_eq!(restored_z.storage_ref, "artifact://patches/z");
}

#[test]
fn multi_table_event_write_is_transactional() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    let ticket = TicketRecord {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "789"),
        provider: TicketProvider::Linear,
        provider_ticket_id: "789".to_owned(),
        identifier: "ORCH-789".to_owned(),
        title: "txn test".to_owned(),
        state: "todo".to_owned(),
        updated_at: "2026-02-16T02:00:00Z".to_owned(),
    };
    store.upsert_ticket(&ticket).expect("upsert ticket");
    store
        .map_ticket_to_work_item(&TicketWorkItemMapping {
            ticket_id: ticket.ticket_id,
            work_item_id: WorkItemId::new("wi-txn-1"),
        })
        .expect("map work item");

    // Do not create artifact row. FK violation should fail and rollback event insert.
    let result = store.append_event(
        sample_event(
            "evt-txn-1",
            OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                work_item_id: WorkItemId::new("wi-txn-1"),
                from: WorkflowState::Planning,
                to: WorkflowState::Implementing,
                reason: None,
            }),
        ),
        &[ArtifactId::new("missing-artifact")],
    );

    assert!(result.is_err());
    let events = store.read_ordered().expect("read ordered after failed tx");
    assert!(
        events.is_empty(),
        "event row should rollback on join-table failure"
    );
}

#[test]
fn forward_compat_schema_guard_returns_typed_error() {
    let db = unique_db("forward-compat");
    let future_schema_version: u32 = 999;

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite");
    conn.execute_batch(
        "
        CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        INSERT INTO schema_migrations (version, applied_at) VALUES (999, '2026-02-16T00:00:00Z');
        ",
    )
    .expect("seed future migration");
    drop(conn);

    let err = match SqliteEventStore::open(db.path()) {
        Ok(_) => panic!("expected forward compat failure"),
        Err(err) => err,
    };
    match err {
        CoreError::UnsupportedSchemaVersion { supported, found } => {
            assert_eq!(found, future_schema_version);
            assert!(supported < future_schema_version);
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn ticket_query_limit_round_trips_as_fixed_width_integer() {
    let query = TicketQuery {
        assigned_to_me: true,
        states: vec!["In Progress".to_owned(), "Todo".to_owned()],
        search: Some("provider surface".to_owned()),
        limit: Some(50),
    };

    let value = serde_json::to_value(&query).expect("serialize ticket query");
    assert_eq!(value["limit"], serde_json::json!(50));

    let parsed: TicketQuery = serde_json::from_value(value).expect("deserialize ticket query");
    assert_eq!(parsed, query);
}

#[test]
fn adapter_defaults_are_stable() {
    assert_eq!(
        BackendCapabilities::default(),
        BackendCapabilities {
            structured_events: false,
            session_export: false,
            diff_provider: false,
            supports_background: false,
        }
    );
    let reviewers = ReviewerRequest::default();
    assert!(reviewers.users.is_empty());
    assert!(reviewers.teams.is_empty());
}

#[test]
fn worker_session_id_converts_to_runtime_session_id_and_back() {
    let core_id = WorkerSessionId::new("sess-runtime-bridge");
    let runtime_id = RuntimeSessionId::from(core_id.clone());
    let round_trip = WorkerSessionId::from(runtime_id);

    assert_eq!(round_trip, core_id);
}

#[test]
fn runtime_error_converts_into_core_error_without_losing_variant() {
    let error: CoreError = RuntimeError::Protocol("tag parse failed".to_owned()).into();

    match error {
        CoreError::Runtime(RuntimeError::Protocol(reason)) => {
            assert_eq!(reason, "tag parse failed");
        }
        other => panic!("unexpected core error variant: {other:?}"),
    }
}

struct EmptyWorkerStream;

#[async_trait]
impl WorkerEventSubscription for EmptyWorkerStream {
    async fn next_event(&mut self) -> Result<Option<BackendEvent>, RuntimeError> {
        Ok(None)
    }
}

struct EmptyLlmStream;

#[async_trait]
impl LlmResponseSubscription for EmptyLlmStream {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
        Ok(None)
    }
}

#[tokio::test]
async fn adapter_stream_type_aliases_are_object_safe() {
    let mut worker_stream: WorkerEventStream = Box::new(EmptyWorkerStream);
    assert!(worker_stream
        .next_event()
        .await
        .expect("worker stream poll")
        .is_none());

    let mut llm_stream: LlmResponseStream = Box::new(EmptyLlmStream);
    assert!(llm_stream
        .next_chunk()
        .await
        .expect("llm stream poll")
        .is_none());
}
