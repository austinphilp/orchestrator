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
fn workflow_transition_with_legacy_merging_state_deserializes_as_pending_merge() {
    let workflow_json = r#"{"type":"WorkflowTransition","data":{"work_item_id":"wi-legacy","from":"InReview","to":"Merging"}}"#;
    let workflow_payload: OrchestrationEventPayload =
        serde_json::from_str(workflow_json).expect("deserialize legacy merging workflow transition");

    match workflow_payload {
        OrchestrationEventPayload::WorkflowTransition(payload) => {
            assert_eq!(payload.work_item_id, WorkItemId::new("wi-legacy"));
            assert_eq!(payload.from, WorkflowState::InReview);
            assert_eq!(payload.to, WorkflowState::PendingMerge);
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
    let inbox_item_id = InboxItemId::new("inbox-1");
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
                "evt-inbox-created",
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_item_id.clone(),
                    work_item_id: WorkItemId::new("wi-1"),
                    kind: InboxItemKind::NeedsDecision,
                    title: "Need input".to_owned(),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            3,
            sample_event(
                "evt-session-completed",
                OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                    session_id: WorkerSessionId::new("sess-1"),
                    summary: Some("complete".to_owned()),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            4,
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
    let inbox_item = projection
        .inbox_items
        .get(&inbox_item_id)
        .expect("inbox item exists");
    assert!(inbox_item.resolved);
}

#[test]
fn inbox_items_created_after_session_end_are_auto_resolved() {
    let inbox_item_id = InboxItemId::new("inbox-after-done");
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
                    summary: Some("done".to_owned()),
                }),
            ),
        )),
        StoredEventEnvelope::from((
            3,
            sample_event(
                "evt-inbox-created-after-done",
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_item_id.clone(),
                    work_item_id: WorkItemId::new("wi-1"),
                    kind: InboxItemKind::Blocked,
                    title: "late error".to_owned(),
                }),
            ),
        )),
    ];

    let projection = rebuild_projection(&events);
    let inbox_item = projection
        .inbox_items
        .get(&inbox_item_id)
        .expect("inbox item exists");
    assert!(inbox_item.resolved);
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

fn seed_runtime_mapping(
    store: &mut SqliteEventStore,
    work_item_id: &str,
    session_id: &str,
    status: WorkerSessionStatus,
    updated_at: &str,
) {
    let ticket_suffix = work_item_id.trim_start_matches("wi-");
    let mapping = RuntimeMappingRecord {
        ticket: TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, ticket_suffix),
            provider: TicketProvider::Linear,
            provider_ticket_id: ticket_suffix.to_owned(),
            identifier: format!("AP-{ticket_suffix}"),
            title: format!("Ticket {ticket_suffix}"),
            state: "In Progress".to_owned(),
            updated_at: updated_at.to_owned(),
        },
        work_item_id: WorkItemId::new(work_item_id),
        worktree: WorktreeRecord {
            worktree_id: WorktreeId::new(format!("wt-{work_item_id}")),
            work_item_id: WorkItemId::new(work_item_id),
            path: format!("/tmp/{work_item_id}"),
            branch: format!("ap/{work_item_id}"),
            base_branch: "main".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        },
        session: SessionRecord {
            session_id: WorkerSessionId::new(session_id),
            work_item_id: WorkItemId::new(work_item_id),
            backend_kind: BackendKind::Codex,
            workdir: format!("/tmp/{work_item_id}"),
            model: Some("gpt-5-codex".to_owned()),
            status,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: updated_at.to_owned(),
        },
    };

    store
        .upsert_runtime_mapping(&mapping)
        .expect("seed runtime mapping");
}

