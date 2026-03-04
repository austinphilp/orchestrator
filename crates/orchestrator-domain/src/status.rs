use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowState {
    New,
    Planning,
    #[serde(alias = "Testing")]
    Implementing,
    PRDrafted,
    AwaitingYourReview,
    ReadyForReview,
    InReview,
    #[serde(alias = "Merging")]
    PendingMerge,
    Done,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerSessionStatus {
    Running,
    WaitingForUser,
    Blocked,
    Done,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxItemKind {
    NeedsDecision,
    NeedsApproval,
    Blocked,
    FYI,
    ReadyForReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InboxLane {
    DecideOrUnblock,
    Approvals,
    ReviewReady,
    FyiDigest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InboxLaneColor {
    Crimson,
    Amber,
    Emerald,
    Azure,
    Violet,
    Slate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxLaneColorPreferences {
    pub decide_or_unblock: InboxLaneColor,
    pub approvals: InboxLaneColor,
    pub review_ready: InboxLaneColor,
    pub fyi_digest: InboxLaneColor,
}

impl Default for InboxLaneColorPreferences {
    fn default() -> Self {
        Self {
            decide_or_unblock: InboxLaneColor::Amber,
            approvals: InboxLaneColor::Azure,
            review_ready: InboxLaneColor::Emerald,
            fyi_digest: InboxLaneColor::Violet,
        }
    }
}

impl InboxLaneColorPreferences {
    pub fn color_for_lane(&self, lane: InboxLane) -> InboxLaneColor {
        match lane {
            InboxLane::DecideOrUnblock => self.decide_or_unblock,
            InboxLane::Approvals => self.approvals,
            InboxLane::ReviewReady => self.review_ready,
            InboxLane::FyiDigest => self.fyi_digest,
        }
    }

    pub fn set_lane_color(&mut self, lane: InboxLane, color: InboxLaneColor) {
        match lane {
            InboxLane::DecideOrUnblock => self.decide_or_unblock = color,
            InboxLane::Approvals => self.approvals = color,
            InboxLane::ReviewReady => self.review_ready = color,
            InboxLane::FyiDigest => self.fyi_digest = color,
        }
    }

    pub fn reset_lane(&mut self, lane: InboxLane) {
        let defaults = Self::default();
        self.set_lane_color(lane, defaults.color_for_lane(lane));
    }

    pub fn reset_all(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    Diff,
    PR,
    TestRun,
    LogSnippet,
    Link,
    Export,
}
