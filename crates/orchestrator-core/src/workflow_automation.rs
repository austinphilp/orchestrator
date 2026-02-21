use serde::{Deserialize, Deserializer, Serialize};

use crate::status::{InboxItemKind, WorkflowState};
use crate::workflow::{
    validate_workflow_transition, WorkflowGuardContext, WorkflowTransitionError,
    WorkflowTransitionReason,
};

pub const DRAFT_PULL_REQUEST_INBOX_TITLE: &str = "Draft pull request created.";
pub const NEEDS_APPROVAL_INBOX_TITLE: &str =
    "Approval needed: convert draft pull request to ready for review.";
pub const READY_FOR_REVIEW_INBOX_TITLE: &str =
    "Ready for review: tests passed and draft PR is available.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HumanApprovalGateMode {
    HumanOnly,
    Automated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowAutomationGatePolicy {
    pub requirements_ambiguity: HumanApprovalGateMode,
    pub convert_draft_to_ready: HumanApprovalGateMode,
    pub merge: HumanApprovalGateMode,
}

impl Default for WorkflowAutomationGatePolicy {
    fn default() -> Self {
        Self {
            requirements_ambiguity: HumanApprovalGateMode::HumanOnly,
            convert_draft_to_ready: HumanApprovalGateMode::HumanOnly,
            merge: HumanApprovalGateMode::HumanOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowNotificationRoute {
    Interrupt,
    Digest,
}

impl Default for WorkflowNotificationRoute {
    fn default() -> Self {
        Self::Interrupt
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowAutomationNotificationPolicy {
    pub draft_pull_request_created: Option<WorkflowNotificationRoute>,
    pub convert_draft_to_ready_gate: Option<WorkflowNotificationRoute>,
    pub ready_for_review: Option<WorkflowNotificationRoute>,
}

impl Default for WorkflowAutomationNotificationPolicy {
    fn default() -> Self {
        Self {
            draft_pull_request_created: None,
            convert_draft_to_ready_gate: Some(WorkflowNotificationRoute::Interrupt),
            ready_for_review: Some(WorkflowNotificationRoute::Digest),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct WorkflowAutomationPolicy {
    pub gates: WorkflowAutomationGatePolicy,
    pub notifications: WorkflowAutomationNotificationPolicy,
}

impl<'de> Deserialize<'de> for WorkflowAutomationPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Debug, Default, Deserialize)]
        struct PolicyCompat {
            #[serde(default)]
            gates: Option<WorkflowAutomationGatePolicy>,
            #[serde(default)]
            notifications: Option<WorkflowAutomationNotificationPolicy>,
            #[serde(default)]
            auto_progress_to_awaiting_review: Option<bool>,
            #[serde(default)]
            emit_ready_for_review_inbox: Option<bool>,
        }

        let compat = PolicyCompat::deserialize(deserializer)?;
        let mut policy = WorkflowAutomationPolicy::default();

        if let Some(auto_progress) = compat.auto_progress_to_awaiting_review {
            policy.gates.convert_draft_to_ready = if auto_progress {
                HumanApprovalGateMode::HumanOnly
            } else {
                HumanApprovalGateMode::Automated
            };
        }

        if let Some(emit_ready_for_review) = compat.emit_ready_for_review_inbox {
            policy.notifications.ready_for_review =
                emit_ready_for_review.then_some(WorkflowNotificationRoute::Digest);
        }

        if let Some(gates) = compat.gates {
            policy.gates = gates;
        }

        if let Some(notifications) = compat.notifications {
            policy.notifications = notifications;
        }

        Ok(policy)
    }
}

impl Default for WorkflowAutomationPolicy {
    fn default() -> Self {
        Self {
            gates: WorkflowAutomationGatePolicy::default(),
            notifications: WorkflowAutomationNotificationPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowAutomationStep {
    BeginImplementation,
    CreateDraftPullRequest,
    ProgressReviewReadiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAutomationTransitionIntent {
    pub from: WorkflowState,
    pub to: WorkflowState,
    pub reason: WorkflowTransitionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAutomationInboxIntent {
    pub kind: InboxItemKind,
    pub title: String,
    #[serde(default)]
    pub route: WorkflowNotificationRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowAutomationPlan {
    pub steps: Vec<WorkflowAutomationStep>,
    pub follow_up_transitions: Vec<WorkflowAutomationTransitionIntent>,
    pub inbox_items: Vec<WorkflowAutomationInboxIntent>,
}

pub fn plan_workflow_automation(
    from: &WorkflowState,
    to: &WorkflowState,
    reason: &WorkflowTransitionReason,
    guards: &WorkflowGuardContext,
) -> Result<WorkflowAutomationPlan, WorkflowTransitionError> {
    let default_policy = WorkflowAutomationPolicy::default();
    plan_workflow_automation_with_policy(from, to, reason, guards, &default_policy)
}

pub fn plan_workflow_automation_with_policy(
    from: &WorkflowState,
    to: &WorkflowState,
    reason: &WorkflowTransitionReason,
    guards: &WorkflowGuardContext,
    policy: &WorkflowAutomationPolicy,
) -> Result<WorkflowAutomationPlan, WorkflowTransitionError> {
    validate_workflow_transition(from, to, reason, guards)?;

    let mut plan = WorkflowAutomationPlan::default();

    match (from, to, reason) {
        (
            WorkflowState::Planning,
            WorkflowState::Implementing,
            WorkflowTransitionReason::PlanCommitted,
        ) => {
            plan.steps.push(WorkflowAutomationStep::BeginImplementation);
        }
        (
            WorkflowState::Implementing,
            WorkflowState::PRDrafted,
            WorkflowTransitionReason::DraftPullRequestCreated,
        ) => {
            plan.steps
                .push(WorkflowAutomationStep::CreateDraftPullRequest);
            append_notification(
                &mut plan,
                policy.notifications.draft_pull_request_created,
                InboxItemKind::FYI,
                DRAFT_PULL_REQUEST_INBOX_TITLE,
            );

            append_review_readiness_actions(&mut plan, guards, policy)?;
        }
        _ => {}
    }

    Ok(plan)
}

fn append_review_readiness_actions(
    plan: &mut WorkflowAutomationPlan,
    guards: &WorkflowGuardContext,
    policy: &WorkflowAutomationPolicy,
) -> Result<(), WorkflowTransitionError> {
    if !guards.tests_passed || !guards.has_draft_pr {
        return Ok(());
    }

    match policy.gates.convert_draft_to_ready {
        HumanApprovalGateMode::HumanOnly => {
            let follow_up_reason = WorkflowTransitionReason::AwaitingApproval;
            validate_workflow_transition(
                &WorkflowState::PRDrafted,
                &WorkflowState::AwaitingYourReview,
                &follow_up_reason,
                guards,
            )?;

            plan.steps
                .push(WorkflowAutomationStep::ProgressReviewReadiness);
            plan.follow_up_transitions
                .push(WorkflowAutomationTransitionIntent {
                    from: WorkflowState::PRDrafted,
                    to: WorkflowState::AwaitingYourReview,
                    reason: follow_up_reason,
                });

            append_notification(
                plan,
                policy.notifications.convert_draft_to_ready_gate,
                InboxItemKind::NeedsApproval,
                NEEDS_APPROVAL_INBOX_TITLE,
            );
        }
        HumanApprovalGateMode::Automated => {
            append_notification(
                plan,
                policy.notifications.ready_for_review,
                InboxItemKind::ReadyForReview,
                READY_FOR_REVIEW_INBOX_TITLE,
            );
        }
    }

    Ok(())
}

fn append_notification(
    plan: &mut WorkflowAutomationPlan,
    route: Option<WorkflowNotificationRoute>,
    kind: InboxItemKind,
    title: &str,
) {
    let Some(route) = route else {
        return;
    };

    plan.inbox_items.push(WorkflowAutomationInboxIntent {
        kind,
        title: title.to_owned(),
        route,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn planning_to_implementing_triggers_begin_implementation_step() {
        let plan = plan_workflow_automation(
            &WorkflowState::Planning,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::PlanCommitted,
            &WorkflowGuardContext {
                has_active_session: true,
                plan_ready: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should be valid");

        assert_eq!(
            plan.steps,
            vec![WorkflowAutomationStep::BeginImplementation]
        );
        assert!(plan.follow_up_transitions.is_empty());
        assert!(plan.inbox_items.is_empty());
    }

    #[test]
    fn pr_drafted_with_passing_tests_progresses_to_review_readiness() {
        let plan = plan_workflow_automation(
            &WorkflowState::Implementing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                tests_passed: true,
                has_draft_pr: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should be valid");

        assert_eq!(
            plan.steps,
            vec![
                WorkflowAutomationStep::CreateDraftPullRequest,
                WorkflowAutomationStep::ProgressReviewReadiness,
            ]
        );
        assert_eq!(plan.follow_up_transitions.len(), 1);
        assert_eq!(plan.follow_up_transitions[0].from, WorkflowState::PRDrafted);
        assert_eq!(
            plan.follow_up_transitions[0].to,
            WorkflowState::AwaitingYourReview
        );
        assert_eq!(
            plan.follow_up_transitions[0].reason,
            WorkflowTransitionReason::AwaitingApproval
        );
        assert_eq!(plan.inbox_items.len(), 1);
        assert_eq!(plan.inbox_items[0].kind, InboxItemKind::NeedsApproval);
        assert_eq!(plan.inbox_items[0].title, NEEDS_APPROVAL_INBOX_TITLE);
        assert_eq!(
            plan.inbox_items[0].route,
            WorkflowNotificationRoute::Interrupt
        );
    }

    #[test]
    fn pr_drafted_without_passing_tests_fails_transition_validation() {
        let err = plan_workflow_automation(
            &WorkflowState::Implementing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                has_draft_pr: true,
                tests_passed: false,
                ..WorkflowGuardContext::default()
            },
        )
        .expect_err("transition should fail without passing tests");
        assert!(matches!(
            err,
            WorkflowTransitionError::GuardFailed {
                guard: crate::workflow::WorkflowGuard::PassingTests,
                ..
            }
        ));
    }

    #[test]
    fn implementing_to_pr_drafted_with_passing_tests_progresses_review_readiness() {
        let plan = plan_workflow_automation(
            &WorkflowState::Implementing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                has_draft_pr: true,
                tests_passed: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should be valid");

        assert_eq!(
            plan.steps,
            vec![
                WorkflowAutomationStep::CreateDraftPullRequest,
                WorkflowAutomationStep::ProgressReviewReadiness,
            ]
        );
        assert_eq!(plan.follow_up_transitions.len(), 1);
        assert_eq!(plan.inbox_items.len(), 1);
        assert_eq!(plan.inbox_items[0].kind, InboxItemKind::NeedsApproval);
    }

    #[test]
    fn policy_can_auto_approve_gate_and_route_ready_for_review_to_digest() {
        let plan = plan_workflow_automation_with_policy(
            &WorkflowState::Implementing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                tests_passed: true,
                has_draft_pr: true,
                ..WorkflowGuardContext::default()
            },
            &WorkflowAutomationPolicy {
                gates: WorkflowAutomationGatePolicy {
                    convert_draft_to_ready: HumanApprovalGateMode::Automated,
                    ..WorkflowAutomationGatePolicy::default()
                },
                notifications: WorkflowAutomationNotificationPolicy {
                    draft_pull_request_created: None,
                    convert_draft_to_ready_gate: None,
                    ready_for_review: Some(WorkflowNotificationRoute::Digest),
                },
            },
        )
        .expect("transition should be valid");

        assert_eq!(
            plan.steps,
            vec![WorkflowAutomationStep::CreateDraftPullRequest]
        );
        assert!(plan.follow_up_transitions.is_empty());
        assert_eq!(plan.inbox_items.len(), 1);
        assert_eq!(plan.inbox_items[0].kind, InboxItemKind::ReadyForReview);
        assert_eq!(plan.inbox_items[0].route, WorkflowNotificationRoute::Digest);
    }

    #[test]
    fn policy_can_emit_pr_draft_notification_without_review_gate_notification() {
        let plan = plan_workflow_automation_with_policy(
            &WorkflowState::Implementing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                has_draft_pr: true,
                tests_passed: true,
                ..WorkflowGuardContext::default()
            },
            &WorkflowAutomationPolicy {
                gates: WorkflowAutomationGatePolicy::default(),
                notifications: WorkflowAutomationNotificationPolicy {
                    draft_pull_request_created: Some(WorkflowNotificationRoute::Digest),
                    convert_draft_to_ready_gate: None,
                    ready_for_review: None,
                },
            },
        )
        .expect("transition should be valid");

        assert_eq!(
            plan.steps,
            vec![
                WorkflowAutomationStep::CreateDraftPullRequest,
                WorkflowAutomationStep::ProgressReviewReadiness,
            ]
        );
        assert_eq!(plan.follow_up_transitions.len(), 1);
        assert_eq!(plan.inbox_items.len(), 1);
        assert_eq!(plan.inbox_items[0].kind, InboxItemKind::FYI);
        assert_eq!(plan.inbox_items[0].title, DRAFT_PULL_REQUEST_INBOX_TITLE);
        assert_eq!(plan.inbox_items[0].route, WorkflowNotificationRoute::Digest);
    }

    #[test]
    fn transitions_outside_ap132_scope_do_not_schedule_steps() {
        let plan = plan_workflow_automation(
            &WorkflowState::AwaitingYourReview,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::ImplementationResumed,
            &WorkflowGuardContext {
                has_active_session: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should be valid");

        assert!(plan.steps.is_empty());
        assert!(plan.follow_up_transitions.is_empty());
        assert!(plan.inbox_items.is_empty());
    }

    #[test]
    fn invalid_transition_returns_validation_error() {
        let err = plan_workflow_automation(
            &WorkflowState::New,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext::default(),
        )
        .expect_err("invalid transition should fail");

        assert!(matches!(
            err,
            WorkflowTransitionError::InvalidTransition { .. }
        ));
    }

    #[test]
    fn policy_deserializes_legacy_boolean_fields() {
        let policy: WorkflowAutomationPolicy = serde_json::from_value(json!({
            "auto_progress_to_awaiting_review": false,
            "emit_ready_for_review_inbox": false
        }))
        .expect("legacy policy should deserialize");

        assert_eq!(
            policy.gates.convert_draft_to_ready,
            HumanApprovalGateMode::Automated
        );
        assert_eq!(policy.notifications.ready_for_review, None);
        assert_eq!(
            policy.notifications.convert_draft_to_ready_gate,
            Some(WorkflowNotificationRoute::Interrupt)
        );
    }

    #[test]
    fn policy_supports_partial_nested_configuration_with_defaults() {
        let policy: WorkflowAutomationPolicy = serde_json::from_value(json!({
            "notifications": {
                "draft_pull_request_created": "Digest"
            }
        }))
        .expect("partial policy should deserialize");

        assert_eq!(
            policy.notifications.draft_pull_request_created,
            Some(WorkflowNotificationRoute::Digest)
        );
        assert_eq!(
            policy.notifications.convert_draft_to_ready_gate,
            Some(WorkflowNotificationRoute::Interrupt)
        );
        assert_eq!(
            policy.gates.convert_draft_to_ready,
            HumanApprovalGateMode::HumanOnly
        );
    }

    #[test]
    fn policy_prefers_new_fields_when_legacy_and_new_are_both_present() {
        let policy: WorkflowAutomationPolicy = serde_json::from_value(json!({
            "auto_progress_to_awaiting_review": true,
            "emit_ready_for_review_inbox": false,
            "gates": {
                "convert_draft_to_ready": "Automated"
            },
            "notifications": {
                "ready_for_review": "Interrupt"
            }
        }))
        .expect("policy should deserialize");

        assert_eq!(
            policy.gates.convert_draft_to_ready,
            HumanApprovalGateMode::Automated
        );
        assert_eq!(
            policy.notifications.ready_for_review,
            Some(WorkflowNotificationRoute::Interrupt)
        );
    }

    #[test]
    fn inbox_intent_deserializes_missing_route_as_interrupt() {
        let inbox: WorkflowAutomationInboxIntent = serde_json::from_value(json!({
            "kind": "NeedsApproval",
            "title": "Approval needed"
        }))
        .expect("legacy inbox payload should deserialize");

        assert_eq!(inbox.route, WorkflowNotificationRoute::Interrupt);
    }
}
