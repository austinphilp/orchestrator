use std::collections::HashSet;

use orchestrator_core::{
    ArtifactId, ArtifactKind, ArtifactRecord, CoreError, EventStore, OrchestrationEventPayload,
    OrchestrationEventType, RetrievalScope, SqliteEventStore, StoredEventEnvelope,
    SupervisorQueryContextArgs, SupervisorQueryKind, TicketRecord, WorkItemId, WorkerSessionId,
};
use serde_json::Value;

const TRUNCATION_SUFFIX: &str = "...";
const MAX_STATUS_TRANSITIONS: usize = 6;

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetrievalFocusFilters {
    pub scope_hint: String,
    pub selected_work_item_id: Option<WorkItemId>,
    pub selected_session_id: Option<WorkerSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketStatusTransition {
    pub occurred_at: String,
    pub event_id: String,
    pub source: String,
    pub from_state: Option<String>,
    pub to_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TicketStatusContext {
    pub ticket_ref: Option<String>,
    pub ticket_id: Option<String>,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub state: Option<String>,
    pub priority: Option<i32>,
    pub recent_transitions: Vec<TicketStatusTransition>,
    pub fallback_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedContextPack {
    pub scope: RetrievalScope,
    pub focus_filters: RetrievalFocusFilters,
    pub ticket_status: TicketStatusContext,
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
    fn find_ticket_by_work_item(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<TicketRecord>, CoreError> {
        let _ = work_item_id;
        Ok(None)
    }
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

    fn find_ticket_by_work_item(
        &self,
        work_item_id: &WorkItemId,
    ) -> Result<Option<TicketRecord>, CoreError> {
        SqliteEventStore::find_ticket_by_work_item(self, work_item_id)
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
        self.build_context_pack_with_filters(source, scope, &SupervisorQueryContextArgs::default())
    }

    pub fn build_context_pack_with_filters(
        &self,
        source: &dyn SupervisorRetrievalSource,
        scope: RetrievalScope,
        filters: &SupervisorQueryContextArgs,
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
        let focus_filters = build_focus_filters(scope.clone(), filters);
        let ticket_status =
            build_ticket_status_context(source, scope.clone(), &focus_filters, &matched_events)?;

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
            focus_filters,
            ticket_status,
            events,
            evidence,
            stats,
        })
    }
}

fn build_focus_filters(
    scope: RetrievalScope,
    filters: &SupervisorQueryContextArgs,
) -> RetrievalFocusFilters {
    let selected_work_item_id = filters
        .selected_work_item_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(WorkItemId::new)
        .or_else(|| match &scope {
            RetrievalScope::WorkItem(work_item_id) => Some(work_item_id.clone()),
            _ => None,
        });

    let selected_session_id = filters
        .selected_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(WorkerSessionId::new)
        .or_else(|| match &scope {
            RetrievalScope::Session(session_id) => Some(session_id.clone()),
            _ => None,
        });

    let scope_hint = filters
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| render_scope(scope));

    RetrievalFocusFilters {
        scope_hint,
        selected_work_item_id,
        selected_session_id,
    }
}

fn build_ticket_status_context(
    source: &dyn SupervisorRetrievalSource,
    scope: RetrievalScope,
    focus_filters: &RetrievalFocusFilters,
    scoped_events: &[StoredEventEnvelope],
) -> Result<TicketStatusContext, CoreError> {
    let focus_work_item_id = focus_filters
        .selected_work_item_id
        .clone()
        .or_else(|| scope_work_item(scope.clone()))
        .or_else(|| latest_work_item_from_events(scoped_events));
    let ticket_snapshot =
        latest_ticket_sync_from_events(scoped_events, focus_work_item_id.as_ref());
    let mut ticket_status = TicketStatusContext {
        ticket_ref: ticket_snapshot
            .as_ref()
            .map(|snapshot| snapshot.ticket_ref.clone()),
        ticket_id: ticket_snapshot
            .as_ref()
            .map(|snapshot| snapshot.ticket_id.clone()),
        title: ticket_snapshot
            .as_ref()
            .map(|snapshot| snapshot.title.clone()),
        assignee: ticket_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.assignee.clone()),
        state: ticket_snapshot
            .as_ref()
            .map(|snapshot| snapshot.state.clone()),
        priority: ticket_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.priority),
        recent_transitions: recent_status_transitions(scoped_events, focus_work_item_id.as_ref()),
        fallback_message: None,
    };

    if let Some(work_item_id) = focus_work_item_id.as_ref() {
        if let Some(ticket) = source.find_ticket_by_work_item(work_item_id)? {
            if ticket_status.ticket_ref.is_none() {
                ticket_status.ticket_ref = Some(ticket.ticket_id.as_str().to_owned());
            }
            if ticket_status.ticket_id.is_none() {
                ticket_status.ticket_id = Some(ticket.identifier);
            }
            if ticket_status.title.is_none() {
                ticket_status.title = Some(ticket.title);
            }
            if ticket_status.state.is_none() {
                ticket_status.state = Some(ticket.state);
            }
        }
    }

    if ticket_status.ticket_id.is_none() {
        ticket_status.fallback_message = Some(match focus_work_item_id {
            Some(work_item_id) => format!(
                "No local ticket metadata is mapped for work item '{}'. Answer ticket status questions with Unknown.",
                work_item_id.as_str()
            ),
            None => "No ticket is currently in focus for this query scope. Answer ticket status questions with Unknown and ask for a selected ticket.".to_owned(),
        });
    }

    Ok(ticket_status)
}

