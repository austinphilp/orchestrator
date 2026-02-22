//! Runtime/provider event normalization boundary.
//! Translation only: maps backend events into canonical domain event envelopes.

use serde_json::json;

use crate::events::{
    ArtifactCreatedPayload, InboxItemCreatedPayload, NewEventEnvelope, OrchestrationEventPayload,
    SessionBlockedPayload, SessionCheckpointPayload, SessionCompletedPayload,
    SessionCrashedPayload, SessionNeedsInputPayload,
};
use orchestrator_core::{
    ArtifactId, ArtifactKind, ArtifactRecord, BackendArtifactEvent, BackendArtifactKind,
    BackendBlockedEvent, BackendDoneEvent, BackendEvent, BackendNeedsInputEvent,
    BackendOutputEvent, InboxItemId, InboxItemKind, WorkItemId, WorkerSessionId,
};
#[cfg(test)]
use orchestrator_worker_protocol::WorkerOutputStream as BackendOutputStream;

pub const DOMAIN_EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEventNormalizationContext {
    pub occurred_at: String,
    pub work_item_id: WorkItemId,
    pub session_id: WorkerSessionId,
    pub backend_event_index: u64,
}

impl BackendEventNormalizationContext {
    pub fn new(
        occurred_at: impl Into<String>,
        work_item_id: WorkItemId,
        session_id: WorkerSessionId,
        backend_event_index: u64,
    ) -> Self {
        Self {
            occurred_at: occurred_at.into(),
            work_item_id,
            session_id,
            backend_event_index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDomainEvent {
    pub event: NewEventEnvelope,
    pub artifact_refs: Vec<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NormalizedBackendEvent {
    pub raw_output: Option<BackendOutputEvent>,
    pub artifacts: Vec<ArtifactRecord>,
    pub events: Vec<NormalizedDomainEvent>,
}

pub fn normalize_backend_event(
    context: &BackendEventNormalizationContext,
    backend_event: &BackendEvent,
) -> NormalizedBackendEvent {
    let mut normalized = NormalizedBackendEvent::default();

    match backend_event {
        BackendEvent::Output(output) => {
            normalized.raw_output = Some(output.clone());
        }
        BackendEvent::Checkpoint(checkpoint) => {
            let artifact_id = artifact_id(context, "checkpoint");
            let storage_ref = format!(
                "artifact://session/{}/checkpoint/{}",
                context.session_id.as_str(),
                context.backend_event_index
            );
            add_artifact(
                &mut normalized,
                context,
                "artifact-checkpoint",
                ArtifactRecord {
                    artifact_id: artifact_id.clone(),
                    work_item_id: context.work_item_id.clone(),
                    kind: ArtifactKind::LogSnippet,
                    metadata: json!({
                        "source": "session_checkpoint",
                        "summary": checkpoint.summary.clone(),
                        "detail": checkpoint.detail.clone(),
                        "file_refs": checkpoint.file_refs.clone(),
                    }),
                    storage_ref: storage_ref.clone(),
                    created_at: context.occurred_at.clone(),
                },
                checkpoint_artifact_label(checkpoint.summary.as_str()),
                storage_ref,
            );
            add_event(
                &mut normalized,
                context,
                "checkpoint",
                OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                    session_id: context.session_id.clone(),
                    artifact_id: artifact_id.clone(),
                    summary: checkpoint.summary.clone(),
                }),
                vec![artifact_id],
            );
        }
        BackendEvent::NeedsInput(needs_input) => {
            let prompt = normalize_reason(
                primary_needs_input_prompt(needs_input).as_str(),
                "choose next action",
            );
            let options = normalize_needs_input_options(&primary_needs_input_options(needs_input));
            let default_option = normalize_optional_text(needs_input.default_option.as_deref())
                .map(ToOwned::to_owned);
            let prompt_id = Some(normalize_prompt_id(context, needs_input));

            add_event(
                &mut normalized,
                context,
                "needs-input",
                OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                    session_id: context.session_id.clone(),
                    prompt,
                    prompt_id,
                    options,
                    default_option,
                }),
                vec![],
            );
            add_event(
                &mut normalized,
                context,
                "inbox-needs-decision",
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_item_id(context, "needs-decision"),
                    work_item_id: context.work_item_id.clone(),
                    kind: InboxItemKind::NeedsDecision,
                    title: needs_input_title(needs_input),
                }),
                vec![],
            );
        }
        BackendEvent::Blocked(blocked) => {
            normalize_blocked_event(&mut normalized, context, blocked);
        }
        BackendEvent::Artifact(artifact) => {
            normalize_artifact_event(&mut normalized, context, artifact);
        }
        BackendEvent::TurnState(_turn_state) => {}
        BackendEvent::Done(done) => {
            normalize_done_event(&mut normalized, context, done);
        }
        BackendEvent::Crashed(crashed) => {
            let reason = normalize_reason(crashed.reason.as_str(), "worker session crashed");
            let artifact_id = artifact_id(context, "crashed-log");
            let storage_ref = format!(
                "artifact://session/{}/crashed/{}",
                context.session_id.as_str(),
                context.backend_event_index
            );

            add_artifact(
                &mut normalized,
                context,
                "artifact-crashed-log",
                ArtifactRecord {
                    artifact_id: artifact_id.clone(),
                    work_item_id: context.work_item_id.clone(),
                    kind: ArtifactKind::LogSnippet,
                    metadata: json!({
                        "source": "session_crashed",
                        "reason": reason.clone(),
                    }),
                    storage_ref: storage_ref.clone(),
                    created_at: context.occurred_at.clone(),
                },
                "Session crashed".to_owned(),
                storage_ref.clone(),
            );
            add_event(
                &mut normalized,
                context,
                "blocked-crashed",
                OrchestrationEventPayload::SessionBlocked(SessionBlockedPayload {
                    session_id: context.session_id.clone(),
                    reason: reason.clone(),
                    hint: None,
                    log_ref: Some(storage_ref.clone()),
                }),
                vec![artifact_id.clone()],
            );
            add_event(
                &mut normalized,
                context,
                "session-crashed",
                OrchestrationEventPayload::SessionCrashed(SessionCrashedPayload {
                    session_id: context.session_id.clone(),
                    reason: reason.clone(),
                }),
                vec![artifact_id],
            );
            add_event(
                &mut normalized,
                context,
                "inbox-crashed",
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_item_id(context, "blocked"),
                    work_item_id: context.work_item_id.clone(),
                    kind: InboxItemKind::Blocked,
                    title: format!("Session crashed: {}", title_case(reason.as_str())),
                }),
                vec![],
            );
        }
    }

    normalized
}

