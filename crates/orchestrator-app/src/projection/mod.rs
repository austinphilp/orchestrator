//! Event replay/read-model materialization boundary.

pub use orchestrator_core::{
    ArtifactProjection, InboxItemProjection, ProjectionState, SessionProjection,
    SessionRuntimeProjection, WorkItemProjection,
};
use orchestrator_core::{
    OrchestrationEventPayload, RetrievalScope, StoredEventEnvelope, WorkItemId, WorkerSessionId,
    WorkerSessionStatus,
};

fn empty_work_item_projection(id: WorkItemId) -> WorkItemProjection {
    WorkItemProjection {
        id,
        ticket_id: None,
        project_id: None,
        workflow_state: None,
        profile_override: None,
        session_id: None,
        worktree_id: None,
        inbox_items: vec![],
        artifacts: vec![],
    }
}

pub fn apply_event(state: &mut ProjectionState, event: StoredEventEnvelope) {
    match &event.payload {
        OrchestrationEventPayload::WorkItemCreated(payload) => {
            let entry = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            entry.ticket_id = Some(payload.ticket_id.clone());
            entry.project_id = Some(payload.project_id.clone());
        }
        OrchestrationEventPayload::WorktreeCreated(payload) => {
            let entry = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            entry.worktree_id = Some(payload.worktree_id.clone());
        }
        OrchestrationEventPayload::SessionSpawned(payload) => {
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.session_id = Some(payload.session_id.clone());

            state.sessions.insert(
                payload.session_id.clone(),
                SessionProjection {
                    id: payload.session_id.clone(),
                    work_item_id: Some(payload.work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
        }
        OrchestrationEventPayload::SessionCheckpoint(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.latest_checkpoint = Some(payload.artifact_id.clone());
                session.status = Some(WorkerSessionStatus::Running);
            }
        }
        OrchestrationEventPayload::SessionNeedsInput(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::WaitingForUser);
            }
        }
        OrchestrationEventPayload::SessionBlocked(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::Blocked);
            }
        }
        OrchestrationEventPayload::SessionCompleted(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::Done);
            }
            resolve_inbox_items_for_session(state, &payload.session_id);
        }
        OrchestrationEventPayload::SessionCrashed(payload) => {
            if let Some(session) = state.sessions.get_mut(&payload.session_id) {
                session.status = Some(WorkerSessionStatus::Crashed);
            }
            resolve_inbox_items_for_session(state, &payload.session_id);
        }
        OrchestrationEventPayload::ArtifactCreated(payload) => {
            state.artifacts.insert(
                payload.artifact_id.clone(),
                ArtifactProjection {
                    id: payload.artifact_id.clone(),
                    work_item_id: payload.work_item_id.clone(),
                    kind: payload.kind.clone(),
                    label: payload.label.clone(),
                    uri: payload.uri.clone(),
                },
            );
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.artifacts.push(payload.artifact_id.clone());
        }
        OrchestrationEventPayload::WorkflowTransition(payload) => {
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.workflow_state = Some(payload.to.clone());
            state.orchestrator_status = Some(format!("{:?}", payload.to));
        }
        OrchestrationEventPayload::WorkItemProfileOverrideSet(payload) => {
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.profile_override = Some(payload.profile_name.clone());
        }
        OrchestrationEventPayload::WorkItemProfileOverrideCleared(payload) => {
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            work_item.profile_override = None;
        }
        OrchestrationEventPayload::InboxItemCreated(payload) => {
            let resolved = state
                .work_items
                .get(&payload.work_item_id)
                .and_then(|work_item| work_item.session_id.as_ref())
                .and_then(|session_id| state.sessions.get(session_id))
                .and_then(|session| session.status.as_ref())
                .map(session_has_ended)
                .unwrap_or(false);
            state.inbox_items.insert(
                payload.inbox_item_id.clone(),
                InboxItemProjection {
                    id: payload.inbox_item_id.clone(),
                    work_item_id: payload.work_item_id.clone(),
                    kind: payload.kind.clone(),
                    title: payload.title.clone(),
                    resolved,
                },
            );
            let work_item = state
                .work_items
                .entry(payload.work_item_id.clone())
                .or_insert_with(|| empty_work_item_projection(payload.work_item_id.clone()));
            if !work_item.inbox_items.contains(&payload.inbox_item_id) {
                work_item.inbox_items.push(payload.inbox_item_id.clone());
            }
        }
        OrchestrationEventPayload::InboxItemResolved(payload) => {
            if let Some(item) = state.inbox_items.get_mut(&payload.inbox_item_id) {
                item.resolved = true;
            }
        }
        OrchestrationEventPayload::TicketSynced(_)
        | OrchestrationEventPayload::TicketDetailsSynced(_)
        | OrchestrationEventPayload::UserResponded(_)
        | OrchestrationEventPayload::SupervisorQueryStarted(_)
        | OrchestrationEventPayload::SupervisorQueryChunk(_)
        | OrchestrationEventPayload::SupervisorQueryCancelled(_)
        | OrchestrationEventPayload::SupervisorQueryFinished(_) => {}
    }

    state.events.push(event);
}

