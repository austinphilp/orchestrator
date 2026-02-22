//! Canonical orchestration event schema/types boundary.
//!
//! During the app/core merge, orchestrator-app is the canonical import path for
//! event contracts while store/projection ownership remains in orchestrator-core.

pub use orchestrator_core::{
    ArtifactCreatedPayload, InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope,
    OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
    SessionCheckpointPayload, SessionCompletedPayload, SessionCrashedPayload,
    SessionNeedsInputPayload, SessionSpawnedPayload, StoredEventEnvelope,
    SupervisorQueryCancellationSource, SupervisorQueryCancelledPayload,
    SupervisorQueryChunkPayload, SupervisorQueryFinishedPayload, SupervisorQueryKind,
    SupervisorQueryStartedPayload, TicketDetailsSyncedPayload, TicketSyncedPayload,
    UserRespondedPayload, WorkItemCreatedPayload, WorkItemProfileOverrideClearedPayload,
    WorkItemProfileOverrideSetPayload, WorkflowTransitionPayload, WorktreeCreatedPayload,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalization::DOMAIN_EVENT_SCHEMA_VERSION;
    use orchestrator_core::{ArtifactId, WorkItemId, WorkerSessionId, WorkflowState};

    fn sample_event(event_id: &str, payload: OrchestrationEventPayload) -> NewEventEnvelope {
        NewEventEnvelope {
            event_id: event_id.to_owned(),
            occurred_at: "2026-02-15T14:00:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-1")),
            session_id: Some(WorkerSessionId::new("sess-1")),
            payload,
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
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
        assert_eq!(parsed.schema_version, DOMAIN_EVENT_SCHEMA_VERSION);
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

        let blocked_json = r#"{"type":"SessionBlocked","data":{"session_id":"sess-legacy","reason":"tests failing"}}"#;
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
        let workflow_payload: OrchestrationEventPayload = serde_json::from_str(workflow_json)
            .expect("deserialize legacy merging workflow transition");

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
}
