use std::collections::HashSet;

use orchestrator_core::{
    ArtifactId, ArtifactKind, ArtifactRecord, CoreError, EventStore, OrchestrationEventPayload,
    OrchestrationEventType, RetrievalScope, SqliteEventStore, StoredEventEnvelope, WorkItemId,
    WorkerSessionId,
};
use serde_json::Value;

const TRUNCATION_SUFFIX: &str = "...";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalPackLimits {
    pub max_events: usize,
    pub max_evidence: usize,
    pub max_event_summary_chars: usize,
    pub max_evidence_summary_chars: usize,
}

impl Default for RetrievalPackLimits {
    fn default() -> Self {
        Self {
            max_events: 48,
            max_evidence: 24,
            max_event_summary_chars: 280,
            max_evidence_summary_chars: 280,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetrievalPackStats {
    pub total_matching_events: usize,
    pub dropped_events: usize,
    pub total_evidence_candidates: usize,
    pub dropped_evidence: usize,
    pub missing_evidence: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalPackEvent {
    pub event_id: String,
    pub sequence: u64,
    pub occurred_at: String,
    pub event_type: OrchestrationEventType,
    pub work_item_id: Option<WorkItemId>,
    pub session_id: Option<WorkerSessionId>,
    pub summary: String,
    pub artifact_ids: Vec<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalPackEvidence {
    pub artifact_id: ArtifactId,
    pub work_item_id: WorkItemId,
    pub kind: ArtifactKind,
    pub created_at: String,
    pub storage_ref: String,
    pub metadata_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedContextPack {
    pub scope: RetrievalScope,
    pub events: Vec<RetrievalPackEvent>,
    pub evidence: Vec<RetrievalPackEvidence>,
    pub stats: RetrievalPackStats,
}

pub trait SupervisorRetrievalSource {
    fn query_events(
        &self,
        scope: RetrievalScope,
        limit: usize,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError>;
    fn artifact_refs_for_event(&self, event_id: &str) -> Result<Vec<ArtifactId>, CoreError>;
    fn get_artifact(&self, artifact_id: &ArtifactId) -> Result<Option<ArtifactRecord>, CoreError>;
    fn count_events(&self, scope: RetrievalScope) -> Result<Option<usize>, CoreError> {
        let _ = scope;
        Ok(None)
    }
}

impl SupervisorRetrievalSource for SqliteEventStore {
    fn query_events(
        &self,
        scope: RetrievalScope,
        limit: usize,
    ) -> Result<Vec<StoredEventEnvelope>, CoreError> {
        EventStore::query(self, scope, limit)
    }

    fn artifact_refs_for_event(&self, event_id: &str) -> Result<Vec<ArtifactId>, CoreError> {
        SqliteEventStore::read_artifact_refs_for_event(self, event_id)
    }

    fn get_artifact(&self, artifact_id: &ArtifactId) -> Result<Option<ArtifactRecord>, CoreError> {
        SqliteEventStore::get_artifact(self, artifact_id)
    }

    fn count_events(&self, scope: RetrievalScope) -> Result<Option<usize>, CoreError> {
        SqliteEventStore::count_events(self, scope).map(Some)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SupervisorQueryEngine {
    limits: RetrievalPackLimits,
}

impl SupervisorQueryEngine {
    pub fn new(limits: RetrievalPackLimits) -> Self {
        Self { limits }
    }

    pub fn limits(&self) -> &RetrievalPackLimits {
        &self.limits
    }

    pub fn build_context_pack(
        &self,
        source: &dyn SupervisorRetrievalSource,
        scope: RetrievalScope,
    ) -> Result<BoundedContextPack, CoreError> {
        let event_query_limit = self.limits.max_events.saturating_add(1);
        let mut matched_events = source.query_events(scope.clone(), event_query_limit)?;
        let total_matching_events = source
            .count_events(scope.clone())?
            .unwrap_or(matched_events.len());
        if matched_events.len() > self.limits.max_events {
            matched_events.truncate(self.limits.max_events);
        }
        let dropped_events = total_matching_events.saturating_sub(matched_events.len());

        let mut events = Vec::with_capacity(matched_events.len());
        let mut candidate_artifact_ids = Vec::new();
        let mut seen_candidate_artifacts = HashSet::new();

        for event in matched_events {
            let mut artifact_ids = source.artifact_refs_for_event(event.event_id.as_str())?;
            if let OrchestrationEventPayload::ArtifactCreated(payload) = &event.payload {
                artifact_ids.push(payload.artifact_id.clone());
            }
            dedupe_preserving_order(&mut artifact_ids);

            for artifact_id in &artifact_ids {
                if seen_candidate_artifacts.insert(artifact_id.clone()) {
                    candidate_artifact_ids.push(artifact_id.clone());
                }
            }

            events.push(RetrievalPackEvent {
                event_id: event.event_id,
                sequence: event.sequence,
                occurred_at: event.occurred_at,
                event_type: event.event_type,
                work_item_id: event.work_item_id,
                session_id: event.session_id,
                summary: summarize_event_payload(
                    &event.payload,
                    self.limits.max_event_summary_chars,
                ),
                artifact_ids,
            });
        }

        let total_evidence_candidates = candidate_artifact_ids.len();
        let mut evidence = Vec::new();
        let mut missing_evidence = 0;
        let mut processed_evidence_candidates = 0;
        for artifact_id in candidate_artifact_ids {
            if evidence.len() >= self.limits.max_evidence {
                break;
            }
            processed_evidence_candidates += 1;
            match source.get_artifact(&artifact_id)? {
                Some(artifact) => evidence.push(RetrievalPackEvidence {
                    metadata_summary: summarize_artifact_metadata(
                        &artifact.kind,
                        &artifact.metadata,
                        self.limits.max_evidence_summary_chars,
                    ),
                    artifact_id: artifact.artifact_id,
                    work_item_id: artifact.work_item_id,
                    kind: artifact.kind,
                    created_at: artifact.created_at,
                    storage_ref: artifact.storage_ref,
                }),
                None => missing_evidence += 1,
            }
        }

        let dropped_evidence =
            total_evidence_candidates.saturating_sub(processed_evidence_candidates);
        let stats = RetrievalPackStats {
            total_matching_events,
            dropped_events,
            total_evidence_candidates,
            dropped_evidence,
            missing_evidence,
        };

        Ok(BoundedContextPack {
            scope,
            events,
            evidence,
            stats,
        })
    }
}

fn dedupe_preserving_order(values: &mut Vec<ArtifactId>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn summarize_event_payload(payload: &OrchestrationEventPayload, max_chars: usize) -> String {
    let summary = match payload {
        OrchestrationEventPayload::TicketSynced(ticket) => {
            format!(
                "ticket {} synced as '{}' in state '{}'",
                ticket.identifier, ticket.title, ticket.state
            )
        }
        OrchestrationEventPayload::WorkItemCreated(work_item) => {
            format!(
                "work item {} created for ticket {} in project {}",
                work_item.work_item_id.as_str(),
                work_item.ticket_id.as_str(),
                work_item.project_id.as_str()
            )
        }
        OrchestrationEventPayload::WorktreeCreated(worktree) => {
            format!(
                "worktree {} created at {} on branch {} (base {})",
                worktree.worktree_id.as_str(),
                worktree.path,
                worktree.branch,
                worktree.base_branch
            )
        }
        OrchestrationEventPayload::SessionSpawned(session) => {
            format!(
                "session {} spawned for work item {} using model {}",
                session.session_id.as_str(),
                session.work_item_id.as_str(),
                session.model
            )
        }
        OrchestrationEventPayload::SessionCheckpoint(checkpoint) => {
            format!(
                "checkpoint for session {}: {}",
                checkpoint.session_id.as_str(),
                compact_text(checkpoint.summary.as_str())
            )
        }
        OrchestrationEventPayload::SessionNeedsInput(needs_input) => {
            let options = if needs_input.options.is_empty() {
                "none".to_owned()
            } else {
                needs_input
                    .options
                    .iter()
                    .take(4)
                    .map(|option| compact_text(option.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!(
                "session {} needs input: {} (options: {})",
                needs_input.session_id.as_str(),
                compact_text(needs_input.prompt.as_str()),
                options
            )
        }
        OrchestrationEventPayload::SessionBlocked(blocked) => match blocked.hint.as_deref() {
            Some(hint) => format!(
                "session {} blocked: {} (hint: {})",
                blocked.session_id.as_str(),
                compact_text(blocked.reason.as_str()),
                compact_text(hint)
            ),
            None => format!(
                "session {} blocked: {}",
                blocked.session_id.as_str(),
                compact_text(blocked.reason.as_str())
            ),
        },
        OrchestrationEventPayload::SessionCompleted(completed) => {
            match completed.summary.as_deref() {
                Some(summary) => format!(
                    "session {} completed: {}",
                    completed.session_id.as_str(),
                    compact_text(summary)
                ),
                None => format!("session {} completed", completed.session_id.as_str()),
            }
        }
        OrchestrationEventPayload::SessionCrashed(crashed) => format!(
            "session {} crashed: {}",
            crashed.session_id.as_str(),
            compact_text(crashed.reason.as_str())
        ),
        OrchestrationEventPayload::ArtifactCreated(artifact) => format!(
            "artifact {} ({:?}) created: {} @ {}",
            artifact.artifact_id.as_str(),
            artifact.kind,
            compact_text(artifact.label.as_str()),
            artifact.uri
        ),
        OrchestrationEventPayload::WorkflowTransition(transition) => format!(
            "workflow for {} moved {:?} -> {:?}",
            transition.work_item_id.as_str(),
            transition.from,
            transition.to
        ),
        OrchestrationEventPayload::InboxItemCreated(inbox) => format!(
            "inbox item {} ({:?}) created: {}",
            inbox.inbox_item_id.as_str(),
            inbox.kind,
            compact_text(inbox.title.as_str())
        ),
        OrchestrationEventPayload::InboxItemResolved(inbox) => format!(
            "inbox item {} resolved for {}",
            inbox.inbox_item_id.as_str(),
            inbox.work_item_id.as_str()
        ),
        OrchestrationEventPayload::UserResponded(response) => {
            format!(
                "user responded: {}",
                compact_text(response.message.as_str())
            )
        }
    };

    truncate_chars(summary.as_str(), max_chars)
}

fn summarize_artifact_metadata(kind: &ArtifactKind, metadata: &Value, max_chars: usize) -> String {
    let keys = prioritized_metadata_keys(kind);
    let extracted = keys
        .iter()
        .filter_map(|key| {
            metadata
                .get(*key)
                .map(|value| format!("{key}={}", summarize_metadata_value(value)))
        })
        .collect::<Vec<_>>();

    let summary = if extracted.is_empty() {
        summarize_metadata_value(metadata)
    } else {
        extracted.join("; ")
    };

    truncate_chars(summary.as_str(), max_chars)
}

fn prioritized_metadata_keys(kind: &ArtifactKind) -> &'static [&'static str] {
    match kind {
        ArtifactKind::Diff => &[
            "summary",
            "diffstat",
            "files_changed",
            "additions",
            "deletions",
            "source",
            "runtime_kind",
        ],
        ArtifactKind::PR => &[
            "summary",
            "title",
            "url",
            "number",
            "state",
            "source",
            "runtime_kind",
        ],
        ArtifactKind::TestRun => &[
            "summary",
            "status",
            "failing_test",
            "failing_tests",
            "tail",
            "source",
            "runtime_kind",
        ],
        ArtifactKind::LogSnippet => &[
            "summary",
            "reason",
            "hint",
            "detail",
            "log_ref",
            "source",
            "runtime_kind",
        ],
        ArtifactKind::Link | ArtifactKind::Export => {
            &["summary", "title", "url", "source", "runtime_kind"]
        }
    }
}

fn summarize_metadata_value(value: &Value) -> String {
    match value {
        Value::String(text) => compact_text(text),
        _ => {
            serde_json::to_string(value).unwrap_or_else(|_| "<unserializable metadata>".to_owned())
        }
    }
}

fn compact_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= TRUNCATION_SUFFIX.len() {
        return value.chars().take(max_chars).collect();
    }

    let keep = max_chars - TRUNCATION_SUFFIX.len();
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push_str(TRUNCATION_SUFFIX);
    truncated
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use orchestrator_core::{
        ArtifactCreatedPayload, InboxItemKind, NewEventEnvelope, OrchestrationEventPayload,
        OrchestrationEventType, SessionNeedsInputPayload, SessionSpawnedPayload, SqliteEventStore,
        TicketId, TicketProvider, TicketRecord, TicketWorkItemMapping, UserRespondedPayload,
        WorkItemCreatedPayload,
    };

    use super::*;

    #[derive(Default)]
    struct TestRetrievalSource {
        events: Vec<StoredEventEnvelope>,
        artifact_refs: HashMap<String, Vec<ArtifactId>>,
        artifacts: HashMap<ArtifactId, ArtifactRecord>,
    }

    impl SupervisorRetrievalSource for TestRetrievalSource {
        fn query_events(
            &self,
            scope: RetrievalScope,
            limit: usize,
        ) -> Result<Vec<StoredEventEnvelope>, CoreError> {
            if limit == 0 {
                return Ok(Vec::new());
            }

            let mut scoped = self
                .events
                .iter()
                .filter(|event| match &scope {
                    RetrievalScope::Global => true,
                    RetrievalScope::WorkItem(work_item_id) => {
                        event.work_item_id.as_ref() == Some(work_item_id)
                    }
                    RetrievalScope::Session(session_id) => {
                        event.session_id.as_ref() == Some(session_id)
                    }
                })
                .cloned()
                .collect::<Vec<_>>();
            scoped.sort_by_key(|event| std::cmp::Reverse(event.sequence));
            scoped.truncate(limit);
            Ok(scoped)
        }

        fn artifact_refs_for_event(&self, event_id: &str) -> Result<Vec<ArtifactId>, CoreError> {
            Ok(self
                .artifact_refs
                .get(event_id)
                .cloned()
                .unwrap_or_default())
        }

        fn get_artifact(
            &self,
            artifact_id: &ArtifactId,
        ) -> Result<Option<ArtifactRecord>, CoreError> {
            Ok(self.artifacts.get(artifact_id).cloned())
        }

        fn count_events(&self, scope: RetrievalScope) -> Result<Option<usize>, CoreError> {
            let count = self
                .events
                .iter()
                .filter(|event| match &scope {
                    RetrievalScope::Global => true,
                    RetrievalScope::WorkItem(work_item_id) => {
                        event.work_item_id.as_ref() == Some(work_item_id)
                    }
                    RetrievalScope::Session(session_id) => {
                        event.session_id.as_ref() == Some(session_id)
                    }
                })
                .count();
            Ok(Some(count))
        }
    }

    #[test]
    fn build_context_pack_honors_scope_and_event_bounds() {
        let source = TestRetrievalSource {
            events: vec![
                stored_event(
                    "evt-1",
                    1,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::WorkItemCreated,
                    OrchestrationEventPayload::WorkItemCreated(WorkItemCreatedPayload {
                        work_item_id: WorkItemId::new("wi-a"),
                        ticket_id: TicketId::from("linear:1"),
                        project_id: orchestrator_core::ProjectId::new("proj-1"),
                    }),
                ),
                stored_event(
                    "evt-2",
                    2,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::SessionSpawned,
                    OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                        session_id: WorkerSessionId::new("sess-a"),
                        work_item_id: WorkItemId::new("wi-a"),
                        model: "model-a".to_owned(),
                    }),
                ),
                stored_event(
                    "evt-3",
                    3,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::UserResponded,
                    OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                        session_id: Some(WorkerSessionId::new("sess-a")),
                        work_item_id: Some(WorkItemId::new("wi-a")),
                        message: "go ahead".to_owned(),
                    }),
                ),
                stored_event(
                    "evt-4",
                    4,
                    Some("wi-b"),
                    Some("sess-b"),
                    OrchestrationEventType::UserResponded,
                    OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                        session_id: Some(WorkerSessionId::new("sess-b")),
                        work_item_id: Some(WorkItemId::new("wi-b")),
                        message: "different scope".to_owned(),
                    }),
                ),
            ],
            ..Default::default()
        };

        let engine = SupervisorQueryEngine::new(RetrievalPackLimits {
            max_events: 2,
            max_evidence: 0,
            max_event_summary_chars: 80,
            max_evidence_summary_chars: 80,
        });
        let pack = engine
            .build_context_pack(
                &source,
                RetrievalScope::Session(WorkerSessionId::new("sess-a")),
            )
            .expect("build context pack");

        assert_eq!(pack.events.len(), 2);
        assert_eq!(pack.events[0].event_id, "evt-3");
        assert_eq!(pack.events[1].event_id, "evt-2");
        assert_eq!(pack.stats.total_matching_events, 3);
        assert_eq!(pack.stats.dropped_events, 1);
        assert_eq!(pack.evidence.len(), 0);
    }

    #[test]
    fn build_context_pack_bounds_and_dedupes_evidence_candidates() {
        let mut source = TestRetrievalSource {
            events: vec![stored_event(
                "evt-art",
                10,
                Some("wi-a"),
                Some("sess-a"),
                OrchestrationEventType::ArtifactCreated,
                OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                    artifact_id: ArtifactId::new("art-3"),
                    work_item_id: WorkItemId::new("wi-a"),
                    kind: ArtifactKind::Diff,
                    label: "diff artifact".to_owned(),
                    uri: "artifact://diffs/3".to_owned(),
                }),
            )],
            ..Default::default()
        };
        source.artifact_refs.insert(
            "evt-art".to_owned(),
            vec![
                ArtifactId::new("art-1"),
                ArtifactId::new("art-2"),
                ArtifactId::new("art-1"),
            ],
        );
        source.artifacts.insert(
            ArtifactId::new("art-1"),
            test_artifact(
                "art-1",
                ArtifactKind::Diff,
                serde_json::json!({"summary":"first diff"}),
            ),
        );
        source.artifacts.insert(
            ArtifactId::new("art-2"),
            test_artifact(
                "art-2",
                ArtifactKind::TestRun,
                serde_json::json!({"status":"failed","summary":"2 failing tests"}),
            ),
        );
        source.artifacts.insert(
            ArtifactId::new("art-3"),
            test_artifact(
                "art-3",
                ArtifactKind::PR,
                serde_json::json!({"url":"https://example/pull/3"}),
            ),
        );

        let engine = SupervisorQueryEngine::new(RetrievalPackLimits {
            max_events: 8,
            max_evidence: 2,
            max_event_summary_chars: 120,
            max_evidence_summary_chars: 120,
        });
        let pack = engine
            .build_context_pack(&source, RetrievalScope::WorkItem(WorkItemId::new("wi-a")))
            .expect("build context pack");

        assert_eq!(
            pack.events[0].artifact_ids,
            vec![
                ArtifactId::new("art-1"),
                ArtifactId::new("art-2"),
                ArtifactId::new("art-3"),
            ]
        );
        assert_eq!(pack.evidence.len(), 2);
        assert_eq!(pack.evidence[0].artifact_id, ArtifactId::new("art-1"));
        assert_eq!(pack.evidence[1].artifact_id, ArtifactId::new("art-2"));
        assert_eq!(pack.stats.total_evidence_candidates, 3);
        assert_eq!(pack.stats.dropped_evidence, 1);
    }

    #[test]
    fn build_context_pack_truncates_event_and_evidence_summaries() {
        let mut source = TestRetrievalSource {
            events: vec![stored_event(
                "evt-long",
                5,
                Some("wi-a"),
                Some("sess-a"),
                OrchestrationEventType::SessionNeedsInput,
                OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                    session_id: WorkerSessionId::new("sess-a"),
                    prompt: "Please choose between option A and option B after reading all details"
                        .to_owned(),
                    prompt_id: Some("prompt-1".to_owned()),
                    options: vec![
                        "safe path".to_owned(),
                        "fast path".to_owned(),
                        "balanced path".to_owned(),
                    ],
                    default_option: Some("safe path".to_owned()),
                }),
            )],
            ..Default::default()
        };
        source
            .artifact_refs
            .insert("evt-long".to_owned(), vec![ArtifactId::new("art-long")]);
        source.artifacts.insert(
            ArtifactId::new("art-long"),
            test_artifact(
                "art-long",
                ArtifactKind::LogSnippet,
                serde_json::json!({
                    "summary": "this is an intentionally long summary line for truncation checks"
                }),
            ),
        );

        let engine = SupervisorQueryEngine::new(RetrievalPackLimits {
            max_events: 3,
            max_evidence: 3,
            max_event_summary_chars: 24,
            max_evidence_summary_chars: 20,
        });
        let pack = engine
            .build_context_pack(
                &source,
                RetrievalScope::Session(WorkerSessionId::new("sess-a")),
            )
            .expect("build context pack");

        assert_eq!(pack.events.len(), 1);
        assert_eq!(pack.evidence.len(), 1);
        assert!(pack.events[0].summary.ends_with("..."));
        assert!(pack.evidence[0].metadata_summary.ends_with("..."));
        assert!(pack.events[0].summary.chars().count() <= 24);
        assert!(pack.evidence[0].metadata_summary.chars().count() <= 20);
    }

    #[test]
    fn build_context_pack_backfills_evidence_when_candidates_are_missing() {
        let mut source = TestRetrievalSource {
            events: vec![stored_event(
                "evt-missing",
                3,
                Some("wi-a"),
                Some("sess-a"),
                OrchestrationEventType::ArtifactCreated,
                OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                    artifact_id: ArtifactId::new("art-1"),
                    work_item_id: WorkItemId::new("wi-a"),
                    kind: ArtifactKind::Diff,
                    label: "diff".to_owned(),
                    uri: "artifact://diffs/1".to_owned(),
                }),
            )],
            ..Default::default()
        };
        source.artifact_refs.insert(
            "evt-missing".to_owned(),
            vec![
                ArtifactId::new("missing-art"),
                ArtifactId::new("art-1"),
                ArtifactId::new("art-2"),
                ArtifactId::new("art-3"),
            ],
        );
        source.artifacts.insert(
            ArtifactId::new("art-1"),
            test_artifact(
                "art-1",
                ArtifactKind::Diff,
                serde_json::json!({"summary":"first available"}),
            ),
        );
        source.artifacts.insert(
            ArtifactId::new("art-2"),
            test_artifact(
                "art-2",
                ArtifactKind::TestRun,
                serde_json::json!({"status":"failed"}),
            ),
        );

