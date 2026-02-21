fn unique_db(tag: &str) -> TestDbPath {
    TestDbPath::new(&format!("core-tests-{tag}"))
}

#[test]
fn initialization_creates_required_schema_and_version() {
    let db = unique_db("init");

    let store = SqliteEventStore::open(db.path()).expect("open store");
    assert_eq!(store.schema_version().expect("schema version"), 8);

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite for inspection");
    let tables = [
        "schema_migrations",
        "tickets",
        "work_items",
        "worktrees",
        "sessions",
        "session_runtime_flags",
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
    assert_eq!(applied_migrations, 8);

    drop(store);
}

#[test]
fn startup_is_idempotent_and_does_not_duplicate_migrations() {
    let db = unique_db("idempotent");

    let first = SqliteEventStore::open(db.path()).expect("first open");
    assert_eq!(first.schema_version().expect("schema version"), 8);
    drop(first);

    let second = SqliteEventStore::open(db.path()).expect("second open");
    assert_eq!(second.schema_version().expect("schema version"), 8);
    drop(second);

    let conn = rusqlite::Connection::open(db.path()).expect("open sqlite for inspection");
    let migration_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(migration_count, 8);
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
    assert_eq!(store.schema_version().expect("schema version"), 8);

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
        "session_runtime_flags",
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
fn session_working_state_round_trips_and_defaults_to_false() {
    let db = unique_db("session-working-state");

    let mapping = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "990"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "990".to_owned(),
            identifier: "AP-990".to_owned(),
            title: "Persist working-state".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-20T08:00:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-working-1"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-working-1"),
            work_item_id: WorkItemId::new("wi-working-1"),
            path: "/tmp/orchestrator/wt-working-1".to_owned(),
            branch: "ap/AP-990-working-state".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-20T08:00:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-working-1"),
            work_item_id: WorkItemId::new("wi-working-1"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-working-1".to_owned(),
            model: None,
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-20T08:00:20Z".to_owned(),
            updated_at: "2026-02-20T08:00:30Z".to_owned(),
        },
    };

    let mut writer = SqliteEventStore::open(db.path()).expect("open store");
    writer
        .upsert_runtime_mapping(&mapping)
        .expect("insert runtime mapping");
    assert!(
        !writer
            .is_session_working(&mapping.session.session_id)
            .expect("default working state")
    );
    writer
        .set_session_working_state(&mapping.session.session_id, true)
        .expect("set working state true");
    assert!(
        writer
            .is_session_working(&mapping.session.session_id)
            .expect("working state after set")
    );
    writer
        .set_session_working_state(&WorkerSessionId::new("sess-not-in-db"), true)
        .expect("setting unknown session should be a no-op");
    drop(writer);

    let reopened = SqliteEventStore::open(db.path()).expect("reopen store");
    assert!(
        reopened
            .is_session_working(&mapping.session.session_id)
            .expect("working state persisted")
    );
    assert!(
        !reopened
            .is_session_working(&WorkerSessionId::new("sess-not-in-db"))
            .expect("unknown session defaults to false")
    );
}

#[test]
fn list_session_working_states_reports_defaults_and_updates() {
    let db = unique_db("session-working-state-list");

    let mut writer = SqliteEventStore::open(db.path()).expect("open store");
    let mapping_a = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "1001"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "1001".to_owned(),
            identifier: "AP-1001".to_owned(),
            title: "Working-state A".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-20T08:00:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-working-list-1"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-working-list-1"),
            work_item_id: WorkItemId::new("wi-working-list-1"),
            path: "/tmp/orchestrator/wt-working-list-1".to_owned(),
            branch: "ap/AP-1001".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-20T08:00:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-working-list-1"),
            work_item_id: WorkItemId::new("wi-working-list-1"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-working-list-1".to_owned(),
            model: None,
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-20T08:00:20Z".to_owned(),
            updated_at: "2026-02-20T08:00:30Z".to_owned(),
        },
    };
    let mapping_b = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "1002"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "1002".to_owned(),
            identifier: "AP-1002".to_owned(),
            title: "Working-state B".to_owned(),
            state: "in_progress".to_owned(),
            updated_at: "2026-02-20T08:01:00Z".to_owned(),
        },
        work_item_id: WorkItemId::new("wi-working-list-2"),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new("wt-working-list-2"),
            work_item_id: WorkItemId::new("wi-working-list-2"),
            path: "/tmp/orchestrator/wt-working-list-2".to_owned(),
            branch: "ap/AP-1002".to_owned(),
            base_branch: "main".to_owned(),
            created_at: "2026-02-20T08:01:10Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new("sess-working-list-2"),
            work_item_id: WorkItemId::new("wi-working-list-2"),
            backend_kind: BackendKind::OpenCode,
            workdir: "/tmp/orchestrator/wt-working-list-2".to_owned(),
            model: None,
            status: WorkerSessionStatus::Running,
            created_at: "2026-02-20T08:01:20Z".to_owned(),
            updated_at: "2026-02-20T08:01:30Z".to_owned(),
        },
    };
    writer
        .upsert_runtime_mapping(&mapping_a)
        .expect("insert runtime mapping a");
    writer
        .upsert_runtime_mapping(&mapping_b)
        .expect("insert runtime mapping b");
    writer
        .set_session_working_state(&mapping_a.session.session_id, true)
        .expect("set working state true");
    drop(writer);

    let reopened = SqliteEventStore::open(db.path()).expect("reopen store");
    let states = reopened
        .list_session_working_states()
        .expect("list working states");
    assert_eq!(states.get(&mapping_a.session.session_id), Some(&true));
    assert_eq!(states.get(&mapping_b.session.session_id), Some(&false));
}