fn normalize_blocked_event(
    normalized: &mut NormalizedBackendEvent,
    context: &BackendEventNormalizationContext,
    blocked: &BackendBlockedEvent,
) {
    let reason = normalize_reason(blocked.reason.as_str(), "worker blocked");
    let hint = normalize_optional_text(blocked.hint.as_deref()).map(ToOwned::to_owned);
    let log_ref = normalize_optional_text(blocked.log_ref.as_deref()).map(ToOwned::to_owned);

    let mut artifact_refs = Vec::new();
    if let Some(log_ref) = log_ref.as_deref() {
        let artifact_id = artifact_id(context, "blocked-log");
        add_artifact(
            normalized,
            context,
            "artifact-blocked-log",
            ArtifactRecord {
                artifact_id: artifact_id.clone(),
                work_item_id: context.work_item_id.clone(),
                kind: ArtifactKind::LogSnippet,
                metadata: json!({
                    "source": "session_blocked",
                    "reason": reason.clone(),
                    "hint": hint.clone(),
                    "log_ref": log_ref,
                }),
                storage_ref: log_ref.to_owned(),
                created_at: context.occurred_at.clone(),
            },
            "Blocked log reference".to_owned(),
            log_ref.to_owned(),
        );
        artifact_refs.push(artifact_id);
    }

    add_event(
        normalized,
        context,
        "blocked",
        OrchestrationEventPayload::SessionBlocked(SessionBlockedPayload {
            session_id: context.session_id.clone(),
            reason: reason.clone(),
            hint: hint.clone(),
            log_ref: log_ref.clone(),
        }),
        artifact_refs,
    );
    add_event(
        normalized,
        context,
        "inbox-blocked",
        OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
            inbox_item_id: inbox_item_id(context, "blocked"),
            work_item_id: context.work_item_id.clone(),
            kind: InboxItemKind::Blocked,
            title: blocked_inbox_title(reason.as_str(), hint.as_deref()),
        }),
        vec![],
    );
}