fn resolve_inbox_items_for_session(state: &mut ProjectionState, session_id: &WorkerSessionId) {
    let work_item_id = state
        .sessions
        .get(session_id)
        .and_then(|session| session.work_item_id.as_ref())
        .cloned();
    let Some(work_item_id) = work_item_id else {
        return;
    };
    let inbox_items = state
        .work_items
        .get(&work_item_id)
        .map(|work_item| work_item.inbox_items.clone())
        .unwrap_or_default();
    for inbox_item_id in inbox_items {
        if let Some(item) = state.inbox_items.get_mut(&inbox_item_id) {
            item.resolved = true;
        }
    }
}

fn session_has_ended(status: &WorkerSessionStatus) -> bool {
    matches!(
        status,
        WorkerSessionStatus::Done | WorkerSessionStatus::Crashed
    )
}

pub fn rebuild_projection(events: &[StoredEventEnvelope]) -> ProjectionState {
    let mut state = ProjectionState::default();
    for event in events {
        apply_event(&mut state, event.clone());
    }
    state
}

pub fn retrieve_events(
    state: &ProjectionState,
    scope: RetrievalScope,
    limit: usize,
) -> Vec<StoredEventEnvelope> {
    if limit == 0 {
        return vec![];
    }

    let mut events: Vec<_> = state
        .events
        .iter()
        .filter(|event| match &scope {
            RetrievalScope::Global => true,
            RetrievalScope::WorkItem(id) => event.work_item_id.as_ref() == Some(id),
            RetrievalScope::Session(id) => event.session_id.as_ref() == Some(id),
        })
        .cloned()
        .collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.sequence));
    events.into_iter().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        InboxItemCreatedPayload, NewEventEnvelope, OrchestrationEventType, SessionCompletedPayload,
        SessionCrashedPayload, SessionSpawnedPayload, UserRespondedPayload, WorkItemCreatedPayload,
    };
    use orchestrator_core::{
        ArtifactId, InboxItemId, InboxItemKind, ProjectId, TicketId, WorkflowState,
        WorkflowTransitionPayload,
    };

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
    fn apply_event_matches_rebuild_projection() {
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
                        model: "gpt-5".to_owned(),
                    }),
                ),
            )),
            StoredEventEnvelope::from((
                3,
                sample_event(
                    "evt-3",
                    OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                        session_id: WorkerSessionId::new("sess-1"),
                        summary: Some("done".to_owned()),
                    }),
                ),
            )),
        ];

        let rebuilt = rebuild_projection(&events);
        let mut applied = ProjectionState::default();
        for event in &events {
            apply_event(&mut applied, event.clone());
        }

        assert_eq!(applied, rebuilt);
    }

    #[test]
    fn retrieve_events_is_scoped_ordered_and_limited() {
        let events = (1..=5)
            .map(|seq| StoredEventEnvelope {
                event_id: format!("evt-{seq}"),
                sequence: seq,
                occurred_at: "2026-02-15T14:00:00Z".to_owned(),
                work_item_id: Some(if seq <= 3 {
                    WorkItemId::new("wi-1")
                } else {
                    WorkItemId::new("wi-2")
                }),
                session_id: Some(if seq <= 3 {
                    WorkerSessionId::new("sess-1")
                } else {
                    WorkerSessionId::new("sess-2")
                }),
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

        let top2 = retrieve_events(&state, RetrievalScope::Global, 2);
        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0].sequence, 5);
        assert_eq!(top2[1].sequence, 4);

        let scoped = retrieve_events(
            &state,
            RetrievalScope::WorkItem(WorkItemId::new("wi-1")),
            10,
        );
        assert_eq!(scoped.len(), 3);
        assert!(scoped
            .iter()
            .all(|event| event.work_item_id == Some(WorkItemId::new("wi-1"))));
    }

    #[test]
    fn projection_resolves_inbox_after_terminal_session_events() {
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
    fn moved_projection_logic_matches_legacy_core_behavior() {
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
                        model: "gpt-5".to_owned(),
                    }),
                ),
            )),
            StoredEventEnvelope::from((
                3,
                sample_event(
                    "evt-3",
                    OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                        session_id: WorkerSessionId::new("sess-1"),
                        summary: Some("done".to_owned()),
                    }),
                ),
            )),
            StoredEventEnvelope::from((
                4,
                sample_event(
                    "evt-4",
                    OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                        inbox_item_id: InboxItemId::new("inbox-late"),
                        work_item_id: WorkItemId::new("wi-1"),
                        kind: InboxItemKind::Blocked,
                        title: "late blocked".to_owned(),
                    }),
                ),
            )),
            StoredEventEnvelope::from((
                5,
                sample_event(
                    "evt-5",
                    OrchestrationEventPayload::SessionCheckpoint(
                        orchestrator_core::SessionCheckpointPayload {
                            session_id: WorkerSessionId::new("sess-1"),
                            artifact_id: ArtifactId::new("artifact-1"),
                            summary: "checkpoint".to_owned(),
                        },
                    ),
                ),
            )),
        ];

        let app_projection = rebuild_projection(&events);
        let core_projection = orchestrator_core::rebuild_projection(&events);
        assert_eq!(app_projection, core_projection);

        let app_scoped = retrieve_events(
            &app_projection,
            RetrievalScope::Session(WorkerSessionId::new("sess-1")),
            3,
        );
        let core_scoped = orchestrator_core::retrieve_events(
            &core_projection,
            RetrievalScope::Session(WorkerSessionId::new("sess-1")),
            3,
        );
        assert_eq!(app_scoped, core_scoped);
    }
}