fn scope_work_item(scope: RetrievalScope) -> Option<WorkItemId> {
    match scope {
        RetrievalScope::WorkItem(work_item_id) => Some(work_item_id),
        _ => None,
    }
}

fn latest_work_item_from_events(scoped_events: &[StoredEventEnvelope]) -> Option<WorkItemId> {
    scoped_events
        .iter()
        .find_map(|event| event.work_item_id.clone())
}

#[derive(Debug, Clone)]
struct TicketSyncSnapshot {
    ticket_ref: String,
    ticket_id: String,
    title: String,
    state: String,
    assignee: Option<String>,
    priority: Option<i32>,
}

fn latest_ticket_sync_from_events(
    scoped_events: &[StoredEventEnvelope],
    focus_work_item_id: Option<&WorkItemId>,
) -> Option<TicketSyncSnapshot> {
    scoped_events.iter().find_map(|event| {
        if focus_work_item_id.is_some() && event.work_item_id.as_ref() != focus_work_item_id {
            return None;
        }
        let OrchestrationEventPayload::TicketSynced(payload) = &event.payload else {
            return None;
        };
        Some(TicketSyncSnapshot {
            ticket_ref: payload.ticket_id.as_str().to_owned(),
            ticket_id: payload.identifier.clone(),
            title: payload.title.clone(),
            state: payload.state.clone(),
            assignee: payload.assignee.clone(),
            priority: payload.priority,
        })
    })
}

