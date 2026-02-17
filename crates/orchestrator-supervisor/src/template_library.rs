use std::collections::BTreeMap;

use orchestrator_core::{
    command_ids, CoreError, LlmMessage, LlmRole, OrchestrationEventType, RetrievalScope,
};

use crate::BoundedContextPack;

pub const SUPERVISOR_TEMPLATE_CURRENT_ACTIVITY: &str = "current_activity";
pub const SUPERVISOR_TEMPLATE_WHAT_CHANGED: &str = "what_changed";
pub const SUPERVISOR_TEMPLATE_WHAT_NEEDS_ME: &str = "what_needs_me";
pub const SUPERVISOR_TEMPLATE_RISK_ASSESSMENT: &str = "risk_assessment";
pub const SUPERVISOR_TEMPLATE_RECOMMENDED_RESPONSE: &str = "recommended_response";

const SUPERVISOR_SYSTEM_PROMPT: &str = concat!(
    "You are the orchestrator supervisor.\n",
    "Answer only from the provided context pack and avoid inventing facts.\n",
    "If evidence is missing, say Unknown and call out what is missing.\n",
    "Keep responses terse and operationally actionable."
);

const ALL_TEMPLATES: [SupervisorTemplate; 5] = [
    SupervisorTemplate::CurrentActivity,
    SupervisorTemplate::WhatChanged,
    SupervisorTemplate::WhatNeedsMe,
    SupervisorTemplate::RiskAssessment,
    SupervisorTemplate::RecommendedResponse,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupervisorTemplate {
    CurrentActivity,
    WhatChanged,
    WhatNeedsMe,
    RiskAssessment,
    RecommendedResponse,
}

impl SupervisorTemplate {
    pub fn all() -> &'static [SupervisorTemplate] {
        &ALL_TEMPLATES
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::CurrentActivity => SUPERVISOR_TEMPLATE_CURRENT_ACTIVITY,
            Self::WhatChanged => SUPERVISOR_TEMPLATE_WHAT_CHANGED,
            Self::WhatNeedsMe => SUPERVISOR_TEMPLATE_WHAT_NEEDS_ME,
            Self::RiskAssessment => SUPERVISOR_TEMPLATE_RISK_ASSESSMENT,
            Self::RecommendedResponse => SUPERVISOR_TEMPLATE_RECOMMENDED_RESPONSE,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::CurrentActivity => "Current activity",
            Self::WhatChanged => "What changed",
            Self::WhatNeedsMe => "What needs me",
            Self::RiskAssessment => "Risk assessment",
            Self::RecommendedResponse => "Recommended response",
        }
    }

    pub fn resolve(raw: &str) -> Result<Self, CoreError> {
        let normalized = normalize_template_key(raw);
        match normalized.as_str() {
            "currentactivity" | "status" | "statuscurrentsession" => Ok(Self::CurrentActivity),
            "whatchanged" => Ok(Self::WhatChanged),
            "whatneedsme" => Ok(Self::WhatNeedsMe),
            "riskassessment" => Ok(Self::RiskAssessment),
            "recommendedresponse" => Ok(Self::RecommendedResponse),
            _ => Err(CoreError::InvalidCommandArgs {
                command_id: command_ids::SUPERVISOR_QUERY.to_owned(),
                reason: format!(
                    "unknown supervisor template '{}'; known templates: {}",
                    raw.trim(),
                    supervisor_template_catalog()
                        .iter()
                        .map(|template| template.key())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    fn objective(self) -> &'static str {
        match self {
            Self::CurrentActivity => {
                "Summarize what the active scope is currently doing and what is waiting."
            }
            Self::WhatChanged => "Summarize concrete deltas from recent events and artifacts.",
            Self::WhatNeedsMe => {
                "Call out only actions or decisions that require explicit human input."
            }
            Self::RiskAssessment => {
                "Identify material delivery or quality risks from available evidence."
            }
            Self::RecommendedResponse => {
                "Draft the next operator response that best unblocks safe progress."
            }
        }
    }

    fn output_contract(self) -> &'static str {
        match self {
            Self::CurrentActivity => concat!(
                "Current activity:\n",
                "- One sentence status summary.\n",
                "In progress:\n",
                "- Up to 3 bullets with active work and references.\n",
                "Waiting:\n",
                "- Bullets for blockers/dependencies; use `None` if clear.\n",
                "Next checkpoint:\n",
                "- One immediate next observable milestone."
            ),
            Self::WhatChanged => concat!(
                "What changed:\n",
                "- Up to 5 bullets describing concrete deltas; cite event/artifact IDs.\n",
                "Impact:\n",
                "- One sentence on net effect to delivery."
            ),
            Self::WhatNeedsMe => concat!(
                "What needs me:\n",
                "- Ordered bullets for required decisions/actions.\n",
                "Why now:\n",
                "- One short line explaining urgency.\n",
                "If no action:\n",
                "- One line with expected stall/failure mode."
            ),
            Self::RiskAssessment => concat!(
                "Risk assessment:\n",
                "- Up to 4 bullets in format `[severity] risk -> mitigation`.\n",
                "Top risk:\n",
                "- Single line naming highest-priority risk and confidence (high/med/low)."
            ),
            Self::RecommendedResponse => concat!(
                "Recommended response:\n",
                "- A direct message to send to the worker.\n",
                "Rationale:\n",
                "- One line connecting to evidence.\n",
                "Fallback:\n",
                "- One safer alternative if assumptions are wrong."
            ),
        }
    }
}

pub fn supervisor_template_catalog() -> &'static [SupervisorTemplate] {
    SupervisorTemplate::all()
}

pub fn build_template_messages(
    raw_template: &str,
    context: &BoundedContextPack,
) -> Result<Vec<LlmMessage>, CoreError> {
    build_template_messages_with_variables(raw_template, &BTreeMap::new(), context)
}

pub fn build_template_messages_with_variables(
    raw_template: &str,
    variables: &BTreeMap<String, String>,
    context: &BoundedContextPack,
) -> Result<Vec<LlmMessage>, CoreError> {
    let template = SupervisorTemplate::resolve(raw_template)?;
    let rendered_variables = render_template_variables(variables);
    let user_prompt = format!(
        "Template: {} ({})\n\
Objective: {}\n\n\
Output contract:\n{}\n\n\
{}\
Context pack:\n{}",
        template.title(),
        template.key(),
        template.objective(),
        template.output_contract(),
        rendered_variables,
        render_context_pack(context)
    );

    Ok(vec![
        LlmMessage {
            role: LlmRole::System,
            content: SUPERVISOR_SYSTEM_PROMPT.to_owned(),
            name: None,
        },
        LlmMessage {
            role: LlmRole::User,
            content: user_prompt,
            name: None,
        },
    ])
}

pub fn build_freeform_messages(
    raw_query: &str,
    context: &BoundedContextPack,
) -> Result<Vec<LlmMessage>, CoreError> {
    let query = raw_query.trim();
    if query.is_empty() {
        return Err(CoreError::InvalidCommandArgs {
            command_id: command_ids::SUPERVISOR_QUERY.to_owned(),
            reason: "freeform supervisor query requires a non-empty query string".to_owned(),
        });
    }

    let user_prompt = format!(
        "Operator question:\n\
{query}\n\n\
Respond directly to this question using only the context pack.\n\
If evidence is missing, answer `Unknown` and list what is missing.\n\n\
Context pack:\n{}",
        render_context_pack(context)
    );

    Ok(vec![
        LlmMessage {
            role: LlmRole::System,
            content: SUPERVISOR_SYSTEM_PROMPT.to_owned(),
            name: None,
        },
        LlmMessage {
            role: LlmRole::User,
            content: user_prompt,
            name: None,
        },
    ])
}

fn render_template_variables(variables: &BTreeMap<String, String>) -> String {
    if variables.is_empty() {
        return String::new();
    }

    let mut lines = String::from("Template variables:\n");
    for (key, value) in variables {
        lines.push_str(format!("- {key}={value}\n").as_str());
    }
    lines.push('\n');
    lines
}

fn render_context_pack(context: &BoundedContextPack) -> String {
    let mut lines = vec![
        format!("Scope: {}", render_scope(&context.scope)),
        "Focus filters:".to_owned(),
        format!("- scope_hint: {}", context.focus_filters.scope_hint),
        format!(
            "- selected_work_item_id: {}",
            context
                .focus_filters
                .selected_work_item_id
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_else(|| "none".to_owned())
        ),
        format!(
            "- selected_session_id: {}",
            context
                .focus_filters
                .selected_session_id
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_else(|| "none".to_owned())
        ),
        "Ticket status context:".to_owned(),
        format!(
            "- ticket_ref: {}",
            context
                .ticket_status
                .ticket_ref
                .as_deref()
                .unwrap_or("Unknown")
        ),
        format!(
            "- ticket_id: {}",
            context
                .ticket_status
                .ticket_id
                .as_deref()
                .unwrap_or("Unknown")
        ),
        format!(
            "- title: {}",
            context.ticket_status.title.as_deref().unwrap_or("Unknown")
        ),
        format!(
            "- assignee: {}",
            context
                .ticket_status
                .assignee
                .as_deref()
                .unwrap_or("Unknown")
        ),
        format!(
            "- state: {}",
            context.ticket_status.state.as_deref().unwrap_or("Unknown")
        ),
        format!(
            "- priority: {}",
            context
                .ticket_status
                .priority
                .map(|value| value.to_string())
                .unwrap_or_else(|| "Unknown".to_owned())
        ),
        "Recent status transitions:".to_owned(),
    ];

    if context.ticket_status.recent_transitions.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(
            context
                .ticket_status
                .recent_transitions
                .iter()
                .map(|transition| {
                    let from_state = transition
                        .from_state
                        .as_deref()
                        .unwrap_or("Unknown")
                        .to_owned();
                    format!(
                        "- {} {} {} -> {} ({})",
                        transition.occurred_at,
                        transition.source,
                        from_state,
                        transition.to_state,
                        transition.event_id
                    )
                }),
        );
    }

    if let Some(fallback) = context.ticket_status.fallback_message.as_deref() {
        lines.push(format!("Ticket status fallback: {fallback}"));
    }

    lines.extend([
        format!(
            "Stats: total_matching_events={} dropped_events={} evidence_candidates={} dropped_evidence={} missing_evidence={}",
            context.stats.total_matching_events,
            context.stats.dropped_events,
            context.stats.total_evidence_candidates,
            context.stats.dropped_evidence,
            context.stats.missing_evidence
        ),
        "Events (newest first):".to_owned(),
    ]);

    if context.events.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(context.events.iter().map(|event| {
            let mut line = format!(
                "- [{}] {} {} @ {}: {}",
                event.sequence,
                event.event_id,
                render_event_type(&event.event_type),
                event.occurred_at,
                event.summary
            );
            if !event.artifact_ids.is_empty() {
                let artifact_ids = event
                    .artifact_ids
                    .iter()
                    .map(|artifact_id| artifact_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                line.push_str(format!(" | artifacts: {artifact_ids}").as_str());
            }
            line
        }));
    }

    lines.push("Evidence (newest first):".to_owned());
    if context.evidence.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(context.evidence.iter().map(|evidence| {
            format!(
                "- {} {:?} @ {} ({}) {}",
                evidence.artifact_id.as_str(),
                evidence.kind,
                evidence.created_at,
                evidence.storage_ref,
                evidence.metadata_summary
            )
        }));
    }

    lines.join("\n")
}