fn normalize_artifact_event(
    normalized: &mut NormalizedBackendEvent,
    context: &BackendEventNormalizationContext,
    artifact: &BackendArtifactEvent,
) {
    let kind = map_artifact_kind(&artifact.kind);
    let artifact_id = artifact
        .artifact_id
        .as_ref()
        .and_then(|id| normalize_optional_text(Some(id.as_str())))
        .map(ArtifactId::from)
        .unwrap_or_else(|| artifact_id(context, "backend-artifact"));
    let storage_ref = normalize_optional_text(artifact.uri.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "artifact://session/{}/artifact/{}",
                context.session_id.as_str(),
                context.backend_event_index
            )
        });
    let label = normalize_optional_text(artifact.label.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_artifact_label(&kind).to_owned());

    add_artifact(
        normalized,
        context,
        "artifact-backend",
        ArtifactRecord {
            artifact_id,
            work_item_id: context.work_item_id.clone(),
            kind,
            metadata: json!({
                "source": "backend_artifact",
                "runtime_kind": runtime_artifact_kind(&artifact.kind),
            }),
            storage_ref: storage_ref.clone(),
            created_at: context.occurred_at.clone(),
        },
        label,
        storage_ref,
    );
}

fn normalize_done_event(
    normalized: &mut NormalizedBackendEvent,
    context: &BackendEventNormalizationContext,
    done: &BackendDoneEvent,
) {
    let summary = normalize_optional_text(done.summary.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "session complete".to_owned());
    let artifact_id = artifact_id(context, "done-summary");
    let storage_ref = format!(
        "artifact://session/{}/done/{}",
        context.session_id.as_str(),
        context.backend_event_index
    );

    add_artifact(
        normalized,
        context,
        "artifact-done-summary",
        ArtifactRecord {
            artifact_id: artifact_id.clone(),
            work_item_id: context.work_item_id.clone(),
            kind: ArtifactKind::LogSnippet,
            metadata: json!({
                "source": "session_done",
                "summary": summary.clone(),
            }),
            storage_ref: storage_ref.clone(),
            created_at: context.occurred_at.clone(),
        },
        "Session completion summary".to_owned(),
        storage_ref,
    );
    add_event(
        normalized,
        context,
        "checkpoint-done",
        OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
            session_id: context.session_id.clone(),
            artifact_id: artifact_id.clone(),
            summary: summary.clone(),
        }),
        vec![artifact_id.clone()],
    );
    add_event(
        normalized,
        context,
        "session-completed",
        OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
            session_id: context.session_id.clone(),
            summary: Some(summary.clone()),
        }),
        vec![artifact_id],
    );
    add_event(
        normalized,
        context,
        "inbox-fyi",
        OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
            inbox_item_id: inbox_item_id(context, "fyi"),
            work_item_id: context.work_item_id.clone(),
            kind: InboxItemKind::FYI,
            title: format!("Session complete: {}", title_case(summary.as_str())),
        }),
        vec![],
    );
}

fn add_artifact(
    normalized: &mut NormalizedBackendEvent,
    context: &BackendEventNormalizationContext,
    suffix: &str,
    artifact: ArtifactRecord,
    label: String,
    uri: String,
) {
    add_event(
        normalized,
        context,
        suffix,
        OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
            artifact_id: artifact.artifact_id.clone(),
            work_item_id: context.work_item_id.clone(),
            kind: artifact.kind.clone(),
            label,
            uri,
        }),
        vec![artifact.artifact_id.clone()],
    );
    normalized.artifacts.push(artifact);
}