        let engine = SupervisorQueryEngine::new(RetrievalPackLimits {
            max_events: 3,
            max_evidence: 2,
            max_event_summary_chars: 120,
            max_evidence_summary_chars: 120,
        });
        let pack = engine
            .build_context_pack(&source, RetrievalScope::WorkItem(WorkItemId::new("wi-a")))
            .expect("build context pack");

        assert_eq!(pack.evidence.len(), 2);
        assert_eq!(pack.evidence[0].artifact_id, ArtifactId::new("art-1"));
        assert_eq!(pack.evidence[1].artifact_id, ArtifactId::new("art-2"));
        assert_eq!(pack.stats.total_evidence_candidates, 4);
        assert_eq!(pack.stats.missing_evidence, 1);
        assert_eq!(pack.stats.dropped_evidence, 1);
    }

    #[test]
    fn build_context_pack_uses_total_matching_count_from_source() {
        let source = TestRetrievalSource {
            events: (0..10)
                .map(|index| {
                    stored_event(
                        format!("evt-{index}").as_str(),
                        (index + 1) as u64,
                        Some("wi-a"),
                        Some("sess-a"),
                        OrchestrationEventType::UserResponded,
                        OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                            session_id: Some(WorkerSessionId::new("sess-a")),
                            work_item_id: Some(WorkItemId::new("wi-a")),
                            message: format!("message-{index}"),
                        }),
                    )
                })
                .collect(),
            ..Default::default()
        };

        let engine = SupervisorQueryEngine::new(RetrievalPackLimits {
            max_events: 2,
            max_evidence: 0,
            max_event_summary_chars: 120,
            max_evidence_summary_chars: 120,
        });
        let pack = engine
            .build_context_pack(
                &source,
                RetrievalScope::Session(WorkerSessionId::new("sess-a")),
            )
            .expect("build context pack");

        assert_eq!(pack.events.len(), 2);
        assert_eq!(pack.stats.total_matching_events, 10);
        assert_eq!(pack.stats.dropped_events, 8);
    }

    #[test]
    fn sqlite_source_supports_event_and_artifact_retrieval() {
        let mut store = SqliteEventStore::in_memory().expect("in-memory store");

        let ticket = TicketRecord {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "123"),
            provider: TicketProvider::Linear,
            provider_ticket_id: "123".to_owned(),
            identifier: "AP-123".to_owned(),
            title: "retrieval context test".to_owned(),
            state: "todo".to_owned(),
            updated_at: "2026-02-16T00:00:00Z".to_owned(),
        };
        store.upsert_ticket(&ticket).expect("upsert ticket");
        store
            .map_ticket_to_work_item(&TicketWorkItemMapping {
                ticket_id: ticket.ticket_id.clone(),
                work_item_id: WorkItemId::new("wi-ctx"),
            })
            .expect("map ticket");

        let artifact = ArtifactRecord {
            artifact_id: ArtifactId::new("artifact-ctx"),
            work_item_id: WorkItemId::new("wi-ctx"),
            kind: ArtifactKind::TestRun,
            metadata: serde_json::json!({
                "status": "failed",
                "summary": "2 tests failed",
                "tail": "test_a failed\\ntest_b failed",
            }),
            storage_ref: "artifact://testruns/ctx".to_owned(),
            created_at: "2026-02-16T00:01:00Z".to_owned(),
        };
        store.create_artifact(&artifact).expect("create artifact");

        store
            .append_event(
                NewEventEnvelope {
                    event_id: "evt-ctx".to_owned(),
                    occurred_at: "2026-02-16T00:02:00Z".to_owned(),
                    work_item_id: Some(WorkItemId::new("wi-ctx")),
                    session_id: Some(WorkerSessionId::new("sess-ctx")),
                    payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                        artifact_id: ArtifactId::new("artifact-ctx"),
                        work_item_id: WorkItemId::new("wi-ctx"),
                        kind: ArtifactKind::TestRun,
                        label: "failing test run".to_owned(),
                        uri: "artifact://testruns/ctx".to_owned(),
                    }),
                    schema_version: 1,
                },
                &[ArtifactId::new("artifact-ctx")],
            )
            .expect("append event");

        let engine = SupervisorQueryEngine::default();
        let pack = engine
            .build_context_pack(&store, RetrievalScope::WorkItem(WorkItemId::new("wi-ctx")))
            .expect("build context pack");

        assert_eq!(pack.events.len(), 1);
        assert_eq!(pack.evidence.len(), 1);
        assert_eq!(
            pack.evidence[0].artifact_id,
            ArtifactId::new("artifact-ctx")
        );
        assert!(
            pack.evidence[0].metadata_summary.contains("status=failed"),
            "expected status in metadata summary, got: {}",
            pack.evidence[0].metadata_summary
        );
    }

    fn stored_event(
        event_id: &str,
        sequence: u64,
        work_item_id: Option<&str>,
        session_id: Option<&str>,
        event_type: OrchestrationEventType,
        payload: OrchestrationEventPayload,
    ) -> StoredEventEnvelope {
        StoredEventEnvelope {
            event_id: event_id.to_owned(),
            sequence,
            occurred_at: "2026-02-16T00:00:00Z".to_owned(),
            work_item_id: work_item_id.map(WorkItemId::new),
            session_id: session_id.map(WorkerSessionId::new),
            event_type,
            payload,
            schema_version: 1,
        }
    }

    fn test_artifact(artifact_id: &str, kind: ArtifactKind, metadata: Value) -> ArtifactRecord {
        ArtifactRecord {
            artifact_id: ArtifactId::new(artifact_id),
            work_item_id: WorkItemId::new("wi-a"),
            kind,
            metadata,
            storage_ref: format!("artifact://test/{artifact_id}"),
            created_at: "2026-02-16T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn compact_text_collapses_whitespace() {
        assert_eq!(compact_text("hello\n\nworld\t!"), "hello world !");
    }

    #[test]
    fn truncate_chars_uses_ascii_suffix() {
        assert_eq!(truncate_chars("abcdef", 5), "ab...");
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("abcdef", 2), "ab");
    }

    #[test]
    fn metadata_summary_falls_back_to_json_for_unknown_shape() {
        let summary = summarize_artifact_metadata(
            &ArtifactKind::Link,
            &serde_json::json!({"nested":{"k":"v"}}),
            100,
        );
        assert!(summary.contains("nested"));
    }

    #[test]
    fn metadata_summary_prioritizes_known_keys() {
        let summary = summarize_artifact_metadata(
            &ArtifactKind::TestRun,
            &serde_json::json!({
                "status":"failed",
                "summary":"3 failed",
                "ignored":"value"
            }),
            100,
        );
        assert!(summary.starts_with("summary=3 failed; status=failed"));
    }

    #[test]
    fn event_summary_compacts_prompt_options() {
        let summary = summarize_event_payload(
            &OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id: WorkerSessionId::new("sess-x"),
                prompt: "choose     wisely".to_owned(),
                prompt_id: Some("p".to_owned()),
                options: vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
                default_option: None,
            }),
            200,
        );
        assert!(summary.contains("choose wisely"));
        assert!(summary.contains("alpha, beta, gamma"));
    }

    #[test]
    fn event_summary_for_inbox_created_includes_kind_and_title() {
        let summary = summarize_event_payload(
            &OrchestrationEventPayload::InboxItemCreated(
                orchestrator_core::InboxItemCreatedPayload {
                    inbox_item_id: orchestrator_core::InboxItemId::new("inbox-1"),
                    work_item_id: WorkItemId::new("wi-1"),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Approve deployment".to_owned(),
                },
            ),
            200,
        );
        assert!(summary.contains("NeedsApproval"));
        assert!(summary.contains("Approve deployment"));
    }

    #[test]
    fn sqlite_trait_query_handles_zero_limit() {
        let source = SqliteEventStore::in_memory().expect("in-memory store");
        let engine = SupervisorQueryEngine::new(RetrievalPackLimits {
            max_events: 0,
            max_evidence: 0,
            max_event_summary_chars: 10,
            max_evidence_summary_chars: 10,
        });
        let pack = engine
            .build_context_pack(&source, RetrievalScope::Global)
            .expect("build context pack");
        assert!(pack.events.is_empty());
        assert!(pack.evidence.is_empty());
    }
}
