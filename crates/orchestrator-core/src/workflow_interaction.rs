use serde::{Deserialize, Serialize};

use crate::status::WorkflowState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowInteractionLevel {
    Manual,
    Auto,
}

impl Default for WorkflowInteractionLevel {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowInteractionStateLevel {
    pub state: WorkflowState,
    pub level: WorkflowInteractionLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowInteractionProfile {
    pub name: String,
    pub levels: Vec<WorkflowInteractionStateLevel>,
}

impl WorkflowInteractionProfile {
    pub fn level_for_state(&self, state: &WorkflowState) -> WorkflowInteractionLevel {
        self.levels
            .iter()
            .find(|entry| entry.state == *state)
            .map(|entry| entry.level)
            .unwrap_or_default()
    }

    pub fn with_all_states_manual(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            levels: all_workflow_states()
                .into_iter()
                .map(|state| WorkflowInteractionStateLevel {
                    state,
                    level: WorkflowInteractionLevel::Manual,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowInteractionProfilesConfig {
    pub default_profile: String,
    pub profiles: Vec<WorkflowInteractionProfile>,
}

impl Default for WorkflowInteractionProfilesConfig {
    fn default() -> Self {
        Self {
            default_profile: "manual".to_owned(),
            profiles: vec![
                WorkflowInteractionProfile::with_all_states_manual("manual"),
                WorkflowInteractionProfile {
                    name: "auto".to_owned(),
                    levels: all_workflow_states()
                        .into_iter()
                        .map(|state| WorkflowInteractionStateLevel {
                            state,
                            level: WorkflowInteractionLevel::Auto,
                        })
                        .collect(),
                },
            ],
        }
    }
}

pub fn all_workflow_states() -> Vec<WorkflowState> {
    vec![
        WorkflowState::New,
        WorkflowState::Planning,
        WorkflowState::Implementing,
        WorkflowState::PRDrafted,
        WorkflowState::AwaitingYourReview,
        WorkflowState::ReadyForReview,
        WorkflowState::InReview,
        WorkflowState::PendingMerge,
        WorkflowState::Done,
        WorkflowState::Abandoned,
    ]
}
