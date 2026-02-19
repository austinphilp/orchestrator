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
    let tables = [
        "worktrees",
        "sessions",
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
