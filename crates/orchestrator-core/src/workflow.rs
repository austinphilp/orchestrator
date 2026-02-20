use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::status::WorkflowState;

const NO_GUARDS: &[WorkflowGuard] = &[];
const REQUIRES_ACTIVE_SESSION: &[WorkflowGuard] = &[WorkflowGuard::ActiveSession];
const REQUIRES_PLAN_AND_ACTIVE_SESSION: &[WorkflowGuard] =
    &[WorkflowGuard::ActiveSession, WorkflowGuard::PlanReady];
const REQUIRES_CODE_CHANGES: &[WorkflowGuard] = &[WorkflowGuard::CodeChangesPresent];
const REQUIRES_PASSING_TESTS_AND_PR: &[WorkflowGuard] = &[
    WorkflowGuard::PassingTests,
    WorkflowGuard::DraftPullRequestExists,
];
const REQUIRES_PR: &[WorkflowGuard] = &[WorkflowGuard::DraftPullRequestExists];
const REQUIRES_APPROVAL_AND_PR: &[WorkflowGuard] = &[
    WorkflowGuard::UserApprovalGranted,
    WorkflowGuard::DraftPullRequestExists,
];
const REQUIRES_MERGE_COMPLETED: &[WorkflowGuard] = &[WorkflowGuard::MergeCompleted];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowTransitionReason {
    TicketAccepted,
    PlanCommitted,
    ImplementationResumed,
    TestsStarted,
    TestsFailed,
    DraftPullRequestCreated,
    AwaitingApproval,
    ApprovalGranted,
    ApprovalRejected,
    ReviewStarted,
    ReviewChangesRequested,
    MergeInitiated,
    MergeFailed,
    ReviewApprovedAndMerged,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowGuard {
    ActiveSession,
    PlanReady,
    CodeChangesPresent,
    PassingTests,
    DraftPullRequestExists,
    UserApprovalGranted,
    MergeCompleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowGuardContext {
    pub has_active_session: bool,
    pub plan_ready: bool,
    pub has_code_changes: bool,
    pub tests_passed: bool,
    pub has_draft_pr: bool,
    pub approval_granted: bool,
    pub merge_completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowTransitionError {
    #[error("workflow state '{state:?}' is terminal and cannot transition")]
    TerminalState { state: WorkflowState },
    #[error("invalid workflow transition '{from:?}' -> '{to:?}' for reason '{reason:?}'")]
    InvalidTransition {
        from: WorkflowState,
        to: WorkflowState,
        reason: WorkflowTransitionReason,
    },
    #[error(
        "workflow transition '{from:?}' -> '{to:?}' for reason '{reason:?}' failed guard '{guard:?}'"
    )]
    GuardFailed {
        from: WorkflowState,
        to: WorkflowState,
        reason: WorkflowTransitionReason,
        guard: WorkflowGuard,
    },
}

pub fn initial_workflow_state() -> WorkflowState {
    WorkflowState::New
}

pub fn apply_workflow_transition(
    from: &WorkflowState,
    to: &WorkflowState,
    reason: &WorkflowTransitionReason,
    guards: &WorkflowGuardContext,
) -> Result<WorkflowState, WorkflowTransitionError> {
    validate_workflow_transition(from, to, reason, guards)?;
    Ok(to.clone())
}

pub fn validate_workflow_transition(
    from: &WorkflowState,
    to: &WorkflowState,
    reason: &WorkflowTransitionReason,
    guards: &WorkflowGuardContext,
) -> Result<(), WorkflowTransitionError> {
    if is_terminal_state(from) {
        return Err(WorkflowTransitionError::TerminalState {
            state: from.clone(),
        });
    }

    let Some(required_guards) = transition_guards(from, to, reason) else {
        return Err(WorkflowTransitionError::InvalidTransition {
            from: from.clone(),
            to: to.clone(),
            reason: reason.clone(),
        });
    };

    for guard in required_guards {
        if !guard_satisfied(*guard, guards) {
            return Err(WorkflowTransitionError::GuardFailed {
                from: from.clone(),
                to: to.clone(),
                reason: reason.clone(),
                guard: *guard,
            });
        }
    }

    Ok(())
}

fn is_terminal_state(state: &WorkflowState) -> bool {
    matches!(state, WorkflowState::Done | WorkflowState::Abandoned)
}

fn transition_guards(
    from: &WorkflowState,
    to: &WorkflowState,
    reason: &WorkflowTransitionReason,
) -> Option<&'static [WorkflowGuard]> {
    match (from, to, reason) {
        (WorkflowState::New, WorkflowState::Planning, WorkflowTransitionReason::TicketAccepted) => {
            Some(NO_GUARDS)
        }
        (
            WorkflowState::Planning,
            WorkflowState::Implementing,
            WorkflowTransitionReason::PlanCommitted,
        ) => Some(REQUIRES_PLAN_AND_ACTIVE_SESSION),
        (
            WorkflowState::Planning,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ImplementationResumed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::Implementing,
            WorkflowState::Testing,
            WorkflowTransitionReason::TestsStarted,
        ) => Some(REQUIRES_CODE_CHANGES),
        (
            WorkflowState::Testing,
            WorkflowState::Implementing,
            WorkflowTransitionReason::TestsFailed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::Testing,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ImplementationResumed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::Testing,
            WorkflowState::PRDrafted,
            WorkflowTransitionReason::DraftPullRequestCreated,
        ) => Some(REQUIRES_PASSING_TESTS_AND_PR),
        (
            WorkflowState::Implementing,
            WorkflowState::PRDrafted,
            WorkflowTransitionReason::DraftPullRequestCreated,
        ) => Some(REQUIRES_PR),
        (
            WorkflowState::PRDrafted,
            WorkflowState::AwaitingYourReview,
            WorkflowTransitionReason::AwaitingApproval,
        ) => Some(NO_GUARDS),
        (
            WorkflowState::AwaitingYourReview,
            WorkflowState::ReadyForReview,
            WorkflowTransitionReason::ApprovalGranted,
        ) => Some(REQUIRES_APPROVAL_AND_PR),
        (
            WorkflowState::AwaitingYourReview,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ApprovalRejected,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::AwaitingYourReview,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ImplementationResumed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::PRDrafted,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ImplementationResumed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::ReadyForReview,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ImplementationResumed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::InReview,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ReviewChangesRequested,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::InReview,
            WorkflowState::Merging,
            WorkflowTransitionReason::MergeInitiated,
        ) => Some(REQUIRES_PR),
        (
            WorkflowState::Merging,
            WorkflowState::InReview,
            WorkflowTransitionReason::MergeFailed,
        ) => Some(REQUIRES_PR),
        (
            WorkflowState::InReview,
            WorkflowState::Implementing,
            WorkflowTransitionReason::ImplementationResumed,
        ) => Some(REQUIRES_ACTIVE_SESSION),
        (
            WorkflowState::ReadyForReview,
            WorkflowState::InReview,
            WorkflowTransitionReason::ReviewStarted,
        ) => Some(REQUIRES_PR),
        (
            WorkflowState::New
            | WorkflowState::Planning
            | WorkflowState::Implementing
            | WorkflowState::Testing
            | WorkflowState::PRDrafted
            | WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview,
            WorkflowState::Done,
            WorkflowTransitionReason::ReviewApprovedAndMerged,
        ) => Some(REQUIRES_MERGE_COMPLETED),
        (
            WorkflowState::Merging,
            WorkflowState::Done,
            WorkflowTransitionReason::ReviewApprovedAndMerged,
        ) => Some(REQUIRES_MERGE_COMPLETED),
        (
            WorkflowState::New
            | WorkflowState::Planning
            | WorkflowState::Implementing
            | WorkflowState::Testing
            | WorkflowState::PRDrafted
            | WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview
            | WorkflowState::Merging,
            WorkflowState::Abandoned,
            WorkflowTransitionReason::Cancelled,
        ) => Some(NO_GUARDS),
        _ => None,
    }
}

