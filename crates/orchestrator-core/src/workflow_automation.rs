use serde::{Deserialize, Serialize};

use crate::status::{InboxItemKind, WorkflowState};
use crate::workflow::{
    validate_workflow_transition, WorkflowGuardContext, WorkflowTransitionError,
    WorkflowTransitionReason,
};

pub const READY_FOR_REVIEW_INBOX_TITLE: &str =
    "Ready for review: tests passed and draft PR is available.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAutomationPolicy {
    pub auto_progress_to_awaiting_review: bool,
    pub emit_ready_for_review_inbox: bool,
}

impl Default for WorkflowAutomationPolicy {
    fn default() -> Self {
        Self {
            auto_progress_to_awaiting_review: true,
            emit_ready_for_review_inbox: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowAutomationStep {
    BeginImplementation,
    RunVerification,
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
            WorkflowState::Testing,
            WorkflowTransitionReason::TestsStarted,
        ) => {
            plan.steps.push(WorkflowAutomationStep::RunVerification);
        }
        (
            WorkflowState::Implementing | WorkflowState::Testing,
            WorkflowState::PRDrafted,
            WorkflowTransitionReason::DraftPullRequestCreated,
        ) => {
            plan.steps
                .push(WorkflowAutomationStep::CreateDraftPullRequest);

            if matches!(from, WorkflowState::Testing) {
                append_review_readiness_actions(&mut plan, guards, policy)?;
            }
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

    if policy.auto_progress_to_awaiting_review {
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
    }

    if policy.emit_ready_for_review_inbox {
        plan.inbox_items.push(WorkflowAutomationInboxIntent {
            kind: InboxItemKind::ReadyForReview,
            title: READY_FOR_REVIEW_INBOX_TITLE.to_owned(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn implementing_to_testing_triggers_run_verification_step() {
        let plan = plan_workflow_automation(
            &WorkflowState::Implementing,
            &WorkflowState::Testing,
            &WorkflowTransitionReason::TestsStarted,
            &WorkflowGuardContext {
                has_code_changes: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should be valid");

        assert_eq!(plan.steps, vec![WorkflowAutomationStep::RunVerification]);
        assert!(plan.follow_up_transitions.is_empty());
        assert!(plan.inbox_items.is_empty());
    }

    #[test]
    fn pr_drafted_with_passing_tests_progresses_to_review_readiness() {
        let plan = plan_workflow_automation(
            &WorkflowState::Testing,
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
        assert_eq!(plan.inbox_items[0].kind, InboxItemKind::ReadyForReview);
        assert_eq!(plan.inbox_items[0].title, READY_FOR_REVIEW_INBOX_TITLE);
    }

    #[test]
    fn pr_drafted_without_passing_tests_defers_review_readiness() {
        let plan = plan_workflow_automation(
            &WorkflowState::Implementing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                has_draft_pr: true,
                tests_passed: false,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should be valid");

        assert_eq!(
            plan.steps,
            vec![WorkflowAutomationStep::CreateDraftPullRequest]
        );
        assert!(plan.follow_up_transitions.is_empty());
        assert!(plan.inbox_items.is_empty());
    }

    #[test]
    fn implementing_to_pr_drafted_with_stale_tests_does_not_progress_review_readiness() {
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
            vec![WorkflowAutomationStep::CreateDraftPullRequest]
        );
        assert!(plan.follow_up_transitions.is_empty());
        assert!(plan.inbox_items.is_empty());
    }

    #[test]
    fn policy_can_defer_transition_but_keep_review_ready_notification() {
        let plan = plan_workflow_automation_with_policy(
            &WorkflowState::Testing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                tests_passed: true,
                has_draft_pr: true,
                ..WorkflowGuardContext::default()
            },
            &WorkflowAutomationPolicy {
                auto_progress_to_awaiting_review: false,
                emit_ready_for_review_inbox: true,
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
            &WorkflowState::Testing,
            &WorkflowTransitionReason::TestsStarted,
            &WorkflowGuardContext::default(),
        )
        .expect_err("invalid transition should fail");

        assert!(matches!(
            err,
            WorkflowTransitionError::InvalidTransition { .. }
        ));
    }
}