fn add_event(
    normalized: &mut NormalizedBackendEvent,
    context: &BackendEventNormalizationContext,
    suffix: &str,
    payload: OrchestrationEventPayload,
    artifact_refs: Vec<ArtifactId>,
) {
    normalized.events.push(NormalizedDomainEvent {
        event: NewEventEnvelope {
            event_id: event_id(context, suffix),
            occurred_at: context.occurred_at.clone(),
            work_item_id: Some(context.work_item_id.clone()),
            session_id: Some(context.session_id.clone()),
            payload,
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        },
        artifact_refs,
    });
}

fn event_id(context: &BackendEventNormalizationContext, suffix: &str) -> String {
    format!(
        "evt-{}-{}-{suffix}",
        context.session_id.as_str(),
        context.backend_event_index
    )
}

fn artifact_id(context: &BackendEventNormalizationContext, suffix: &str) -> ArtifactId {
    ArtifactId::new(format!(
        "artifact-{}-{}-{suffix}",
        context.session_id.as_str(),
        context.backend_event_index
    ))
}

fn inbox_item_id(context: &BackendEventNormalizationContext, suffix: &str) -> InboxItemId {
    InboxItemId::new(format!(
        "inbox-{}-{}-{suffix}",
        context.session_id.as_str(),
        context.backend_event_index
    ))
}

fn map_artifact_kind(kind: &BackendArtifactKind) -> ArtifactKind {
    match kind {
        BackendArtifactKind::Diff => ArtifactKind::Diff,
        BackendArtifactKind::PullRequest => ArtifactKind::PR,
        BackendArtifactKind::TestRun => ArtifactKind::TestRun,
        BackendArtifactKind::LogSnippet => ArtifactKind::LogSnippet,
        BackendArtifactKind::Link => ArtifactKind::Link,
        BackendArtifactKind::SessionExport => ArtifactKind::Export,
        BackendArtifactKind::Other(_) => ArtifactKind::Link,
    }
}

fn runtime_artifact_kind(kind: &BackendArtifactKind) -> String {
    match kind {
        BackendArtifactKind::Diff => "diff".to_owned(),
        BackendArtifactKind::PullRequest => "pull_request".to_owned(),
        BackendArtifactKind::TestRun => "test_run".to_owned(),
        BackendArtifactKind::LogSnippet => "log_snippet".to_owned(),
        BackendArtifactKind::Link => "link".to_owned(),
        BackendArtifactKind::SessionExport => "session_export".to_owned(),
        BackendArtifactKind::Other(value) => format!("other:{value}"),
    }
}

fn default_artifact_label(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Diff => "Diff artifact",
        ArtifactKind::PR => "Pull request artifact",
        ArtifactKind::TestRun => "Test run artifact",
        ArtifactKind::LogSnippet => "Log snippet",
        ArtifactKind::Link => "Linked artifact",
        ArtifactKind::Export => "Session export artifact",
    }
}

fn checkpoint_artifact_label(summary: &str) -> String {
    let summary = normalize_reason(summary, "checkpoint");
    format!("Checkpoint: {}", title_case(summary.as_str()))
}

fn needs_input_title(event: &BackendNeedsInputEvent) -> String {
    format!(
        "Decision needed: {}",
        title_case(normalize_reason(event.question.as_str(), "choose next action").as_str())
    )
}

fn normalize_prompt_id(
    context: &BackendEventNormalizationContext,
    event: &BackendNeedsInputEvent,
) -> String {
    normalize_optional_text(Some(event.prompt_id.as_str()))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "prompt-{}-{}",
                context.session_id.as_str(),
                context.backend_event_index
            )
        })
}

fn primary_needs_input_prompt(event: &BackendNeedsInputEvent) -> String {
    event.questions.first().map_or_else(
        || event.question.clone(),
        |question| question.question.clone(),
    )
}

fn primary_needs_input_options(event: &BackendNeedsInputEvent) -> Vec<String> {
    if let Some(question) = event.questions.first() {
        if let Some(options) = question.options.as_ref() {
            return options.iter().map(|option| option.label.clone()).collect();
        }
    }
    event.options.clone()
}