fn render_scope(scope: &RetrievalScope) -> String {
    match scope {
        RetrievalScope::Global => "global".to_owned(),
        RetrievalScope::WorkItem(work_item_id) => format!("work_item:{}", work_item_id.as_str()),
        RetrievalScope::Session(session_id) => format!("session:{}", session_id.as_str()),
    }
}

fn render_event_type(event_type: &OrchestrationEventType) -> &'static str {
    match event_type {
        OrchestrationEventType::TicketSynced => "ticket_synced",
        OrchestrationEventType::WorkItemCreated => "work_item_created",
        OrchestrationEventType::WorktreeCreated => "worktree_created",
        OrchestrationEventType::SessionSpawned => "session_spawned",
        OrchestrationEventType::SessionCheckpoint => "session_checkpoint",
        OrchestrationEventType::SessionNeedsInput => "session_needs_input",
        OrchestrationEventType::SessionBlocked => "session_blocked",
        OrchestrationEventType::SessionCompleted => "session_completed",
        OrchestrationEventType::SessionCrashed => "session_crashed",
        OrchestrationEventType::ArtifactCreated => "artifact_created",
        OrchestrationEventType::WorkflowTransition => "workflow_transition",
        OrchestrationEventType::InboxItemCreated => "inbox_item_created",
        OrchestrationEventType::InboxItemResolved => "inbox_item_resolved",
        OrchestrationEventType::UserResponded => "user_responded",
    }
}