fn recent_status_transitions(
    scoped_events: &[StoredEventEnvelope],
    focus_work_item_id: Option<&WorkItemId>,
) -> Vec<TicketStatusTransition> {
    #[derive(Debug)]
    struct OrderedTransition {
        sequence: u64,
        transition: TicketStatusTransition,
    }

    let mut transitions = scoped_events
        .iter()
        .filter(|event| {
            focus_work_item_id.is_none() || event.work_item_id.as_ref() == focus_work_item_id
        })
        .filter_map(|event| match &event.payload {
            OrchestrationEventPayload::WorkflowTransition(payload) => Some(OrderedTransition {
                sequence: event.sequence,
                transition: TicketStatusTransition {
                    occurred_at: event.occurred_at.clone(),
                    event_id: event.event_id.clone(),
                    source: "workflow".to_owned(),
                    from_state: Some(format!("{:?}", payload.from)),
                    to_state: format!("{:?}", payload.to),
                },
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut ordered_ticket_syncs = scoped_events
        .iter()
        .filter(|event| {
            focus_work_item_id.is_none() || event.work_item_id.as_ref() == focus_work_item_id
        })
        .filter_map(|event| match &event.payload {
            OrchestrationEventPayload::TicketSynced(payload) => Some((
                event.occurred_at.clone(),
                event.event_id.clone(),
                event.sequence,
                payload.state.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    ordered_ticket_syncs.sort_by_key(|(_, _, sequence, _)| *sequence);
    let mut previous_state: Option<String> = None;
    for (occurred_at, event_id, sequence, state) in ordered_ticket_syncs {
        let from_state = previous_state.clone();
        if from_state.as_deref() != Some(state.as_str()) {
            transitions.push(OrderedTransition {
                sequence,
                transition: TicketStatusTransition {
                    occurred_at,
                    event_id,
                    source: "ticket_sync".to_owned(),
                    from_state,
                    to_state: state.clone(),
                },
            });
        }
        previous_state = Some(state);
    }

    transitions.sort_by(|left, right| right.sequence.cmp(&left.sequence));
    transitions.truncate(MAX_STATUS_TRANSITIONS);
    transitions
        .into_iter()
        .map(|ordered| ordered.transition)
        .collect()
}

fn render_scope(scope: RetrievalScope) -> String {
    match scope {
        RetrievalScope::Global => "global".to_owned(),
        RetrievalScope::WorkItem(work_item_id) => format!("work_item:{}", work_item_id.as_str()),
        RetrievalScope::Session(session_id) => format!("session:{}", session_id.as_str()),
    }
}

fn dedupe_preserving_order(values: &mut Vec<ArtifactId>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn summarize_event_payload(payload: &OrchestrationEventPayload, max_chars: usize) -> String {
    let summary = match payload {
        OrchestrationEventPayload::TicketSynced(ticket) => {
            let mut summary = format!(
                "ticket {} synced as '{}' in state '{}'",
                ticket.identifier, ticket.title, ticket.state
            );
            if let Some(priority) = ticket.priority {
                summary.push_str(format!(" (priority: {priority})").as_str());
            }
            if let Some(assignee) = ticket.assignee.as_deref() {
                summary.push_str(format!(" (assignee: {})", compact_text(assignee)).as_str());
            }
            summary
        }
        OrchestrationEventPayload::TicketDetailsSynced(details) => format!(
            "ticket details synced for {} ({})",
            details.ticket_id.as_str(),
            if details
                .description
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty())
            {
                "description present"
            } else {
                "description empty"
            }
        ),
        OrchestrationEventPayload::WorkItemCreated(work_item) => {
            format!(
                "work item {} created for ticket {} in project {}",
                work_item.work_item_id.as_str(),
                work_item.ticket_id.as_str(),
                work_item.project_id.as_str()
            )
        }
        OrchestrationEventPayload::WorkItemProfileOverrideSet(override_set) => format!(
            "work item {} profile override set to {}",
            override_set.work_item_id.as_str(),
            override_set.profile_name
        ),
        OrchestrationEventPayload::WorkItemProfileOverrideCleared(override_cleared) => format!(
            "work item {} profile override cleared",
            override_cleared.work_item_id.as_str()
        ),
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
        OrchestrationEventPayload::SupervisorQueryStarted(started) => match started.kind {
            SupervisorQueryKind::Template => format!(
                "supervisor query {} started (template: {}, scope: {})",
                started.query_id,
                started.template.as_deref().unwrap_or("unknown"),
                started.scope
            ),
            SupervisorQueryKind::Freeform => format!(
                "supervisor query {} started (freeform, scope: {})",
                started.query_id, started.scope
            ),
        },
        OrchestrationEventPayload::SupervisorQueryChunk(chunk) => {
            let mut summary = format!(
                "supervisor query {} chunk #{} (+{} chars, total {} chars)",
                chunk.query_id, chunk.chunk_index, chunk.delta_chars, chunk.cumulative_output_chars
            );
            if let Some(reason) = chunk.finish_reason.as_ref() {
                summary.push_str(format!(" finish={reason:?}").as_str());
            }
            if let Some(usage) = chunk.usage.as_ref() {
                summary.push_str(
                    format!(
                        " usage(input={}, output={}, total={})",
                        usage.input_tokens, usage.output_tokens, usage.total_tokens
                    )
                    .as_str(),
                );
            }
            summary
        }
        OrchestrationEventPayload::SupervisorQueryCancelled(cancelled) => format!(
            "supervisor query {} cancel requested ({:?})",
            cancelled.query_id, cancelled.source
        ),
        OrchestrationEventPayload::SupervisorQueryFinished(finished) => {
            let mut summary = format!(
                "supervisor query {} finished {:?} in {}ms (chunks={}, chars={})",
                finished.query_id,
                finished.finish_reason,
                finished.duration_ms,
                finished.chunk_count,
                finished.output_chars
            );
            if let Some(usage) = finished.usage.as_ref() {
                summary.push_str(
                    format!(
                        " usage(input={}, output={}, total={})",
                        usage.input_tokens, usage.output_tokens, usage.total_tokens
                    )
                    .as_str(),
                );
            }
            if let Some(source) = finished.cancellation_source.as_ref() {
                summary.push_str(format!(" cancellation={source:?}").as_str());
            }
            if let Some(error) = finished.error.as_deref() {
                summary.push_str(format!(" error={}", compact_text(error)).as_str());
            }
            summary
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
        SupervisorQueryContextArgs, TicketId, TicketProvider, TicketRecord, TicketSyncedPayload,
        TicketWorkItemMapping, UserRespondedPayload, WorkItemCreatedPayload, WorkflowState,
        WorkflowTransitionPayload,
    };

    use super::*;

    #[derive(Default)]
    struct TestRetrievalSource {
        events: Vec<StoredEventEnvelope>,
        artifact_refs: HashMap<String, Vec<ArtifactId>>,
        artifacts: HashMap<ArtifactId, ArtifactRecord>,
        tickets_by_work_item: HashMap<WorkItemId, TicketRecord>,
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

        fn find_ticket_by_work_item(
            &self,
            work_item_id: &WorkItemId,
        ) -> Result<Option<TicketRecord>, CoreError> {
            Ok(self.tickets_by_work_item.get(work_item_id).cloned())
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
    fn build_context_pack_includes_ticket_status_and_focus_filters() {
        let mut source = TestRetrievalSource {
            events: vec![
                stored_event(
                    "evt-1",
                    1,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::TicketSynced,
                    OrchestrationEventPayload::TicketSynced(TicketSyncedPayload {
                        ticket_id: TicketId::from("linear:issue-123"),
                        identifier: "AP-123".to_owned(),
                        title: "Ticket status payload".to_owned(),
                        state: "Todo".to_owned(),
                        assignee: Some("alice".to_owned()),
                        priority: Some(2),
                    }),
                ),
                stored_event(
                    "evt-2",
                    2,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::WorkflowTransition,
                    OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                        work_item_id: WorkItemId::new("wi-a"),
                        from: WorkflowState::Planning,
                        to: WorkflowState::Implementing,
                        reason: None,
                    }),
                ),
                stored_event(
                    "evt-3",
                    3,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::TicketSynced,
                    OrchestrationEventPayload::TicketSynced(TicketSyncedPayload {
                        ticket_id: TicketId::from("linear:issue-123"),
                        identifier: "AP-123".to_owned(),
                        title: "Ticket status payload".to_owned(),
                        state: "In Progress".to_owned(),
                        assignee: Some("alice".to_owned()),
                        priority: Some(2),
                    }),
                ),
            ],
            ..Default::default()
        };
        source.tickets_by_work_item.insert(
            WorkItemId::new("wi-a"),
            TicketRecord {
                ticket_id: TicketId::from("linear:issue-123"),
                provider: TicketProvider::Linear,
                provider_ticket_id: "issue-123".to_owned(),
                identifier: "AP-123".to_owned(),
                title: "Ticket status payload".to_owned(),
                state: "In Progress".to_owned(),
                updated_at: "2026-02-16T00:00:00Z".to_owned(),
            },
        );

        let engine = SupervisorQueryEngine::default();
        let pack = engine
            .build_context_pack_with_filters(
                &source,
                RetrievalScope::Session(WorkerSessionId::new("sess-a")),
                &SupervisorQueryContextArgs {
                    selected_work_item_id: Some("wi-a".to_owned()),
                    selected_session_id: Some("sess-a".to_owned()),
                    scope: Some("session:sess-a".to_owned()),
                },
            )
            .expect("build context pack");

        assert_eq!(pack.focus_filters.scope_hint, "session:sess-a");
        assert_eq!(
            pack.focus_filters.selected_work_item_id,
            Some(WorkItemId::new("wi-a"))
        );
        assert_eq!(
            pack.focus_filters.selected_session_id,
            Some(WorkerSessionId::new("sess-a"))
        );
        assert_eq!(
            pack.ticket_status.ticket_ref.as_deref(),
            Some("linear:issue-123")
        );
        assert_eq!(pack.ticket_status.ticket_id.as_deref(), Some("AP-123"));
        assert_eq!(
            pack.ticket_status.title.as_deref(),
            Some("Ticket status payload")
        );
        assert_eq!(pack.ticket_status.assignee.as_deref(), Some("alice"));
        assert_eq!(pack.ticket_status.priority, Some(2));
        assert_eq!(pack.ticket_status.state.as_deref(), Some("In Progress"));
        assert!(pack
            .ticket_status
            .recent_transitions
            .iter()
            .any(|entry| entry.source == "workflow"));
        assert!(pack
            .ticket_status
            .recent_transitions
            .iter()
            .any(|entry| entry.source == "ticket_sync"));
        assert_eq!(pack.ticket_status.fallback_message, None);
    }

    #[test]
    fn build_context_pack_prefers_scoped_ticket_sync_over_store_snapshot() {
        let mut source = TestRetrievalSource {
            events: vec![stored_event(
                "evt-sync",
                11,
                Some("wi-a"),
                Some("sess-a"),
                OrchestrationEventType::TicketSynced,
                OrchestrationEventPayload::TicketSynced(TicketSyncedPayload {
                    ticket_id: TicketId::from("linear:issue-123"),
                    identifier: "AP-123".to_owned(),
                    title: "Scope snapshot title".to_owned(),
                    state: "In Progress".to_owned(),
                    assignee: Some("alice".to_owned()),
                    priority: Some(2),
                }),
            )],
            ..Default::default()
        };
        source.tickets_by_work_item.insert(
            WorkItemId::new("wi-a"),
            TicketRecord {
                ticket_id: TicketId::from("linear:issue-123"),
                provider: TicketProvider::Linear,
                provider_ticket_id: "issue-123".to_owned(),
                identifier: "AP-123".to_owned(),
                title: "Store snapshot title".to_owned(),
                state: "Done".to_owned(),
                updated_at: "2026-02-16T00:00:00Z".to_owned(),
            },
        );

        let engine = SupervisorQueryEngine::default();
        let pack = engine
            .build_context_pack_with_filters(
                &source,
                RetrievalScope::WorkItem(WorkItemId::new("wi-a")),
                &SupervisorQueryContextArgs {
                    selected_work_item_id: Some("wi-a".to_owned()),
                    selected_session_id: Some("sess-a".to_owned()),
                    scope: Some("work_item:wi-a".to_owned()),
                },
            )
            .expect("build context pack");

        assert_eq!(pack.ticket_status.state.as_deref(), Some("In Progress"));
        assert_eq!(
            pack.ticket_status.title.as_deref(),
            Some("Scope snapshot title")
        );
    }

    #[test]
    fn build_context_pack_orders_recent_transitions_by_sequence_desc() {
        let source = TestRetrievalSource {
            events: vec![
                stored_event(
                    "evt-1",
                    1,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::WorkflowTransition,
                    OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
                        work_item_id: WorkItemId::new("wi-a"),
                        from: WorkflowState::Planning,
                        to: WorkflowState::Implementing,
                        reason: None,
                    }),
                ),
                stored_event(
                    "evt-2",
                    2,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::TicketSynced,
                    OrchestrationEventPayload::TicketSynced(TicketSyncedPayload {
                        ticket_id: TicketId::from("linear:issue-123"),
                        identifier: "AP-123".to_owned(),
                        title: "Ticket status payload".to_owned(),
                        state: "Todo".to_owned(),
                        assignee: None,
                        priority: None,
                    }),
                ),
                stored_event(
                    "evt-3",
                    3,
                    Some("wi-a"),
                    Some("sess-a"),
                    OrchestrationEventType::TicketSynced,
                    OrchestrationEventPayload::TicketSynced(TicketSyncedPayload {
                        ticket_id: TicketId::from("linear:issue-123"),
                        identifier: "AP-123".to_owned(),
                        title: "Ticket status payload".to_owned(),
                        state: "In Progress".to_owned(),
                        assignee: None,
                        priority: None,
                    }),
                ),
            ],
            ..Default::default()
        };

        let engine = SupervisorQueryEngine::default();
        let pack = engine
            .build_context_pack_with_filters(
                &source,
                RetrievalScope::Session(WorkerSessionId::new("sess-a")),
                &SupervisorQueryContextArgs {
                    selected_work_item_id: Some("wi-a".to_owned()),
                    selected_session_id: Some("sess-a".to_owned()),
                    scope: Some("session:sess-a".to_owned()),
                },
            )
            .expect("build context pack");

        assert_eq!(pack.ticket_status.recent_transitions.len(), 3);
        assert_eq!(pack.ticket_status.recent_transitions[0].event_id, "evt-3");
        assert_eq!(pack.ticket_status.recent_transitions[1].event_id, "evt-2");
        assert_eq!(pack.ticket_status.recent_transitions[2].event_id, "evt-1");
    }

    #[test]
    fn build_context_pack_sets_fallback_when_ticket_context_is_missing() {
        let source = TestRetrievalSource::default();
        let engine = SupervisorQueryEngine::default();
        let pack = engine
            .build_context_pack_with_filters(
                &source,
                RetrievalScope::Global,
                &SupervisorQueryContextArgs::default(),
            )
            .expect("build context pack");

        assert!(pack.ticket_status.ticket_id.is_none());
        assert!(pack.ticket_status.recent_transitions.is_empty());
        assert!(pack.ticket_status.fallback_message.is_some());
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
