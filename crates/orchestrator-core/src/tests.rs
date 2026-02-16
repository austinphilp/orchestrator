use async_trait::async_trait;
use rusqlite::OptionalExtension;

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

fn unique_db_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("duration")
        .as_nanos();
    std::env::temp_dir().join(format!("orchestrator-{tag}-{nanos}.db"))
}

#[test]
fn initialization_creates_required_schema_and_version() {
    let path = unique_db_path("init");
    let _ = std::fs::remove_file(&path);

    let store = SqliteEventStore::open(&path).expect("open store");
    assert_eq!(store.schema_version().expect("schema version"), 2);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite for inspection");
    let tables = [
        "schema_migrations",
        "tickets",
        "work_items",
        "worktrees",
        "sessions",
        "artifacts",
        "events",
        "event_artifact_refs",
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
        "idx_sessions_work_item_lookup",
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
    assert_eq!(applied_migrations, 2);

    drop(store);
    let _ = std::fs::remove_file(path);
}

#[test]
fn startup_is_idempotent_and_does_not_duplicate_migrations() {
    let path = unique_db_path("idempotent");
    let _ = std::fs::remove_file(&path);

    let first = SqliteEventStore::open(&path).expect("first open");
    assert_eq!(first.schema_version().expect("schema version"), 2);
    drop(first);

    let second = SqliteEventStore::open(&path).expect("second open");
    assert_eq!(second.schema_version().expect("schema version"), 2);
    drop(second);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite for inspection");
    let migration_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(migration_count, 2);

    let _ = std::fs::remove_file(path);
}

#[test]
fn startup_adopts_legacy_events_schema_without_recreating_events_table() {
    let path = unique_db_path("legacy-schema");
    let _ = std::fs::remove_file(&path);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite");
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

    let store = SqliteEventStore::open(&path).expect("open store");
    assert_eq!(store.schema_version().expect("schema version"), 2);

    let events = store.read_ordered().expect("read ordered");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, "evt-legacy-1");
    drop(store);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite for inspection");
    let tables = [
        "schema_migrations",
        "tickets",
        "work_items",
        "worktrees",
        "sessions",
        "artifacts",
        "event_artifact_refs",
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

    let _ = std::fs::remove_file(path);
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
    let path = unique_db_path("runtime-resume");
    let _ = std::fs::remove_file(&path);

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

    let mut writer = SqliteEventStore::open(&path).expect("open store");
    writer
        .upsert_runtime_mapping(&mapping)
        .expect("insert runtime mapping");
    writer
        .upsert_runtime_mapping(&mapping)
        .expect("idempotent re-upsert should not duplicate");
    drop(writer);

    let reopened = SqliteEventStore::open(&path).expect("reopen store");
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

    let _ = std::fs::remove_file(path);
}

#[test]
fn migration_from_schema_v1_adds_runtime_mapping_tables() {
    let path = unique_db_path("schema-v1-upgrade");
    let _ = std::fs::remove_file(&path);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite");
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

    let store = SqliteEventStore::open(&path).expect("open and migrate");
    assert_eq!(store.schema_version().expect("schema version"), 2);
    drop(store);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite for inspection");
    let tables = ["worktrees", "sessions"];
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
    assert_eq!(migration_count, 2);

    let _ = std::fs::remove_file(path);
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
fn runtime_mapping_upsert_replaces_session_and_worktree_for_existing_work_item() {
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
    store
        .upsert_runtime_mapping(&replacement)
        .expect("upsert replacement mapping");

    let found = store
        .find_runtime_mapping_by_ticket(&TicketProvider::Linear, "903")
        .expect("lookup mapping")
        .expect("mapping exists");
    assert_eq!(found, replacement);

    let all = store
        .list_runtime_mappings()
        .expect("list runtime mappings");
    assert_eq!(all.len(), 1, "replacement should update in place");
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
    let path = unique_db_path("artifact-refs");
    let _ = std::fs::remove_file(&path);

    let mut store = SqliteEventStore::open(&path).expect("open store");
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

    let artifact_a = ArtifactRecord {
        artifact_id: ArtifactId::new("artifact-a"),
        work_item_id: WorkItemId::new("wi-art-1"),
        kind: ArtifactKind::Diff,
        metadata: serde_json::json!({"name": "patch.diff"}),
        storage_ref: "artifact://patches/a".to_owned(),
        created_at: "2026-02-16T01:01:00Z".to_owned(),
    };
    let artifact_b = ArtifactRecord {
        artifact_id: ArtifactId::new("artifact-b"),
        work_item_id: WorkItemId::new("wi-art-1"),
        kind: ArtifactKind::LogSnippet,
        metadata: serde_json::json!({"lines": 20}),
        storage_ref: "artifact://logs/b".to_owned(),
        created_at: "2026-02-16T01:01:30Z".to_owned(),
    };
    store
        .create_artifact(&artifact_a)
        .expect("create artifact a");
    store
        .create_artifact(&artifact_b)
        .expect("create artifact b");

    store
        .append_event(
            NewEventEnvelope {
                event_id: "evt-art-1".to_owned(),
                occurred_at: "2026-02-16T01:02:00Z".to_owned(),
                work_item_id: Some(WorkItemId::new("wi-art-1")),
                session_id: Some(WorkerSessionId::new("sess-art-1")),
                payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                    artifact_id: ArtifactId::new("artifact-a"),
                    work_item_id: WorkItemId::new("wi-art-1"),
                    kind: ArtifactKind::Diff,
                    label: "patch a".to_owned(),
                    uri: "artifact://patches/a".to_owned(),
                }),
                schema_version: 1,
            },
            &[ArtifactId::new("artifact-a"), ArtifactId::new("artifact-b")],
        )
        .expect("append event with refs");

    drop(store);

    let reopened = SqliteEventStore::open(&path).expect("reopen store");
    let events = reopened
        .read_event_with_artifacts(&WorkItemId::new("wi-art-1"))
        .expect("read event with refs");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].artifact_ids.len(), 2);
    assert!(events[0]
        .artifact_ids
        .contains(&ArtifactId::new("artifact-a")));
    assert!(events[0]
        .artifact_ids
        .contains(&ArtifactId::new("artifact-b")));

    let restored_a = reopened
        .get_artifact(&ArtifactId::new("artifact-a"))
        .expect("get artifact")
        .expect("artifact exists");
    assert_eq!(restored_a.storage_ref, "artifact://patches/a");

    let _ = std::fs::remove_file(path);
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
    let path = unique_db_path("forward-compat");
    let _ = std::fs::remove_file(&path);

    let conn = rusqlite::Connection::open(&path).expect("open sqlite");
    conn.execute_batch(
        "
        CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        INSERT INTO schema_migrations (version, applied_at) VALUES (3, '2026-02-16T00:00:00Z');
        ",
    )
    .expect("seed future migration");
    drop(conn);

    let err = match SqliteEventStore::open(&path) {
        Ok(_) => panic!("expected forward compat failure"),
        Err(err) => err,
    };
    match err {
        CoreError::UnsupportedSchemaVersion { supported, found } => {
            assert_eq!(supported, 2);
            assert_eq!(found, 3);
        }
        other => panic!("unexpected error variant: {other:?}"),
    }

    let _ = std::fs::remove_file(path);
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

struct EmptyWorkerStream;

#[async_trait]
impl WorkerEventSubscription for EmptyWorkerStream {
    async fn next_event(&mut self) -> Result<Option<BackendEvent>, CoreError> {
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