fn normalize_template_key(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use orchestrator_core::{ArtifactId, ArtifactKind, WorkItemId, WorkerSessionId};

    use crate::{
        RetrievalFocusFilters, RetrievalPackEvent, RetrievalPackEvidence, RetrievalPackStats,
        TicketStatusContext, TicketStatusTransition,
    };

    use super::*;

    #[test]
    fn template_catalog_contains_expected_templates_in_stable_order() {
        let catalog = supervisor_template_catalog();
        assert_eq!(
            catalog
                .iter()
                .map(|template| template.key())
                .collect::<Vec<_>>(),
            vec![
                SUPERVISOR_TEMPLATE_CURRENT_ACTIVITY,
                SUPERVISOR_TEMPLATE_WHAT_CHANGED,
                SUPERVISOR_TEMPLATE_WHAT_NEEDS_ME,
                SUPERVISOR_TEMPLATE_RISK_ASSESSMENT,
                SUPERVISOR_TEMPLATE_RECOMMENDED_RESPONSE,
            ]
        );
        assert_eq!(catalog[0].title(), "Current activity");
        assert_eq!(catalog[4].title(), "Recommended response");
    }

    #[test]
    fn resolve_template_accepts_keys_titles_and_legacy_alias() {
        assert_eq!(
            SupervisorTemplate::resolve("current_activity").expect("resolve current"),
            SupervisorTemplate::CurrentActivity
        );
        assert_eq!(
            SupervisorTemplate::resolve("Current activity").expect("resolve title"),
            SupervisorTemplate::CurrentActivity
        );
        assert_eq!(
            SupervisorTemplate::resolve("status_current_session").expect("resolve legacy"),
            SupervisorTemplate::CurrentActivity
        );
        assert_eq!(
            SupervisorTemplate::resolve("status").expect("resolve short alias"),
            SupervisorTemplate::CurrentActivity
        );
        assert_eq!(
            SupervisorTemplate::resolve("what-changed").expect("resolve hyphenated"),
            SupervisorTemplate::WhatChanged
        );
        assert_eq!(
            SupervisorTemplate::resolve("What needs me").expect("resolve title"),
            SupervisorTemplate::WhatNeedsMe
        );
    }

    #[test]
    fn resolve_template_rejects_unknown_with_actionable_error() {
        let err = SupervisorTemplate::resolve("unknown-template").expect_err("unknown template");
        let message = err.to_string();
        assert!(message.contains("unknown supervisor template"));
        assert!(message.contains(SUPERVISOR_TEMPLATE_CURRENT_ACTIVITY));
        assert!(message.contains(SUPERVISOR_TEMPLATE_RECOMMENDED_RESPONSE));
    }

    #[test]
    fn build_template_messages_includes_context_and_format_contract() {
        let context = sample_context_pack();

        let messages = build_template_messages("risk_assessment", &context).expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, LlmRole::System);
        assert_eq!(messages[1].role, LlmRole::User);
        assert!(messages[1].content.contains("Template: Risk assessment"));
        assert!(messages[1].content.contains("Risk assessment:"));
        assert!(messages[1].content.contains("evt-1"));
        assert!(messages[1].content.contains("art-2"));
        assert!(messages[1].content.contains("Scope: session:sess-7"));
        assert!(messages[1].content.contains("Ticket status context:"));
        assert!(messages[1].content.contains("ticket_id: AP-129"));
    }

    #[test]
    fn build_template_messages_with_variables_lists_sorted_template_variables() {
        let context = sample_context_pack();
        let variables = BTreeMap::from([
            ("ticket".to_owned(), "AP-129".to_owned()),
            ("channel".to_owned(), "review".to_owned()),
        ]);

        let messages =
            build_template_messages_with_variables("what_needs_me", &variables, &context)
                .expect("messages with variables");
        assert_eq!(messages.len(), 2);
        assert!(messages[1].content.contains("Template variables:"));
        assert!(messages[1].content.contains("- channel=review"));
        assert!(messages[1].content.contains("- ticket=AP-129"));
    }

    #[test]
    fn build_freeform_messages_includes_operator_question_and_context() {
        let context = sample_context_pack();
        let messages =
            build_freeform_messages("What changed today?", &context).expect("freeform messages");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, LlmRole::System);
        assert_eq!(messages[1].role, LlmRole::User);
        assert!(messages[1].content.contains("Operator question:"));
        assert!(messages[1].content.contains("What changed today?"));
        assert!(messages[1].content.contains("Context pack:"));
        assert!(messages[1].content.contains("evt-1"));
        assert!(messages[1].content.contains("Focus filters:"));
    }

    #[test]
    fn build_freeform_messages_rejects_blank_query() {
        let context = sample_context_pack();
        let err = build_freeform_messages(" \n\t", &context).expect_err("blank query should fail");
        assert!(err
            .to_string()
            .contains("freeform supervisor query requires a non-empty query string"));
    }

    fn sample_context_pack() -> BoundedContextPack {
        BoundedContextPack {
            scope: RetrievalScope::Session(WorkerSessionId::new("sess-7")),
            focus_filters: RetrievalFocusFilters {
                scope_hint: "session:sess-7".to_owned(),
                selected_work_item_id: Some(WorkItemId::new("wi-3")),
                selected_session_id: Some(WorkerSessionId::new("sess-7")),
            },
            ticket_status: TicketStatusContext {
                ticket_ref: Some("linear:issue-129".to_owned()),
                ticket_id: Some("AP-129".to_owned()),
                title: Some("Refine status context payload".to_owned()),
                assignee: Some("alice".to_owned()),
                state: Some("In Progress".to_owned()),
                priority: Some(1),
                recent_transitions: vec![TicketStatusTransition {
                    occurred_at: "2026-02-16T10:29:00Z".to_owned(),
                    event_id: "evt-ticket-sync".to_owned(),
                    source: "ticket_sync".to_owned(),
                    from_state: Some("Todo".to_owned()),
                    to_state: "In Progress".to_owned(),
                }],
                fallback_message: None,
            },
            events: vec![RetrievalPackEvent {
                event_id: "evt-1".to_owned(),
                sequence: 42,
                occurred_at: "2026-02-16T10:30:00Z".to_owned(),
                event_type: OrchestrationEventType::SessionNeedsInput,
                work_item_id: Some(WorkItemId::new("wi-3")),
                session_id: Some(WorkerSessionId::new("sess-7")),
                summary: "session requires decision on API contract".to_owned(),
                artifact_ids: vec![ArtifactId::new("art-2")],
            }],
            evidence: vec![RetrievalPackEvidence {
                artifact_id: ArtifactId::new("art-2"),
                work_item_id: WorkItemId::new("wi-3"),
                kind: ArtifactKind::LogSnippet,
                created_at: "2026-02-16T10:31:00Z".to_owned(),
                storage_ref: "artifact://logs/2".to_owned(),
                metadata_summary: "reason=waiting on approval".to_owned(),
            }],
            stats: RetrievalPackStats {
                total_matching_events: 5,
                dropped_events: 4,
                total_evidence_candidates: 2,
                dropped_evidence: 1,
                missing_evidence: 0,
            },
        }
    }
}