#[test]
fn prune_deletes_old_completed_work_item_events_and_event_refs() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");

    seed_runtime_mapping(
        &mut store,
        "wi-old",
        "sess-old",
        WorkerSessionStatus::Done,
        "100.000000000Z",
    );
    seed_runtime_mapping(
        &mut store,
        "wi-active",
        "sess-active",
        WorkerSessionStatus::Running,
        "1990000.000000000Z",
    );

    store
        .append(NewEventEnvelope {
            event_id: "evt-old-1".to_owned(),
            occurred_at: "100.000000000Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-old")),
            session_id: Some(WorkerSessionId::new("sess-old")),
            payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(WorkerSessionId::new("sess-old")),
                work_item_id: Some(WorkItemId::new("wi-old")),
                message: "old event".to_owned(),
            }),
            schema_version: 1,
        })
        .expect("append old event");

    let old_artifact_id = ArtifactId::new("artifact-old-1");
    store
        .create_artifact(&ArtifactRecord {
            artifact_id: old_artifact_id.clone(),
            work_item_id: WorkItemId::new("wi-old"),
            kind: ArtifactKind::LogSnippet,
            metadata: serde_json::json!({"summary":"old"}),
            storage_ref: "artifact://old".to_owned(),
            created_at: "100.000000000Z".to_owned(),
        })
        .expect("create old artifact");
    store
        .append_event(
            NewEventEnvelope {
                event_id: "evt-old-2".to_owned(),
                occurred_at: "100.000000000Z".to_owned(),
                work_item_id: Some(WorkItemId::new("wi-old")),
                session_id: Some(WorkerSessionId::new("sess-old")),
                payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                    artifact_id: old_artifact_id.clone(),
                    work_item_id: WorkItemId::new("wi-old"),
                    kind: ArtifactKind::LogSnippet,
                    label: "old artifact".to_owned(),
                    uri: "artifact://old".to_owned(),
                }),
                schema_version: 1,
            },
            std::slice::from_ref(&old_artifact_id),
        )
        .expect("append old artifact event");

    store
        .append(NewEventEnvelope {
            event_id: "evt-active-1".to_owned(),
            occurred_at: "1990000.000000000Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-active")),
            session_id: Some(WorkerSessionId::new("sess-active")),
            payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(WorkerSessionId::new("sess-active")),
                work_item_id: Some(WorkItemId::new("wi-active")),
                message: "active event".to_owned(),
            }),
            schema_version: 1,
        })
        .expect("append active event");

    let report = store
        .prune_completed_session_events(EventPrunePolicy { retention_days: 14 }, 2_000_000)
        .expect("prune old sessions");

    assert_eq!(report.candidate_sessions, 1);
    assert_eq!(report.eligible_sessions, 1);
    assert_eq!(report.pruned_work_items, 1);
    assert_eq!(report.deleted_events, 2);
    assert_eq!(report.deleted_event_artifact_refs, 1);
    assert_eq!(report.skipped_invalid_timestamps, 0);

    assert_eq!(
        store
            .count_events(RetrievalScope::WorkItem(WorkItemId::new("wi-old")))
            .expect("count old work item"),
        0
    );
    assert_eq!(
        store
            .count_events(RetrievalScope::WorkItem(WorkItemId::new("wi-active")))
            .expect("count active work item"),
        1
    );
}

#[test]
fn prune_is_idempotent() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");
    seed_runtime_mapping(
        &mut store,
        "wi-old",
        "sess-old",
        WorkerSessionStatus::Crashed,
        "100.000000000Z",
    );
    store
        .append(NewEventEnvelope {
            event_id: "evt-old-1".to_owned(),
            occurred_at: "100.000000000Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-old")),
            session_id: Some(WorkerSessionId::new("sess-old")),
            payload: OrchestrationEventPayload::SessionCrashed(SessionCrashedPayload {
                session_id: WorkerSessionId::new("sess-old"),
                reason: "panic".to_owned(),
            }),
            schema_version: 1,
        })
        .expect("append old crash event");

    let first = store
        .prune_completed_session_events(EventPrunePolicy { retention_days: 14 }, 2_000_000)
        .expect("first prune");
    let second = store
        .prune_completed_session_events(EventPrunePolicy { retention_days: 14 }, 2_000_000)
        .expect("second prune");

    assert_eq!(first.deleted_events, 1);
    assert_eq!(second.deleted_events, 0);
    assert_eq!(second.pruned_work_items, 0);
}

#[test]
fn prune_skips_invalid_updated_at_without_deleting_events() {
    let mut store = SqliteEventStore::in_memory().expect("in-memory store");
    seed_runtime_mapping(
        &mut store,
        "wi-invalid",
        "sess-invalid",
        WorkerSessionStatus::Done,
        "not-a-timestamp",
    );
    store
        .append(NewEventEnvelope {
            event_id: "evt-invalid-1".to_owned(),
            occurred_at: "2026-02-15T14:00:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-invalid")),
            session_id: Some(WorkerSessionId::new("sess-invalid")),
            payload: OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                session_id: WorkerSessionId::new("sess-invalid"),
                summary: Some("done".to_owned()),
            }),
            schema_version: 1,
        })
        .expect("append event");

    let report = store
        .prune_completed_session_events(EventPrunePolicy { retention_days: 14 }, 2_000_000)
        .expect("prune should succeed");

    assert_eq!(report.skipped_invalid_timestamps, 1);
    assert_eq!(report.deleted_events, 0);
    assert_eq!(
        store
            .count_events(RetrievalScope::WorkItem(WorkItemId::new("wi-invalid")))
            .expect("count remaining events"),
        1
    );
}
