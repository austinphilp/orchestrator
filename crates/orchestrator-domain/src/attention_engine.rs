use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::events::{OrchestrationEventPayload, StoredEventEnvelope};
use crate::identifiers::{InboxItemId, WorkItemId, WorkerSessionId};
use crate::projection::ProjectionState;
use crate::status::{InboxItemKind, WorkerSessionStatus, WorkflowState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AttentionPriorityBand {
    Urgent,
    Attention,
    Background,
}

impl AttentionPriorityBand {
    pub fn label(self) -> &'static str {
        match self {
            Self::Urgent => "Urgent",
            Self::Attention => "Attention",
            Self::Background => "Background",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Urgent => 0,
            Self::Attention => 1,
            Self::Background => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttentionBatchKind {
    DecideOrUnblock,
    Approvals,
    ReviewReady,
    FyiDigest,
}

impl AttentionBatchKind {
    pub const ORDERED: [AttentionBatchKind; 4] = [
        AttentionBatchKind::DecideOrUnblock,
        AttentionBatchKind::Approvals,
        AttentionBatchKind::ReviewReady,
        AttentionBatchKind::FyiDigest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DecideOrUnblock => "Decide / Unblock",
            Self::Approvals => "Approvals",
            Self::ReviewReady => "PR Reviews",
            Self::FyiDigest => "FYI Digest",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::DecideOrUnblock => "󰞋",
            Self::Approvals => "󰄬",
            Self::ReviewReady => "󰳴",
            Self::FyiDigest => "󰋼",
        }
    }

    pub fn heading_label(self) -> String {
        format!("{} {}", self.glyph(), self.label())
    }

    pub fn hotkey(self) -> char {
        match self {
            Self::DecideOrUnblock => '1',
            Self::Approvals => '2',
            Self::ReviewReady => '3',
            Self::FyiDigest => '4',
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::DecideOrUnblock => 0,
            Self::Approvals => 1,
            Self::ReviewReady => 2,
            Self::FyiDigest => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionScoreBreakdown {
    pub base: i32,
    pub age_bonus: i32,
    pub idle_risk: i32,
    pub idle_risk_bonus: i32,
    pub workflow_gate_bonus: i32,
    pub user_pinned_bonus: i32,
    pub resolved_penalty: i32,
    pub minutes_since_created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionInboxItem {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: crate::identifiers::WorkItemId,
    pub kind: InboxItemKind,
    pub title: String,
    pub resolved: bool,
    pub workflow_state: Option<WorkflowState>,
    pub session_id: Option<WorkerSessionId>,
    pub session_status: Option<WorkerSessionStatus>,
    pub priority_score: i32,
    pub priority_band: AttentionPriorityBand,
    pub batch_kind: AttentionBatchKind,
    pub score_breakdown: AttentionScoreBreakdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionBatchSurface {
    pub kind: AttentionBatchKind,
    pub unresolved_count: usize,
    pub total_count: usize,
    pub first_unresolved_index: Option<usize>,
    pub first_any_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttentionInboxSnapshot {
    pub items: Vec<AttentionInboxItem>,
    pub batch_surfaces: Vec<AttentionBatchSurface>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttentionEngineConfig {
    pub base_needs_decision: i32,
    pub base_blocked: i32,
    pub base_needs_approval: i32,
    pub base_ready_for_review: i32,
    pub base_fyi: i32,
    pub age_weight: i32,
    pub idle_risk_weight: i32,
    pub workflow_gate_bonus: i32,
    pub user_pinned_bonus: i32,
    pub resolved_penalty: i32,
    pub urgent_threshold: i32,
    pub attention_threshold: i32,
    pub waiting_for_input_risk: i32,
    pub blocked_risk: i32,
    pub idle_pause_threshold_minutes: u64,
    pub idle_pause_step_minutes: u64,
    pub idle_pause_step_risk: i32,
    pub blocked_no_action_grace_minutes: u64,
    pub blocked_no_action_step_minutes: u64,
    pub blocked_no_action_step_risk: i32,
}

impl Default for AttentionEngineConfig {
    fn default() -> Self {
        Self {
            base_needs_decision: 100,
            base_blocked: 95,
            base_needs_approval: 75,
            base_ready_for_review: 70,
            base_fyi: 30,
            age_weight: 1,
            idle_risk_weight: 3,
            workflow_gate_bonus: 10,
            user_pinned_bonus: 25,
            resolved_penalty: 500,
            urgent_threshold: 95,
            attention_threshold: 60,
            waiting_for_input_risk: 8,
            blocked_risk: 7,
            idle_pause_threshold_minutes: 15,
            idle_pause_step_minutes: 10,
            idle_pause_step_risk: 1,
            blocked_no_action_grace_minutes: 5,
            blocked_no_action_step_minutes: 10,
            blocked_no_action_step_risk: 2,
        }
    }
}

impl AttentionEngineConfig {
    fn base_score(&self, kind: &InboxItemKind) -> i32 {
        match kind {
            InboxItemKind::NeedsDecision => self.base_needs_decision,
            InboxItemKind::Blocked => self.base_blocked,
            InboxItemKind::NeedsApproval => self.base_needs_approval,
            InboxItemKind::ReadyForReview => self.base_ready_for_review,
            InboxItemKind::FYI => self.base_fyi,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct WorkerIdleTimeline {
    last_progress_minute: Option<u64>,
    waiting_for_input_since: Option<u64>,
    blocked_since: Option<u64>,
    last_user_action_minute: Option<u64>,
}

const RESOLVED_AUTO_DISMISS_AFTER_SECS: u64 = 60;

pub fn attention_inbox_snapshot(
    state: &ProjectionState,
    config: &AttentionEngineConfig,
    pinned_inbox_items: &[InboxItemId],
) -> AttentionInboxSnapshot {
    let pinned = pinned_inbox_items
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect::<HashSet<_>>();
    let (created_minutes, resolved_seconds, idle_timeline, as_of_minute, as_of_second) =
        collect_timeline(state);

    let mut items = state
        .inbox_items
        .values()
        .filter_map(|item| {
            if item.resolved
                && is_resolved_item_auto_dismissed(&item.id, &resolved_seconds, as_of_second)
            {
                return None;
            }

            let work_item = state.work_items.get(&item.work_item_id);
            let session_id = work_item.and_then(|entry| entry.session_id.clone());
            let session_status = session_id
                .as_ref()
                .and_then(|id| state.sessions.get(id))
                .and_then(|session| session.status.clone());
            let workflow_state = work_item.and_then(|entry| entry.workflow_state.clone());

            if session_has_ended(session_status.as_ref()) {
                return None;
            }

            let base = config.base_score(&item.kind);
            let minutes_since_created = created_minutes
                .get(&item.id)
                .map(|created| minutes_since(*created, as_of_minute))
                .unwrap_or(0);
            let age_bonus = config
                .age_weight
                .saturating_mul(minutes_since_created.min(i32::MAX as u64) as i32);
            let idle_risk = worker_idle_risk(
                idle_timeline
                    .as_ref()
                    .and_then(|timeline| session_id.as_ref().and_then(|id| timeline.get(id))),
                session_status.as_ref(),
                as_of_minute,
                config,
            );
            let idle_risk_bonus = config.idle_risk_weight.saturating_mul(idle_risk);
            let workflow_gate_bonus = workflow_gate_bonus(workflow_state.as_ref(), config);
            let user_pinned_bonus = if pinned.contains(item.id.as_str()) {
                config.user_pinned_bonus
            } else {
                0
            };
            let resolved_penalty = if item.resolved {
                config.resolved_penalty
            } else {
                0
            };

            let mut score = base;
            score = score.saturating_add(age_bonus);
            score = score.saturating_add(idle_risk_bonus);
            score = score.saturating_add(workflow_gate_bonus);
            score = score.saturating_add(user_pinned_bonus);
            score = score.saturating_sub(resolved_penalty);

            let priority_band = priority_band(score, item.resolved, config);
            Some(AttentionInboxItem {
                inbox_item_id: item.id.clone(),
                work_item_id: item.work_item_id.clone(),
                kind: item.kind.clone(),
                title: item.title.clone(),
                resolved: item.resolved,
                workflow_state,
                session_id,
                session_status,
                priority_score: score,
                priority_band,
                batch_kind: batch_kind(&item.kind),
                score_breakdown: AttentionScoreBreakdown {
                    base,
                    age_bonus,
                    idle_risk,
                    idle_risk_bonus,
                    workflow_gate_bonus,
                    user_pinned_bonus,
                    resolved_penalty,
                    minutes_since_created,
                },
            })
        })
        .collect::<Vec<_>>();

    items.sort_by(|a, b| {
        a.batch_kind
            .rank()
            .cmp(&b.batch_kind.rank())
            .then_with(|| a.resolved.cmp(&b.resolved))
            .then_with(|| b.priority_score.cmp(&a.priority_score))
            .then_with(|| a.inbox_item_id.as_str().cmp(b.inbox_item_id.as_str()))
    });

    AttentionInboxSnapshot {
        batch_surfaces: build_batch_surfaces(&items),
        items,
    }
}

fn is_resolved_item_auto_dismissed(
    inbox_item_id: &InboxItemId,
    resolved_seconds: &HashMap<InboxItemId, u64>,
    as_of_second: u64,
) -> bool {
    let Some(resolved_at) = resolved_seconds.get(inbox_item_id).copied() else {
        return false;
    };
    as_of_second.saturating_sub(resolved_at) >= RESOLVED_AUTO_DISMISS_AFTER_SECS
}

fn session_has_ended(status: Option<&WorkerSessionStatus>) -> bool {
    matches!(
        status,
        Some(WorkerSessionStatus::Done) | Some(WorkerSessionStatus::Crashed)
    )
}

fn priority_band(
    priority_score: i32,
    resolved: bool,
    config: &AttentionEngineConfig,
) -> AttentionPriorityBand {
    if resolved {
        return AttentionPriorityBand::Background;
    }

    if priority_score >= config.urgent_threshold {
        AttentionPriorityBand::Urgent
    } else if priority_score >= config.attention_threshold {
        AttentionPriorityBand::Attention
    } else {
        AttentionPriorityBand::Background
    }
}

fn batch_kind(kind: &InboxItemKind) -> AttentionBatchKind {
    match kind {
        InboxItemKind::NeedsDecision | InboxItemKind::Blocked => {
            AttentionBatchKind::DecideOrUnblock
        }
        InboxItemKind::NeedsApproval => AttentionBatchKind::Approvals,
        InboxItemKind::ReadyForReview => AttentionBatchKind::ReviewReady,
        InboxItemKind::FYI => AttentionBatchKind::FyiDigest,
    }
}

fn build_batch_surfaces(rows: &[AttentionInboxItem]) -> Vec<AttentionBatchSurface> {
    let mut surfaces = AttentionBatchKind::ORDERED
        .iter()
        .map(|kind| AttentionBatchSurface {
            kind: *kind,
            unresolved_count: 0,
            total_count: 0,
            first_unresolved_index: None,
            first_any_index: None,
        })
        .collect::<Vec<_>>();

    let mut kind_to_index = HashMap::new();
    for (index, kind) in AttentionBatchKind::ORDERED.iter().enumerate() {
        kind_to_index.insert(*kind, index);
    }

    for (row_index, row) in rows.iter().enumerate() {
        let Some(surface_index) = kind_to_index.get(&row.batch_kind).copied() else {
            continue;
        };
        let surface = &mut surfaces[surface_index];
        surface.total_count = surface.total_count.saturating_add(1);
        if surface.first_any_index.is_none() {
            surface.first_any_index = Some(row_index);
        }
        if !row.resolved {
            surface.unresolved_count = surface.unresolved_count.saturating_add(1);
            if surface.first_unresolved_index.is_none() {
                surface.first_unresolved_index = Some(row_index);
            }
        }
    }

    surfaces
}

fn workflow_gate_bonus(
    workflow_state: Option<&WorkflowState>,
    config: &AttentionEngineConfig,
) -> i32 {
    if matches!(
        workflow_state,
        Some(WorkflowState::AwaitingYourReview | WorkflowState::ReadyForReview)
    ) {
        config.workflow_gate_bonus
    } else {
        0
    }
}

fn worker_idle_risk(
    timeline: Option<&WorkerIdleTimeline>,
    session_status: Option<&WorkerSessionStatus>,
    as_of_minute: u64,
    config: &AttentionEngineConfig,
) -> i32 {
    let mut risk: i32 = 0;

    if matches!(session_status, Some(WorkerSessionStatus::WaitingForUser)) {
        risk = risk.saturating_add(config.waiting_for_input_risk);
        if let Some(waiting_since) = timeline.and_then(|timeline| timeline.waiting_for_input_since)
        {
            let waiting_minutes = minutes_since(waiting_since, as_of_minute);
            risk = risk.saturating_add(ramped_risk(
                waiting_minutes,
                config.idle_pause_threshold_minutes,
                config.idle_pause_step_minutes,
                config.idle_pause_step_risk,
            ));
        }
    }

    if let Some(last_progress_minute) = timeline.and_then(|timeline| timeline.last_progress_minute)
    {
        let paused_minutes = minutes_since(last_progress_minute, as_of_minute);
        risk = risk.saturating_add(ramped_risk(
            paused_minutes,
            config.idle_pause_threshold_minutes,
            config.idle_pause_step_minutes,
            config.idle_pause_step_risk,
        ));
    }

    if matches!(session_status, Some(WorkerSessionStatus::Blocked)) {
        risk = risk.saturating_add(config.blocked_risk);
        if let Some(blocked_since) = timeline.and_then(|timeline| timeline.blocked_since) {
            let blocked_minutes = minutes_since(blocked_since, as_of_minute);
            let has_action_after_block = timeline
                .and_then(|timeline| timeline.last_user_action_minute)
                .map(|action| action >= blocked_since)
                .unwrap_or(false);

            if !has_action_after_block {
                risk = risk.saturating_add(ramped_risk(
                    blocked_minutes,
                    config.blocked_no_action_grace_minutes,
                    config.blocked_no_action_step_minutes,
                    config.blocked_no_action_step_risk,
                ));
            }
        }
    }

    risk
}

fn ramped_risk(
    elapsed_minutes: u64,
    threshold_minutes: u64,
    step_minutes: u64,
    step_risk: i32,
) -> i32 {
    if elapsed_minutes <= threshold_minutes {
        return 0;
    }
    if step_risk <= 0 {
        return 0;
    }

    let step = step_minutes.max(1);
    let steps = ((elapsed_minutes - threshold_minutes - 1) / step) + 1;
    step_risk.saturating_mul(steps.min(i32::MAX as u64) as i32)
}

fn minutes_since(from_minute: u64, to_minute: u64) -> u64 {
    to_minute.saturating_sub(from_minute)
}

fn collect_timeline(
    state: &ProjectionState,
) -> (
    HashMap<InboxItemId, u64>,
    HashMap<InboxItemId, u64>,
    Option<HashMap<WorkerSessionId, WorkerIdleTimeline>>,
    u64,
    u64,
) {
    let mut events_ordered = state.events.iter().collect::<Vec<_>>();
    if !events_are_sequence_sorted(state.events.as_slice()) {
        events_ordered.sort_by(|a, b| {
            a.sequence
                .cmp(&b.sequence)
                .then_with(|| a.event_id.cmp(&b.event_id))
        });
    }

    let mut work_item_sessions = state
        .work_items
        .iter()
        .filter_map(|(work_item_id, work_item)| {
            work_item
                .session_id
                .as_ref()
                .map(|session_id| (work_item_id.clone(), session_id.clone()))
        })
        .collect::<HashMap<_, _>>();
    let mut session_work_items = work_item_sessions
        .iter()
        .map(|(work_item_id, session_id)| (session_id.clone(), work_item_id.clone()))
        .collect::<HashMap<_, _>>();

    let mut created_minutes = HashMap::new();
    let mut resolved_seconds = HashMap::new();
    let mut timelines = HashMap::<WorkerSessionId, WorkerIdleTimeline>::new();
    let mut as_of_minute = 0;
    let mut as_of_second = 0;
    let mut saw_timestamp = false;

    for event in events_ordered {
        let Some(second) = parse_event_unix_seconds(event.occurred_at.as_str()) else {
            continue;
        };
        let minute = second / 60;
        let event_work_item_id = event.work_item_id.as_ref();
        let event_session_id = event.session_id.as_ref();
        saw_timestamp = true;
        as_of_minute = as_of_minute.max(minute);
        as_of_second = as_of_second.max(second);

        match &event.payload {
            OrchestrationEventPayload::InboxItemCreated(payload) => {
                created_minutes.insert(payload.inbox_item_id.clone(), minute);
                resolved_seconds.remove(&payload.inbox_item_id);
            }
            OrchestrationEventPayload::SessionSpawned(payload) => {
                work_item_sessions.insert(payload.work_item_id.clone(), payload.session_id.clone());
                session_work_items.insert(payload.session_id.clone(), payload.work_item_id.clone());
                let timeline = timelines.entry(payload.session_id.clone()).or_default();
                mark_progress(timeline, minute);
                clear_attention_holds(timeline);
            }
            OrchestrationEventPayload::SessionCheckpoint(payload) => {
                let timeline = timelines.entry(payload.session_id.clone()).or_default();
                mark_progress(timeline, minute);
                clear_attention_holds(timeline);
            }
            OrchestrationEventPayload::SessionCompleted(payload) => {
                let timeline = timelines.entry(payload.session_id.clone()).or_default();
                mark_progress(timeline, minute);
                clear_attention_holds(timeline);
                if let Some(work_item_id) = session_work_items.get(&payload.session_id) {
                    if let Some(work_item) = state.work_items.get(work_item_id) {
                        for inbox_item_id in &work_item.inbox_items {
                            resolved_seconds.insert(inbox_item_id.clone(), second);
                        }
                    }
                }
            }
            OrchestrationEventPayload::SessionNeedsInput(payload) => {
                let timeline = timelines.entry(payload.session_id.clone()).or_default();
                timeline.waiting_for_input_since = Some(minute);
                timeline.blocked_since = None;
            }
            OrchestrationEventPayload::SessionBlocked(payload) => {
                let timeline = timelines.entry(payload.session_id.clone()).or_default();
                timeline.blocked_since = Some(minute);
                timeline.waiting_for_input_since = None;
            }
            OrchestrationEventPayload::ArtifactCreated(payload) => {
                if let Some(session_id) = resolve_session_id(
                    event_session_id,
                    Some(&payload.work_item_id),
                    &work_item_sessions,
                ) {
                    mark_progress(timelines.entry(session_id.clone()).or_default(), minute);
                }
            }
            OrchestrationEventPayload::WorkflowTransition(payload) => {
                if let Some(session_id) = resolve_session_id(
                    event_session_id,
                    Some(&payload.work_item_id),
                    &work_item_sessions,
                ) {
                    let timeline = timelines.entry(session_id.clone()).or_default();
                    mark_progress(timeline, minute);
                    clear_attention_holds(timeline);
                }
            }
            OrchestrationEventPayload::InboxItemResolved(payload) => {
                resolved_seconds.insert(payload.inbox_item_id.clone(), second);
                if let Some(session_id) = resolve_session_id(
                    event_session_id,
                    Some(&payload.work_item_id),
                    &work_item_sessions,
                ) {
                    timelines
                        .entry(session_id.clone())
                        .or_default()
                        .last_user_action_minute = Some(minute);
                }
            }
            OrchestrationEventPayload::UserResponded(payload) => {
                if let Some(session_id) = resolve_session_id(
                    payload.session_id.as_ref().or(event_session_id),
                    payload.work_item_id.as_ref().or(event_work_item_id),
                    &work_item_sessions,
                ) {
                    let timeline = timelines.entry(session_id.clone()).or_default();
                    timeline.last_user_action_minute = Some(minute);
                    timeline.waiting_for_input_since = None;
                }
            }
            OrchestrationEventPayload::SessionCrashed(payload) => {
                if let Some(work_item_id) = session_work_items.get(&payload.session_id) {
                    if let Some(work_item) = state.work_items.get(work_item_id) {
                        for inbox_item_id in &work_item.inbox_items {
                            resolved_seconds.insert(inbox_item_id.clone(), second);
                        }
                    }
                }
            }
            OrchestrationEventPayload::TicketSynced(_)
            | OrchestrationEventPayload::TicketDetailsSynced(_)
            | OrchestrationEventPayload::WorkItemCreated(_)
            | OrchestrationEventPayload::WorkItemProfileOverrideSet(_)
            | OrchestrationEventPayload::WorkItemProfileOverrideCleared(_)
            | OrchestrationEventPayload::WorktreeCreated(_)
            | OrchestrationEventPayload::SupervisorQueryStarted(_)
            | OrchestrationEventPayload::SupervisorQueryChunk(_)
            | OrchestrationEventPayload::SupervisorQueryCancelled(_)
            | OrchestrationEventPayload::SupervisorQueryFinished(_) => {}
        }
    }

    let timeline = saw_timestamp.then_some(timelines);
    (
        created_minutes,
        resolved_seconds,
        timeline,
        as_of_minute,
        as_of_second,
    )
}

fn events_are_sequence_sorted(events: &[StoredEventEnvelope]) -> bool {
    events
        .windows(2)
        .all(|pair| match (pair.first(), pair.get(1)) {
            (Some(left), Some(right)) => {
                left.sequence < right.sequence
                    || (left.sequence == right.sequence && left.event_id <= right.event_id)
            }
            _ => true,
        })
}

fn mark_progress(timeline: &mut WorkerIdleTimeline, minute: u64) {
    timeline.last_progress_minute = Some(minute);
}

fn clear_attention_holds(timeline: &mut WorkerIdleTimeline) {
    timeline.waiting_for_input_since = None;
    timeline.blocked_since = None;
}

fn resolve_session_id<'a>(
    session_id: Option<&'a WorkerSessionId>,
    work_item_id: Option<&WorkItemId>,
    work_item_sessions: &'a HashMap<WorkItemId, WorkerSessionId>,
) -> Option<&'a WorkerSessionId> {
    session_id.or_else(|| work_item_id.and_then(|id| work_item_sessions.get(id)))
}

fn parse_event_unix_seconds(value: &str) -> Option<u64> {
    if let Ok(timestamp) = OffsetDateTime::parse(value, &Rfc3339) {
        let seconds = timestamp.unix_timestamp();
        if seconds >= 0 {
            return Some(seconds as u64);
        }
    }

    let trimmed = value.trim().strip_suffix('Z').unwrap_or(value.trim());
    let seconds = trimmed
        .split_once('.')
        .map(|(left, _)| left)
        .unwrap_or(trimmed);
    seconds.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use crate::{
        InboxItemCreatedPayload, InboxItemProjection, NewEventEnvelope, OrchestrationEventPayload,
        ProjectionState, SessionNeedsInputPayload, SessionProjection, SessionSpawnedPayload,
        UserRespondedPayload, WorkItemProjection, WorkflowState,
    };

    use super::*;

    fn base_projection() -> ProjectionState {
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_id.clone()],
                artifacts: vec![],
                profile_override: None,
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id,
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::Running),
                latest_checkpoint: None,
            },
        );
        projection.inbox_items.insert(
            inbox_id.clone(),
            InboxItemProjection {
                id: inbox_id,
                work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Review PR".to_owned(),
                resolved: false,
            },
        );
        projection
    }

    fn event(
        sequence: u64,
        occurred_at: &str,
        work_item_id: Option<&crate::WorkItemId>,
        session_id: Option<&WorkerSessionId>,
        payload: OrchestrationEventPayload,
    ) -> crate::StoredEventEnvelope {
        crate::StoredEventEnvelope::from((
            sequence,
            NewEventEnvelope {
                event_id: format!("evt-{sequence}"),
                occurred_at: occurred_at.to_owned(),
                work_item_id: work_item_id.cloned(),
                session_id: session_id.cloned(),
                payload,
                schema_version: 1,
            },
        ))
    }

    #[test]
    fn score_is_deterministic_and_tracks_age() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let inbox_id = InboxItemId::new("inbox-1");

        projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:30:00Z",
                None,
                None,
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: None,
                    work_item_id: None,
                    message: "noop".to_owned(),
                }),
            ),
        ];

        let config = AttentionEngineConfig::default();
        let first = attention_inbox_snapshot(&projection, &config, &[]);
        let second = attention_inbox_snapshot(&projection, &config, &[]);
        assert_eq!(first, second);
        assert_eq!(first.items[0].score_breakdown.minutes_since_created, 30);
        assert_eq!(first.items[0].score_breakdown.age_bonus, 30);
    }

    #[test]
    fn waiting_status_and_pause_duration_raise_idle_risk() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");

        projection
            .sessions
            .get_mut(&session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::WaitingForUser);

        projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: session_id.clone(),
                    work_item_id: work_item_id.clone(),
                    model: "gpt-5".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:10:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                    session_id: session_id.clone(),
                    prompt: "pick".to_owned(),
                    prompt_id: None,
                    options: vec![],
                    default_option: None,
                }),
            ),
            event(
                3,
                "2026-02-16T10:50:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        let risk = snapshot.items[0].score_breakdown.idle_risk;
        assert!(risk > AttentionEngineConfig::default().waiting_for_input_risk);
    }

    #[test]
    fn blocked_without_user_action_has_higher_idle_risk() {
        let mut no_action_projection = base_projection();
        let mut with_action_projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");

        no_action_projection
            .sessions
            .get_mut(&session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::Blocked);
        with_action_projection
            .sessions
            .get_mut(&session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::Blocked);

        no_action_projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionBlocked(crate::SessionBlockedPayload {
                    session_id: session_id.clone(),
                    reason: "tests failing".to_owned(),
                    hint: None,
                    log_ref: None,
                }),
            ),
            event(
                2,
                "2026-02-16T10:40:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id.clone(),
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
        ];

        with_action_projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionBlocked(crate::SessionBlockedPayload {
                    session_id: session_id.clone(),
                    reason: "tests failing".to_owned(),
                    hint: None,
                    log_ref: None,
                }),
            ),
            event(
                2,
                "2026-02-16T10:40:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: Some(session_id.clone()),
                    work_item_id: Some(work_item_id.clone()),
                    message: "rerun tests".to_owned(),
                }),
            ),
            event(
                3,
                "2026-02-16T10:41:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
        ];

        let config = AttentionEngineConfig::default();
        let no_action = attention_inbox_snapshot(&no_action_projection, &config, &[]);
        let with_action = attention_inbox_snapshot(&with_action_projection, &config, &[]);
        assert!(
            no_action.items[0].score_breakdown.idle_risk
                > with_action.items[0].score_breakdown.idle_risk
        );
    }

    #[test]
    fn waiting_idle_risk_drops_after_user_response() {
        let mut no_action_projection = base_projection();
        let mut with_action_projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");

        no_action_projection
            .sessions
            .get_mut(&session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::WaitingForUser);
        with_action_projection
            .sessions
            .get_mut(&session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::WaitingForUser);

        no_action_projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: session_id.clone(),
                    work_item_id: work_item_id.clone(),
                    model: "gpt-5".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:05:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                    session_id: session_id.clone(),
                    prompt: "pick".to_owned(),
                    prompt_id: None,
                    options: vec![],
                    default_option: None,
                }),
            ),
            event(
                3,
                "2026-02-16T10:40:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id.clone(),
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
        ];

        with_action_projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: session_id.clone(),
                    work_item_id: work_item_id.clone(),
                    model: "gpt-5".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:05:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                    session_id: session_id.clone(),
                    prompt: "pick".to_owned(),
                    prompt_id: None,
                    options: vec![],
                    default_option: None,
                }),
            ),
            event(
                3,
                "2026-02-16T10:30:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: Some(session_id.clone()),
                    work_item_id: Some(work_item_id.clone()),
                    message: "A".to_owned(),
                }),
            ),
            event(
                4,
                "2026-02-16T10:40:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
        ];

        let config = AttentionEngineConfig::default();
        let no_action = attention_inbox_snapshot(&no_action_projection, &config, &[]);
        let with_action = attention_inbox_snapshot(&with_action_projection, &config, &[]);
        assert!(
            no_action.items[0].score_breakdown.idle_risk
                > with_action.items[0].score_breakdown.idle_risk
        );
    }

    #[test]
    fn timeline_prefers_event_session_for_progress_markers() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let active_session_id = WorkerSessionId::new("sess-1");
        let stray_session_id = WorkerSessionId::new("sess-old");
        let inbox_id = InboxItemId::new("inbox-1");

        projection
            .sessions
            .get_mut(&active_session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::WaitingForUser);

        projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&active_session_id),
                OrchestrationEventPayload::SessionSpawned(SessionSpawnedPayload {
                    session_id: active_session_id.clone(),
                    work_item_id: work_item_id.clone(),
                    model: "gpt-5".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:05:00Z",
                Some(&work_item_id),
                Some(&active_session_id),
                OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                    session_id: active_session_id.clone(),
                    prompt: "pick".to_owned(),
                    prompt_id: None,
                    options: vec![],
                    default_option: None,
                }),
            ),
            event(
                3,
                "2026-02-16T10:39:00Z",
                Some(&work_item_id),
                Some(&stray_session_id),
                OrchestrationEventPayload::ArtifactCreated(crate::ArtifactCreatedPayload {
                    artifact_id: crate::ArtifactId::new("art-1"),
                    work_item_id: work_item_id.clone(),
                    kind: crate::ArtifactKind::LogSnippet,
                    label: "old session log".to_owned(),
                    uri: "artifact://art-1".to_owned(),
                }),
            ),
            event(
                4,
                "2026-02-16T10:40:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert_eq!(snapshot.items[0].score_breakdown.idle_risk, 13);
    }

    #[test]
    fn pinned_items_receive_bonus() {
        let mut projection = base_projection();
        let pinned_work_item = crate::WorkItemId::new("wi-2");
        let pinned_item = InboxItemId::new("inbox-2");
        let session = WorkerSessionId::new("sess-2");

        projection.work_items.insert(
            pinned_work_item.clone(),
            WorkItemProjection {
                id: pinned_work_item.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: Some(session.clone()),
                worktree_id: None,
                inbox_items: vec![pinned_item.clone()],
                artifacts: vec![],
                profile_override: None,
            },
        );
        projection.sessions.insert(
            session.clone(),
            SessionProjection {
                id: session,
                work_item_id: Some(pinned_work_item.clone()),
                status: Some(WorkerSessionStatus::Running),
                latest_checkpoint: None,
            },
        );
        projection.inbox_items.insert(
            pinned_item.clone(),
            InboxItemProjection {
                id: pinned_item.clone(),
                work_item_id: pinned_work_item,
                kind: InboxItemKind::NeedsApproval,
                title: "Pinned".to_owned(),
                resolved: false,
            },
        );

        let config = AttentionEngineConfig::default();
        let unpinned = attention_inbox_snapshot(&projection, &config, &[]);
        let pinned =
            attention_inbox_snapshot(&projection, &config, std::slice::from_ref(&pinned_item));

        let score_unpinned = unpinned
            .items
            .iter()
            .find(|item| item.inbox_item_id == pinned_item)
            .expect("unpinned row")
            .priority_score;
        let score_pinned = pinned
            .items
            .iter()
            .find(|item| item.inbox_item_id == pinned_item)
            .expect("pinned row")
            .priority_score;

        assert_eq!(score_pinned - score_unpinned, config.user_pinned_bonus);
    }

    #[test]
    fn batch_surfaces_are_built_by_lane() {
        let mut projection = ProjectionState::default();
        let rows = vec![
            (
                "wi-decision",
                "inbox-decision",
                InboxItemKind::NeedsDecision,
                false,
            ),
            (
                "wi-approval",
                "inbox-approval",
                InboxItemKind::NeedsApproval,
                true,
            ),
            (
                "wi-review",
                "inbox-review",
                InboxItemKind::ReadyForReview,
                false,
            ),
            ("wi-fyi", "inbox-fyi", InboxItemKind::FYI, false),
        ];

        for (work_item_raw, inbox_item_raw, kind, resolved) in rows {
            let work_item_id = crate::WorkItemId::new(work_item_raw);
            let session_id = WorkerSessionId::new(format!("sess-{work_item_raw}"));
            let inbox_item_id = InboxItemId::new(inbox_item_raw);
            projection.work_items.insert(
                work_item_id.clone(),
                WorkItemProjection {
                    id: work_item_id.clone(),
                    ticket_id: None,
                    project_id: None,
                    workflow_state: Some(WorkflowState::Implementing),
                    session_id: Some(session_id.clone()),
                    worktree_id: None,
                    inbox_items: vec![inbox_item_id.clone()],
                    artifacts: vec![],
                    profile_override: None,
                },
            );
            projection.sessions.insert(
                session_id.clone(),
                SessionProjection {
                    id: session_id,
                    work_item_id: Some(work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
            projection.inbox_items.insert(
                inbox_item_id.clone(),
                InboxItemProjection {
                    id: inbox_item_id,
                    work_item_id,
                    kind,
                    title: "row".to_owned(),
                    resolved,
                },
            );
        }

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert_eq!(snapshot.batch_surfaces.len(), 4);
        assert_eq!(snapshot.batch_surfaces[0].unresolved_count, 1);
        assert_eq!(snapshot.batch_surfaces[0].total_count, 1);
        assert_eq!(snapshot.batch_surfaces[1].unresolved_count, 0);
        assert_eq!(snapshot.batch_surfaces[1].total_count, 1);
        assert!(snapshot.batch_surfaces[1].first_unresolved_index.is_none());
        assert!(snapshot.batch_surfaces[1].first_any_index.is_some());
        assert_eq!(snapshot.batch_surfaces[2].unresolved_count, 1);
        assert_eq!(snapshot.batch_surfaces[3].unresolved_count, 1);
    }

    #[test]
    fn inbox_items_are_sorted_lane_first_and_unresolved_first_within_lane() {
        let mut projection = ProjectionState::default();
        let rows = vec![
            (
                "wi-fyi-resolved",
                "inbox-fyi-resolved",
                InboxItemKind::FYI,
                true,
            ),
            (
                "wi-approval-unresolved",
                "inbox-approval-unresolved",
                InboxItemKind::NeedsApproval,
                false,
            ),
            (
                "wi-decision-unresolved",
                "inbox-decision-unresolved",
                InboxItemKind::NeedsDecision,
                false,
            ),
            (
                "wi-decision-resolved",
                "inbox-decision-resolved",
                InboxItemKind::Blocked,
                true,
            ),
            (
                "wi-review-unresolved",
                "inbox-review-unresolved",
                InboxItemKind::ReadyForReview,
                false,
            ),
        ];

        for (work_item_raw, inbox_item_raw, kind, resolved) in rows {
            let work_item_id = crate::WorkItemId::new(work_item_raw);
            let session_id = WorkerSessionId::new(format!("sess-{work_item_raw}"));
            let inbox_item_id = InboxItemId::new(inbox_item_raw);
            projection.work_items.insert(
                work_item_id.clone(),
                WorkItemProjection {
                    id: work_item_id.clone(),
                    ticket_id: None,
                    project_id: None,
                    workflow_state: Some(WorkflowState::Implementing),
                    session_id: Some(session_id.clone()),
                    worktree_id: None,
                    inbox_items: vec![inbox_item_id.clone()],
                    artifacts: vec![],
                    profile_override: None,
                },
            );
            projection.sessions.insert(
                session_id.clone(),
                SessionProjection {
                    id: session_id,
                    work_item_id: Some(work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
            projection.inbox_items.insert(
                inbox_item_id.clone(),
                InboxItemProjection {
                    id: inbox_item_id,
                    work_item_id,
                    kind,
                    title: "row".to_owned(),
                    resolved,
                },
            );
        }

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        let ordered_ids = snapshot
            .items
            .iter()
            .map(|item| item.inbox_item_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_ids,
            vec![
                "inbox-decision-unresolved",
                "inbox-decision-resolved",
                "inbox-approval-unresolved",
                "inbox-review-unresolved",
                "inbox-fyi-resolved",
            ]
        );
    }

    #[test]
    fn ended_sessions_are_filtered_from_attention_snapshot() {
        let mut projection = base_projection();
        let session_id = WorkerSessionId::new("sess-1");
        projection
            .sessions
            .get_mut(&session_id)
            .expect("session")
            .status = Some(WorkerSessionStatus::Done);

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert!(snapshot.items.is_empty());
        assert!(snapshot
            .batch_surfaces
            .iter()
            .all(|surface| surface.total_count == 0 && surface.unresolved_count == 0));
    }

    #[test]
    fn resolved_item_remains_visible_before_auto_dismiss_window() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");
        projection
            .inbox_items
            .get_mut(&inbox_id)
            .expect("inbox item")
            .resolved = true;

        projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id.clone(),
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:00:30Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::InboxItemResolved(crate::InboxItemResolvedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                }),
            ),
            event(
                3,
                "2026-02-16T10:00:59Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: Some(session_id.clone()),
                    work_item_id: Some(work_item_id.clone()),
                    message: "noop".to_owned(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert_eq!(snapshot.items.len(), 1);
        assert!(snapshot.items[0].resolved);
    }

    #[test]
    fn resolved_item_is_filtered_at_or_after_60_seconds() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");
        projection
            .inbox_items
            .get_mut(&inbox_id)
            .expect("inbox item")
            .resolved = true;

        projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id.clone(),
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:00:30Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::InboxItemResolved(crate::InboxItemResolvedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                }),
            ),
            event(
                3,
                "2026-02-16T10:01:30Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: Some(session_id.clone()),
                    work_item_id: Some(work_item_id.clone()),
                    message: "noop".to_owned(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert!(snapshot.items.is_empty());
    }

    #[test]
    fn session_completed_resolution_auto_dismisses_after_60_seconds() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");
        projection
            .inbox_items
            .get_mut(&inbox_id)
            .expect("inbox item")
            .resolved = true;

        projection.events = vec![
            event(
                1,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
            event(
                2,
                "2026-02-16T10:00:00Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::SessionCompleted(crate::SessionCompletedPayload {
                    session_id: session_id.clone(),
                    summary: Some("done".to_owned()),
                }),
            ),
            event(
                3,
                "2026-02-16T10:01:01Z",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: Some(session_id.clone()),
                    work_item_id: Some(work_item_id.clone()),
                    message: "noop".to_owned(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert!(snapshot.items.is_empty());
    }

    #[test]
    fn epoch_timestamp_format_is_parsed_for_timeline() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let inbox_id = InboxItemId::new("inbox-1");

        projection.events = vec![
            event(
                1,
                "1760600000.000000000Z",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
            event(
                2,
                "1760600120.000000000Z",
                None,
                None,
                OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                    session_id: None,
                    work_item_id: None,
                    message: "noop".to_owned(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].score_breakdown.minutes_since_created, 2);
    }

    #[test]
    fn unparseable_resolution_timestamp_does_not_auto_dismiss() {
        let mut projection = base_projection();
        let work_item_id = crate::WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_id = InboxItemId::new("inbox-1");
        projection
            .inbox_items
            .get_mut(&inbox_id)
            .expect("inbox item")
            .resolved = true;

        projection.events = vec![
            event(
                1,
                "not-a-timestamp",
                Some(&work_item_id),
                None,
                OrchestrationEventPayload::InboxItemCreated(InboxItemCreatedPayload {
                    inbox_item_id: inbox_id.clone(),
                    work_item_id: work_item_id.clone(),
                    kind: InboxItemKind::NeedsApproval,
                    title: "Review PR".to_owned(),
                }),
            ),
            event(
                2,
                "still-not-a-timestamp",
                Some(&work_item_id),
                Some(&session_id),
                OrchestrationEventPayload::InboxItemResolved(crate::InboxItemResolvedPayload {
                    inbox_item_id: inbox_id,
                    work_item_id: work_item_id.clone(),
                }),
            ),
        ];

        let snapshot =
            attention_inbox_snapshot(&projection, &AttentionEngineConfig::default(), &[]);
        assert_eq!(snapshot.items.len(), 1);
        assert!(snapshot.items[0].resolved);
    }
}