fn normalize_needs_input_options(options: &[String]) -> Vec<String> {
    options
        .iter()
        .filter_map(|option| normalize_optional_text(Some(option.as_str())).map(ToOwned::to_owned))
        .collect()
}

fn blocked_inbox_title(reason: &str, hint: Option<&str>) -> String {
    let detail = match hint {
        Some(hint) => format!("{reason} (hint: {hint})"),
        None => reason.to_owned(),
    };
    format!("Blocked: {}", title_case(detail.as_str()))
}

fn normalize_reason(value: &str, fallback: &str) -> String {
    normalize_optional_text(Some(value))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback.to_owned())
}

fn normalize_optional_text(value: Option<&str>) -> Option<&str> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn title_case(value: &str) -> String {
    const TITLE_LIMIT: usize = 120;
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= TITLE_LIMIT {
        return compact;
    }
    let keep = TITLE_LIMIT.saturating_sub(3);
    let truncated = compact.chars().take(keep).collect::<String>();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use orchestrator_core::{
        normalize_backend_event as core_normalize_backend_event, BackendCheckpointEvent,
        BackendCrashedEvent,
        BackendEventNormalizationContext as CoreBackendEventNormalizationContext,
        BackendNeedsInputOption, BackendNeedsInputQuestion,
        NormalizedBackendEvent as CoreNormalizedBackendEvent, RuntimeArtifactId,
        DOMAIN_EVENT_SCHEMA_VERSION as CORE_DOMAIN_EVENT_SCHEMA_VERSION,
    };

    use super::*;

    fn context(index: u64) -> BackendEventNormalizationContext {
        BackendEventNormalizationContext::new(
            "2026-02-16T12:00:00Z",
            WorkItemId::new("wi-116"),
            WorkerSessionId::new("sess-116"),
            index,
        )
    }

    fn core_context(
        context: &BackendEventNormalizationContext,
    ) -> CoreBackendEventNormalizationContext {
        CoreBackendEventNormalizationContext::new(
            context.occurred_at.clone(),
            context.work_item_id.clone(),
            context.session_id.clone(),
            context.backend_event_index,
        )
    }

    fn from_core_normalized(core: CoreNormalizedBackendEvent) -> NormalizedBackendEvent {
        NormalizedBackendEvent {
            raw_output: core.raw_output,
            artifacts: core.artifacts,
            events: core
                .events
                .into_iter()
                .map(|event| NormalizedDomainEvent {
                    event: event.event,
                    artifact_refs: event.artifact_refs,
                })
                .collect(),
        }
    }

    #[test]
    fn schema_version_constant_matches_legacy_core_value() {
        assert_eq!(
            DOMAIN_EVENT_SCHEMA_VERSION,
            CORE_DOMAIN_EVENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn normalization_matches_legacy_core_output_for_supported_backend_events() {
        let scenarios = vec![
            (
                101_u64,
                BackendEvent::Output(BackendOutputEvent {
                    stream: BackendOutputStream::Stderr,
                    bytes: b"stderr output".to_vec(),
                }),
            ),
            (
                102_u64,
                BackendEvent::Checkpoint(BackendCheckpointEvent {
                    summary: "checkpoint summary".to_owned(),
                    detail: Some("checkpoint detail".to_owned()),
                    file_refs: vec!["src/lib.rs".to_owned()],
                }),
            ),
            (
                103_u64,
                BackendEvent::NeedsInput(BackendNeedsInputEvent {
                    prompt_id: " ".to_owned(),
                    question: "fallback question".to_owned(),
                    options: vec!["".to_owned(), "legacy option".to_owned()],
                    default_option: Some(" ".to_owned()),
                    questions: vec![BackendNeedsInputQuestion {
                        id: "q1".to_owned(),
                        header: "Question".to_owned(),
                        question: "Select strategy".to_owned(),
                        is_other: false,
                        is_secret: false,
                        options: Some(vec![
                            BackendNeedsInputOption {
                                label: "safe".to_owned(),
                                description: "safe path".to_owned(),
                            },
                            BackendNeedsInputOption {
                                label: "fast".to_owned(),
                                description: "fast path".to_owned(),
                            },
                        ]),
                    }],
                }),
            ),
            (
                104_u64,
                BackendEvent::Blocked(BackendBlockedEvent {
                    reason: "  blocked by checks  ".to_owned(),
                    hint: Some("  run tests  ".to_owned()),
                    log_ref: Some("artifact://logs/blocked/104".to_owned()),
                }),
            ),
            (
                105_u64,
                BackendEvent::Artifact(BackendArtifactEvent {
                    kind: BackendArtifactKind::Other("trace".to_owned()),
                    artifact_id: None,
                    label: None,
                    uri: None,
                }),
            ),
            (
                106_u64,
                BackendEvent::Done(BackendDoneEvent { summary: None }),
            ),
            (
                107_u64,
                BackendEvent::Crashed(BackendCrashedEvent {
                    reason: "runtime panic".to_owned(),
                }),
            ),
        ];

        for (index, event) in scenarios {
            let context = context(index);
            let app_normalized = normalize_backend_event(&context, &event);
            let core_normalized = core_normalize_backend_event(&core_context(&context), &event);
            assert_eq!(
                app_normalized,
                from_core_normalized(core_normalized),
                "normalization output drifted for backend_event_index={index}"
            );
        }
    }

    #[test]
    fn output_event_is_forwarded_for_separate_raw_log_storage() {
        let output = BackendOutputEvent {
            stream: BackendOutputStream::Stdout,
            bytes: b"log line".to_vec(),
        };
        let normalized =
            normalize_backend_event(&context(1), &BackendEvent::Output(output.clone()));

        assert_eq!(normalized.raw_output, Some(output));
        assert!(normalized.artifacts.is_empty());
        assert!(normalized.events.is_empty());
    }

    #[test]
    fn checkpoint_event_creates_checkpoint_and_artifact_records() {
        let normalized = normalize_backend_event(
            &context(2),
            &BackendEvent::Checkpoint(BackendCheckpointEvent {
                summary: "implementing parser".to_owned(),
                detail: Some("updated tokenizer".to_owned()),
                file_refs: vec!["src/parser.rs".to_owned()],
            }),
        );

        assert!(normalized.raw_output.is_none());
        assert_eq!(normalized.artifacts.len(), 1);
        assert_eq!(normalized.events.len(), 2);
        assert!(matches!(
            normalized.events[1].event.payload,
            OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload { .. })
        ));
        assert_eq!(normalized.events[1].artifact_refs.len(), 1);
    }

    #[test]
    fn needs_input_event_creates_needs_decision_inbox_item() {
        let normalized = normalize_backend_event(
            &context(3),
            &BackendEvent::NeedsInput(BackendNeedsInputEvent {
                prompt_id: "q1".to_owned(),
                question: "Choose API shape".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("A".to_owned()),
                questions: Vec::new(),
            }),
        );

        assert_eq!(normalized.events.len(), 2);
        match &normalized.events[0].event.payload {
            OrchestrationEventPayload::SessionNeedsInput(payload) => {
                assert_eq!(payload.prompt, "Choose API shape");
                assert_eq!(payload.prompt_id.as_deref(), Some("q1"));
                assert_eq!(payload.options, vec!["A".to_owned(), "B".to_owned()]);
                assert_eq!(payload.default_option.as_deref(), Some("A"));
            }
            other => panic!("expected SessionNeedsInput payload, got {other:?}"),
        }
        assert!(matches!(
            normalized.events[1].event.payload,
            OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                kind: InboxItemKind::NeedsDecision,
                ..
            })
        ));
    }

    #[test]
    fn blocked_event_with_log_ref_creates_artifact_and_blocked_inbox_item() {
        let normalized = normalize_backend_event(
            &context(4),
            &BackendEvent::Blocked(BackendBlockedEvent {
                reason: "tests failing".to_owned(),
                hint: Some("run cargo test -p foo".to_owned()),
                log_ref: Some("artifact://logs/failures/123".to_owned()),
            }),
        );

        assert_eq!(normalized.artifacts.len(), 1);
        assert_eq!(normalized.events.len(), 3);
        match &normalized.events[1].event.payload {
            OrchestrationEventPayload::SessionBlocked(payload) => {
                assert_eq!(payload.reason, "tests failing");
                assert_eq!(payload.hint.as_deref(), Some("run cargo test -p foo"));
                assert_eq!(
                    payload.log_ref.as_deref(),
                    Some("artifact://logs/failures/123")
                );
            }
            other => panic!("expected SessionBlocked payload, got {other:?}"),
        }
        assert_eq!(normalized.events[1].artifact_refs.len(), 1);
        assert!(matches!(
            normalized.events[2].event.payload,
            OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                kind: InboxItemKind::Blocked,
                ..
            })
        ));
    }

    #[test]
    fn backend_artifact_event_normalizes_to_domain_artifact_and_event() {
        let normalized = normalize_backend_event(
            &context(5),
            &BackendEvent::Artifact(BackendArtifactEvent {
                kind: BackendArtifactKind::PullRequest,
                artifact_id: Some(RuntimeArtifactId::new("artifact-pr-1")),
                label: Some("Draft PR".to_owned()),
                uri: Some("https://example.test/pulls/1".to_owned()),
            }),
        );

        assert_eq!(normalized.artifacts.len(), 1);
        assert_eq!(normalized.events.len(), 1);
        assert!(matches!(
            normalized.events[0].event.payload,
            OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                kind: ArtifactKind::PR,
                ..
            })
        ));
        assert_eq!(
            normalized.artifacts[0].artifact_id,
            ArtifactId::new("artifact-pr-1")
        );
    }

    #[test]
    fn backend_artifact_event_with_blank_id_falls_back_to_context_scoped_artifact_id() {
        let normalized = normalize_backend_event(
            &context(8),
            &BackendEvent::Artifact(BackendArtifactEvent {
                kind: BackendArtifactKind::Diff,
                artifact_id: Some(RuntimeArtifactId::new("   ")),
                label: Some("Patch".to_owned()),
                uri: Some("artifact://patches/8".to_owned()),
            }),
        );

        assert_eq!(normalized.artifacts.len(), 1);
        assert_eq!(
            normalized.artifacts[0].artifact_id,
            ArtifactId::new("artifact-sess-116-8-backend-artifact")
        );
    }

    #[test]
    fn done_event_creates_checkpoint_completed_signal_and_fyi_inbox_item() {
        let normalized = normalize_backend_event(
            &context(6),
            &BackendEvent::Done(BackendDoneEvent {
                summary: Some("all checks passed".to_owned()),
            }),
        );

        assert_eq!(normalized.artifacts.len(), 1);
        assert_eq!(normalized.events.len(), 4);
        assert!(matches!(
            normalized.events[1].event.payload,
            OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload { .. })
        ));
        assert!(matches!(
            normalized.events[2].event.payload,
            OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload { .. })
        ));
        assert!(matches!(
            normalized.events[3].event.payload,
            OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                kind: InboxItemKind::FYI,
                ..
            })
        ));
    }

    #[test]
    fn crashed_event_creates_blocked_and_crashed_domain_signals() {
        let normalized = normalize_backend_event(
            &context(7),
            &BackendEvent::Crashed(BackendCrashedEvent {
                reason: "panic in parser".to_owned(),
            }),
        );

        assert_eq!(normalized.artifacts.len(), 1);
        assert_eq!(normalized.events.len(), 4);
        match &normalized.events[1].event.payload {
            OrchestrationEventPayload::SessionBlocked(payload) => {
                assert_eq!(payload.reason, "panic in parser");
                assert!(payload.hint.is_none());
                assert_eq!(
                    payload.log_ref.as_deref(),
                    Some("artifact://session/sess-116/crashed/7")
                );
            }
            other => panic!("expected SessionBlocked payload, got {other:?}"),
        }
        assert!(matches!(
            normalized.events[2].event.payload,
            OrchestrationEventPayload::SessionCrashed(SessionCrashedPayload { .. })
        ));
        assert!(matches!(
            normalized.events[3].event.payload,
            OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                kind: InboxItemKind::Blocked,
                ..
            })
        ));
    }
}