fn guard_satisfied(guard: WorkflowGuard, context: &WorkflowGuardContext) -> bool {
    match guard {
        WorkflowGuard::ActiveSession => context.has_active_session,
        WorkflowGuard::PlanReady => context.plan_ready,
        WorkflowGuard::CodeChangesPresent => context.has_code_changes,
        WorkflowGuard::PassingTests => context.tests_passed,
        WorkflowGuard::DraftPullRequestExists => context.has_draft_pr,
        WorkflowGuard::UserApprovalGranted => context.approval_granted,
        WorkflowGuard::MergeCompleted => context.merge_completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_to_planning_is_allowed_without_guards() {
        let result = validate_workflow_transition(
            &WorkflowState::New,
            &WorkflowState::Planning,
            &WorkflowTransitionReason::TicketAccepted,
            &WorkflowGuardContext::default(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn planning_to_implementing_requires_plan_and_active_session() {
        let missing_plan = validate_workflow_transition(
            &WorkflowState::Planning,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::PlanCommitted,
            &WorkflowGuardContext {
                has_active_session: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect_err("missing plan guard should fail");
        assert!(matches!(
            missing_plan,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::PlanReady,
                ..
            }
        ));

        let success = validate_workflow_transition(
            &WorkflowState::Planning,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::PlanCommitted,
            &WorkflowGuardContext {
                has_active_session: true,
                plan_ready: true,
                ..WorkflowGuardContext::default()
            },
        );
        assert!(success.is_ok());
    }

    #[test]
    fn testing_to_pr_drafted_requires_passing_tests_and_pr() {
        let missing_guards = validate_workflow_transition(
            &WorkflowState::Testing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext::default(),
        )
        .expect_err("missing guards should fail");
        assert!(matches!(
            missing_guards,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::PassingTests,
                ..
            }
        ));

        let success = validate_workflow_transition(
            &WorkflowState::Testing,
            &WorkflowState::PRDrafted,
            &WorkflowTransitionReason::DraftPullRequestCreated,
            &WorkflowGuardContext {
                tests_passed: true,
                has_draft_pr: true,
                ..WorkflowGuardContext::default()
            },
        );
        assert!(success.is_ok());
    }

    #[test]
    fn testing_to_implementing_accepts_resume_reason_with_active_session() {
        let missing_session = validate_workflow_transition(
            &WorkflowState::Testing,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::ImplementationResumed,
            &WorkflowGuardContext::default(),
        )
        .expect_err("active session guard should fail");
        assert!(matches!(
            missing_session,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::ActiveSession,
                ..
            }
        ));

        let success = validate_workflow_transition(
            &WorkflowState::Testing,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::ImplementationResumed,
            &WorkflowGuardContext {
                has_active_session: true,
                ..WorkflowGuardContext::default()
            },
        );
        assert!(success.is_ok());
    }

    #[test]
    fn awaiting_review_to_implementing_accepts_resume_reason_with_active_session() {
        let missing_session = validate_workflow_transition(
            &WorkflowState::AwaitingYourReview,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::ImplementationResumed,
            &WorkflowGuardContext::default(),
        )
        .expect_err("active session guard should fail");
        assert!(matches!(
            missing_session,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::ActiveSession,
                ..
            }
        ));

        let success = validate_workflow_transition(
            &WorkflowState::AwaitingYourReview,
            &WorkflowState::Implementing,
            &WorkflowTransitionReason::ImplementationResumed,
            &WorkflowGuardContext {
                has_active_session: true,
                ..WorkflowGuardContext::default()
            },
        );
        assert!(success.is_ok());
    }

    #[test]
    fn in_review_to_done_requires_merge_completed() {
        let failed = validate_workflow_transition(
            &WorkflowState::InReview,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ReviewApprovedAndMerged,
            &WorkflowGuardContext::default(),
        )
        .expect_err("merge guard should fail");
        assert!(matches!(
            failed,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::MergeCompleted,
                ..
            }
        ));

        let success = apply_workflow_transition(
            &WorkflowState::InReview,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ReviewApprovedAndMerged,
            &WorkflowGuardContext {
                merge_completed: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should pass");
        assert_eq!(success, WorkflowState::Done);
    }

    #[test]
    fn in_review_to_merging_requires_draft_pr() {
        let failed = validate_workflow_transition(
            &WorkflowState::InReview,
            &WorkflowState::Merging,
            &WorkflowTransitionReason::MergeInitiated,
            &WorkflowGuardContext::default(),
        )
        .expect_err("draft pr guard should fail");
        assert!(matches!(
            failed,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::DraftPullRequestExists,
                ..
            }
        ));

        let success = apply_workflow_transition(
            &WorkflowState::InReview,
            &WorkflowState::Merging,
            &WorkflowTransitionReason::MergeInitiated,
            &WorkflowGuardContext {
                has_draft_pr: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should pass");
        assert_eq!(success, WorkflowState::Merging);
    }

    #[test]
    fn merging_to_done_requires_merge_completed() {
        let failed = validate_workflow_transition(
            &WorkflowState::Merging,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ReviewApprovedAndMerged,
            &WorkflowGuardContext::default(),
        )
        .expect_err("merge completed guard should fail");
        assert!(matches!(
            failed,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::MergeCompleted,
                ..
            }
        ));

        let success = apply_workflow_transition(
            &WorkflowState::Merging,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ReviewApprovedAndMerged,
            &WorkflowGuardContext {
                merge_completed: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should pass");
        assert_eq!(success, WorkflowState::Done);
    }

    #[test]
    fn merging_to_in_review_requires_draft_pr() {
        let failed = validate_workflow_transition(
            &WorkflowState::Merging,
            &WorkflowState::InReview,
            &WorkflowTransitionReason::MergeFailed,
            &WorkflowGuardContext::default(),
        )
        .expect_err("draft pr guard should fail");
        assert!(matches!(
            failed,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::DraftPullRequestExists,
                ..
            }
        ));

        let success = apply_workflow_transition(
            &WorkflowState::Merging,
            &WorkflowState::InReview,
            &WorkflowTransitionReason::MergeFailed,
            &WorkflowGuardContext {
                has_draft_pr: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should pass");
        assert_eq!(success, WorkflowState::InReview);
    }

    #[test]
    fn planning_to_done_requires_merge_completed() {
        let failed = validate_workflow_transition(
            &WorkflowState::Planning,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ReviewApprovedAndMerged,
            &WorkflowGuardContext::default(),
        )
        .expect_err("merge guard should fail");
        assert!(matches!(
            failed,
            WorkflowTransitionError::GuardFailed {
                guard: WorkflowGuard::MergeCompleted,
                ..
            }
        ));

        let success = apply_workflow_transition(
            &WorkflowState::Planning,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ReviewApprovedAndMerged,
            &WorkflowGuardContext {
                merge_completed: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect("transition should pass");
        assert_eq!(success, WorkflowState::Done);
    }

    #[test]
    fn invalid_transition_reports_invalid_transition_error() {
        let err = validate_workflow_transition(
            &WorkflowState::ReadyForReview,
            &WorkflowState::Done,
            &WorkflowTransitionReason::ApprovalGranted,
            &WorkflowGuardContext {
                approval_granted: true,
                ..WorkflowGuardContext::default()
            },
        )
        .expect_err("skipping in-review should fail");
        assert!(matches!(
            err,
            WorkflowTransitionError::InvalidTransition { .. }
        ));
    }

    #[test]
    fn terminal_state_rejects_all_outbound_transitions() {
        let err = validate_workflow_transition(
            &WorkflowState::Done,
            &WorkflowState::Abandoned,
            &WorkflowTransitionReason::Cancelled,
            &WorkflowGuardContext::default(),
        )
        .expect_err("done is terminal");
        assert!(matches!(err, WorkflowTransitionError::TerminalState { .. }));
    }

    #[test]
    fn workflow_transition_matrix_covers_reason_and_guard_edges() {
        struct Case {
            name: &'static str,
            from: WorkflowState,
            to: WorkflowState,
            reason: WorkflowTransitionReason,
            guards: WorkflowGuardContext,
            expected_guard: Option<WorkflowGuard>,
            expect_invalid: bool,
        }

        let cases = vec![
            Case {
                name: "plan committed path requires active session and plan",
                from: WorkflowState::Planning,
                to: WorkflowState::Implementing,
                reason: WorkflowTransitionReason::PlanCommitted,
                guards: WorkflowGuardContext {
                    has_active_session: true,
                    plan_ready: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: None,
                expect_invalid: false,
            },
            Case {
                name: "plan committed missing active session fails first guard",
                from: WorkflowState::Planning,
                to: WorkflowState::Implementing,
                reason: WorkflowTransitionReason::PlanCommitted,
                guards: WorkflowGuardContext {
                    plan_ready: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: Some(WorkflowGuard::ActiveSession),
                expect_invalid: false,
            },
            Case {
                name: "plan committed missing plan fails second guard",
                from: WorkflowState::Planning,
                to: WorkflowState::Implementing,
                reason: WorkflowTransitionReason::PlanCommitted,
                guards: WorkflowGuardContext {
                    has_active_session: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: Some(WorkflowGuard::PlanReady),
                expect_invalid: false,
            },
            Case {
                name: "testing to pr drafted requires passing tests and draft pr",
                from: WorkflowState::Testing,
                to: WorkflowState::PRDrafted,
                reason: WorkflowTransitionReason::DraftPullRequestCreated,
                guards: WorkflowGuardContext {
                    tests_passed: true,
                    has_draft_pr: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: None,
                expect_invalid: false,
            },
            Case {
                name: "testing to pr drafted without tests fails first guard",
                from: WorkflowState::Testing,
                to: WorkflowState::PRDrafted,
                reason: WorkflowTransitionReason::DraftPullRequestCreated,
                guards: WorkflowGuardContext {
                    has_draft_pr: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: Some(WorkflowGuard::PassingTests),
                expect_invalid: false,
            },
            Case {
                name: "testing to pr drafted without draft pr fails second guard",
                from: WorkflowState::Testing,
                to: WorkflowState::PRDrafted,
                reason: WorkflowTransitionReason::DraftPullRequestCreated,
                guards: WorkflowGuardContext {
                    tests_passed: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: Some(WorkflowGuard::DraftPullRequestExists),
                expect_invalid: false,
            },
            Case {
                name: "review start requires draft pr",
                from: WorkflowState::ReadyForReview,
                to: WorkflowState::InReview,
                reason: WorkflowTransitionReason::ReviewStarted,
                guards: WorkflowGuardContext {
                    has_draft_pr: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: None,
                expect_invalid: false,
            },
            Case {
                name: "review start without draft pr fails guard",
                from: WorkflowState::ReadyForReview,
                to: WorkflowState::InReview,
                reason: WorkflowTransitionReason::ReviewStarted,
                guards: WorkflowGuardContext::default(),
                expected_guard: Some(WorkflowGuard::DraftPullRequestExists),
                expect_invalid: false,
            },
            Case {
                name: "merge initiation requires draft pr",
                from: WorkflowState::InReview,
                to: WorkflowState::Merging,
                reason: WorkflowTransitionReason::MergeInitiated,
                guards: WorkflowGuardContext {
                    has_draft_pr: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: None,
                expect_invalid: false,
            },
            Case {
                name: "merge failure rollback requires draft pr",
                from: WorkflowState::Merging,
                to: WorkflowState::InReview,
                reason: WorkflowTransitionReason::MergeFailed,
                guards: WorkflowGuardContext::default(),
                expected_guard: Some(WorkflowGuard::DraftPullRequestExists),
                expect_invalid: false,
            },
            Case {
                name: "valid state pair with wrong reason is rejected",
                from: WorkflowState::Testing,
                to: WorkflowState::Implementing,
                reason: WorkflowTransitionReason::PlanCommitted,
                guards: WorkflowGuardContext {
                    has_active_session: true,
                    plan_ready: true,
                    ..WorkflowGuardContext::default()
                },
                expected_guard: None,
                expect_invalid: true,
            },
        ];

        for case in cases {
            let result =
                validate_workflow_transition(&case.from, &case.to, &case.reason, &case.guards);
            match (case.expected_guard, case.expect_invalid) {
                (None, false) => assert!(
                    result.is_ok(),
                    "case '{}' expected success but failed: {result:?}",
                    case.name
                ),
                (Some(expected_guard), false) => {
                    let error = result.expect_err("case should fail with missing guard");
                    assert!(
                        matches!(
                            error,
                            WorkflowTransitionError::GuardFailed { guard, .. } if guard == expected_guard
                        ),
                        "case '{}' expected guard failure '{expected_guard:?}', got '{error:?}'",
                        case.name
                    );
                }
                (None, true) => {
                    let error = result.expect_err("case should fail with invalid transition");
                    assert!(
                        matches!(error, WorkflowTransitionError::InvalidTransition { .. }),
                        "case '{}' expected invalid transition, got '{error:?}'",
                        case.name
                    );
                }
                (Some(_), true) => panic!("invalid test matrix: conflicting expectations"),
            }
        }
    }

    #[test]
    fn cancellation_transition_is_allowed_from_all_non_terminal_states() {
        let cancellable_states = [
            WorkflowState::New,
            WorkflowState::Planning,
            WorkflowState::Implementing,
            WorkflowState::Testing,
            WorkflowState::PRDrafted,
            WorkflowState::AwaitingYourReview,
            WorkflowState::ReadyForReview,
            WorkflowState::InReview,
            WorkflowState::Merging,
        ];

        for state in cancellable_states {
            let result = validate_workflow_transition(
                &state,
                &WorkflowState::Abandoned,
                &WorkflowTransitionReason::Cancelled,
                &WorkflowGuardContext::default(),
            );
            assert!(
                result.is_ok(),
                "state '{state:?}' should allow cancellation"
            );
        }
    }

    #[test]
    fn abandoned_is_terminal_state() {
        let err = validate_workflow_transition(
            &WorkflowState::Abandoned,
            &WorkflowState::Planning,
            &WorkflowTransitionReason::TicketAccepted,
            &WorkflowGuardContext::default(),
        )
        .expect_err("abandoned is terminal");
        assert!(matches!(err, WorkflowTransitionError::TerminalState { .. }));
    }
}
