use std::collections::HashSet;
use std::io::{self, Stdout};
use std::sync::OnceLock;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use orchestrator_core::{
    command_ids, ArtifactProjection, InboxItemId, InboxItemKind, OrchestrationEventPayload,
    ProjectionState, WorkItemId, WorkerSessionId, WorkerSessionStatus, WorkflowState,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

mod keymap;

pub use keymap::{
    key_stroke_from_event, KeyBindingConfig, KeyPrefixConfig, KeyStroke, KeymapCompileError,
    KeymapConfig, KeymapLookupResult, KeymapTrie, ModeKeymapConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CenterView {
    InboxView,
    FocusCardView { inbox_item_id: InboxItemId },
    TerminalView { session_id: WorkerSessionId },
}

impl CenterView {
    fn label(&self) -> String {
        match self {
            Self::InboxView => "Inbox".to_owned(),
            Self::FocusCardView { inbox_item_id } => {
                format!("FocusCard({})", inbox_item_id.as_str())
            }
            Self::TerminalView { session_id } => format!("Terminal({})", session_id.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UiMode {
    #[default]
    Normal,
    Insert,
    Terminal,
}

impl UiMode {
    fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Insert => "Insert",
            Self::Terminal => "Terminal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStack {
    center: Vec<CenterView>,
}

impl Default for ViewStack {
    fn default() -> Self {
        Self {
            center: vec![CenterView::InboxView],
        }
    }
}

impl ViewStack {
    pub fn active_center(&self) -> Option<&CenterView> {
        self.center.last()
    }

    pub fn center_views(&self) -> &[CenterView] {
        &self.center
    }

    pub fn replace_center(&mut self, view: CenterView) {
        self.center.clear();
        self.center.push(view);
    }

    pub fn push_center(&mut self, view: CenterView) -> bool {
        if self.active_center() == Some(&view) {
            return false;
        }
        self.center.push(view);
        true
    }

    pub fn pop_center(&mut self) -> bool {
        if self.center.len() <= 1 {
            return false;
        }
        self.center.pop();
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiInboxRow {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: WorkItemId,
    pub kind: InboxItemKind,
    pub priority_score: i32,
    pub priority_band: InboxPriorityBand,
    pub batch_kind: InboxBatchKind,
    pub title: String,
    pub resolved: bool,
    pub workflow_state: Option<WorkflowState>,
    pub session_id: Option<WorkerSessionId>,
    pub session_status: Option<WorkerSessionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CenterPaneState {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InboxPriorityBand {
    Urgent,
    Attention,
    Background,
}

impl InboxPriorityBand {
    fn label(self) -> &'static str {
        match self {
            Self::Urgent => "Urgent",
            Self::Attention => "Attention",
            Self::Background => "Background",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Urgent => 0,
            Self::Attention => 1,
            Self::Background => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxBatchKind {
    DecideOrUnblock,
    Approvals,
    ReviewReady,
    FyiDigest,
}

impl InboxBatchKind {
    const ORDERED: [InboxBatchKind; 4] = [
        InboxBatchKind::DecideOrUnblock,
        InboxBatchKind::Approvals,
        InboxBatchKind::ReviewReady,
        InboxBatchKind::FyiDigest,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::DecideOrUnblock => "Decide / Unblock",
            Self::Approvals => "Approvals",
            Self::ReviewReady => "PR Reviews",
            Self::FyiDigest => "FYI Digest",
        }
    }

    fn hotkey(self) -> char {
        match self {
            Self::DecideOrUnblock => '1',
            Self::Approvals => '2',
            Self::ReviewReady => '3',
            Self::FyiDigest => '4',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiBatchSurface {
    pub kind: InboxBatchKind,
    pub unresolved_count: usize,
    pub total_count: usize,
    pub first_unresolved_index: Option<usize>,
    pub first_any_index: Option<usize>,
}

impl UiBatchSurface {
    fn selection_index(&self) -> Option<usize> {
        self.first_unresolved_index.or(self.first_any_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    pub status: String,
    pub inbox_rows: Vec<UiInboxRow>,
    pub inbox_batch_surfaces: Vec<UiBatchSurface>,
    pub selected_inbox_index: Option<usize>,
    pub selected_inbox_item_id: Option<InboxItemId>,
    pub selected_work_item_id: Option<WorkItemId>,
    pub selected_session_id: Option<WorkerSessionId>,
    pub center_view_stack: Vec<CenterView>,
    pub center_pane: CenterPaneState,
}

impl UiState {
    pub fn center_stack_label(&self) -> String {
        self.center_view_stack
            .iter()
            .map(CenterView::label)
            .collect::<Vec<_>>()
            .join(" > ")
    }
}

pub fn project_ui_state(
    status: &str,
    domain: &ProjectionState,
    view_stack: &ViewStack,
    preferred_selected_inbox: Option<usize>,
    preferred_selected_inbox_item_id: Option<&InboxItemId>,
) -> UiState {
    let mut inbox_rows = domain
        .inbox_items
        .values()
        .map(|item| {
            let work_item = domain.work_items.get(&item.work_item_id);
            let session_id = work_item.and_then(|entry| entry.session_id.clone());
            let session_status = session_id
                .as_ref()
                .and_then(|id| domain.sessions.get(id))
                .and_then(|session| session.status.clone());
            let workflow_state = work_item.and_then(|entry| entry.workflow_state.clone());
            let priority_score = inbox_priority_score(
                &item.kind,
                item.resolved,
                workflow_state.as_ref(),
                session_status.as_ref(),
            );
            let priority_band = inbox_priority_band(priority_score, item.resolved);
            UiInboxRow {
                inbox_item_id: item.id.clone(),
                work_item_id: item.work_item_id.clone(),
                kind: item.kind.clone(),
                priority_score,
                priority_band,
                batch_kind: inbox_batch_kind(&item.kind),
                title: item.title.clone(),
                resolved: item.resolved,
                workflow_state,
                session_id,
                session_status,
            }
        })
        .collect::<Vec<_>>();
    inbox_rows.sort_by(|a, b| {
        a.priority_band
            .rank()
            .cmp(&b.priority_band.rank())
            .then_with(|| b.priority_score.cmp(&a.priority_score))
            .then_with(|| a.resolved.cmp(&b.resolved))
            .then_with(|| a.inbox_item_id.as_str().cmp(b.inbox_item_id.as_str()))
    });
    let inbox_batch_surfaces = build_batch_surfaces(&inbox_rows);

    let selected_inbox_index = resolve_selected_inbox(
        preferred_selected_inbox,
        preferred_selected_inbox_item_id,
        &inbox_rows,
    );
    let selected_row = selected_inbox_index.map(|idx| &inbox_rows[idx]);
    let selected_inbox_item_id = selected_row.map(|row| row.inbox_item_id.clone());
    let selected_work_item_id = selected_row.map(|row| row.work_item_id.clone());
    let selected_session_id = selected_row.and_then(|row| row.session_id.clone());

    let center_view_stack = view_stack.center_views().to_vec();
    let active_center = view_stack
        .active_center()
        .cloned()
        .unwrap_or(CenterView::InboxView);
    let center_pane =
        project_center_pane(&active_center, &inbox_rows, &inbox_batch_surfaces, domain);

    UiState {
        status: status.to_owned(),
        inbox_rows,
        inbox_batch_surfaces,
        selected_inbox_index,
        selected_inbox_item_id,
        selected_work_item_id,
        selected_session_id,
        center_view_stack,
        center_pane,
    }
}

fn resolve_selected_inbox(
    preferred_index: Option<usize>,
    preferred_inbox_item_id: Option<&InboxItemId>,
    inbox_rows: &[UiInboxRow],
) -> Option<usize> {
    if inbox_rows.is_empty() {
        return None;
    }

    if let Some(preferred_inbox_item_id) = preferred_inbox_item_id {
        if let Some(index) = inbox_rows
            .iter()
            .position(|row| &row.inbox_item_id == preferred_inbox_item_id)
        {
            return Some(index);
        }
    }

    Some(preferred_index.unwrap_or(0).min(inbox_rows.len() - 1))
}

fn inbox_priority_score(
    kind: &InboxItemKind,
    resolved: bool,
    workflow_state: Option<&WorkflowState>,
    session_status: Option<&WorkerSessionStatus>,
) -> i32 {
    let mut score = match kind {
        InboxItemKind::NeedsDecision => 100,
        InboxItemKind::Blocked => 95,
        InboxItemKind::NeedsApproval => 75,
        InboxItemKind::ReadyForReview => 70,
        InboxItemKind::FYI => 30,
    };

    if resolved {
        score -= 500;
    }

    if matches!(session_status, Some(WorkerSessionStatus::WaitingForUser)) {
        score += 20;
    }
    if matches!(session_status, Some(WorkerSessionStatus::Blocked)) {
        score += 15;
    }

    if matches!(
        workflow_state,
        Some(WorkflowState::AwaitingYourReview | WorkflowState::ReadyForReview)
    ) {
        score += 10;
    }

    score
}

fn inbox_priority_band(score: i32, resolved: bool) -> InboxPriorityBand {
    if resolved {
        return InboxPriorityBand::Background;
    }

    if score >= 95 {
        InboxPriorityBand::Urgent
    } else if score >= 60 {
        InboxPriorityBand::Attention
    } else {
        InboxPriorityBand::Background
    }
}

fn inbox_batch_kind(kind: &InboxItemKind) -> InboxBatchKind {
    match kind {
        InboxItemKind::NeedsDecision | InboxItemKind::Blocked => InboxBatchKind::DecideOrUnblock,
        InboxItemKind::NeedsApproval => InboxBatchKind::Approvals,
        InboxItemKind::ReadyForReview => InboxBatchKind::ReviewReady,
        InboxItemKind::FYI => InboxBatchKind::FyiDigest,
    }
}

fn build_batch_surfaces(rows: &[UiInboxRow]) -> Vec<UiBatchSurface> {
    InboxBatchKind::ORDERED
        .iter()
        .map(|kind| UiBatchSurface {
            kind: *kind,
            unresolved_count: rows
                .iter()
                .filter(|row| row.batch_kind == *kind && !row.resolved)
                .count(),
            total_count: rows.iter().filter(|row| row.batch_kind == *kind).count(),
            first_unresolved_index: rows
                .iter()
                .position(|row| row.batch_kind == *kind && !row.resolved),
            first_any_index: rows.iter().position(|row| row.batch_kind == *kind),
        })
        .collect()
}

fn project_center_pane(
    active_center: &CenterView,
    inbox_rows: &[UiInboxRow],
    inbox_batch_surfaces: &[UiBatchSurface],
    domain: &ProjectionState,
) -> CenterPaneState {
    match active_center {
        CenterView::InboxView => {
            let unresolved = inbox_rows.iter().filter(|item| !item.resolved).count();
            let mut lines = vec![format!("{unresolved} unresolved inbox items")];
            lines.extend(
                inbox_batch_surfaces
                    .iter()
                    .filter(|surface| surface.total_count > 0)
                    .map(|surface| {
                        format!(
                            "[{}] {}: {} unresolved / {} total",
                            surface.kind.hotkey(),
                            surface.kind.label(),
                            surface.unresolved_count,
                            surface.total_count
                        )
                    }),
            );
            lines.push("j/k or arrows: move selection".to_owned());
            lines.push("Tab/Shift+Tab: cycle batch lanes".to_owned());
            lines.push("g/G: jump first/last item".to_owned());
            lines.push("Enter: open focus card".to_owned());
            lines.push("t: open terminal for selected item".to_owned());
            lines.push("Backspace: minimize top view".to_owned());
            CenterPaneState {
                title: "Inbox View".to_owned(),
                lines,
            }
        }
        CenterView::FocusCardView { inbox_item_id } => {
            project_focus_card_pane(inbox_item_id, inbox_rows, domain)
        }
        CenterView::TerminalView { session_id } => {
            let lines = if let Some(session) = domain.sessions.get(session_id) {
                let status = session
                    .status
                    .as_ref()
                    .map(|entry| format!("{entry:?}"))
                    .unwrap_or_else(|| "Unknown".to_owned());
                let checkpoint = session
                    .latest_checkpoint
                    .as_ref()
                    .map(|entry| entry.as_str().to_owned())
                    .unwrap_or_else(|| "none".to_owned());
                vec![
                    format!("Session: {}", session_id.as_str()),
                    format!("Status: {status}"),
                    format!("Latest checkpoint: {checkpoint}"),
                    "PTY embedding lands in a later ticket.".to_owned(),
                ]
            } else {
                vec![
                    format!("Session: {}", session_id.as_str()),
                    "Session state is unavailable in current projection.".to_owned(),
                ]
            };

            CenterPaneState {
                title: format!("Terminal {}", session_id.as_str()),
                lines,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct FocusCardEventContext {
    latest_needs_input_prompt: Option<String>,
    latest_blocked_reason: Option<String>,
    latest_checkpoint_summary: Option<String>,
}

const FOCUS_CARD_EVENT_SCAN_LIMIT: usize = 512;
const FOCUS_CARD_ARTIFACT_LIMIT: usize = 6;
const FOCUS_CARD_TEXT_MAX_CHARS: usize = 220;

impl FocusCardEventContext {
    fn is_complete(&self) -> bool {
        self.latest_needs_input_prompt.is_some()
            && self.latest_blocked_reason.is_some()
            && self.latest_checkpoint_summary.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FocusCardDetails {
    why_attention: String,
    recommended_response: String,
    evidence_lines: Vec<String>,
}

fn project_focus_card_pane(
    inbox_item_id: &InboxItemId,
    inbox_rows: &[UiInboxRow],
    domain: &ProjectionState,
) -> CenterPaneState {
    if let Some(item) = inbox_rows
        .iter()
        .find(|row| &row.inbox_item_id == inbox_item_id)
    {
        let workflow = item
            .workflow_state
            .as_ref()
            .map(|state| format!("{state:?}"))
            .unwrap_or_else(|| "Unknown".to_owned());
        let session = match (&item.session_id, &item.session_status) {
            (Some(session_id), Some(status)) => format!("{} ({status:?})", session_id.as_str()),
            (Some(session_id), None) => format!("{} (Unknown)", session_id.as_str()),
            (None, _) => "None".to_owned(),
        };
        let focus_details = build_focus_card_details(item, domain);
        let mut lines = vec![
            format!("Title: {}", item.title),
            format!("Kind: {:?}", item.kind),
            format!("Work item: {}", item.work_item_id.as_str()),
            format!("Workflow: {workflow}"),
            format!("Session: {session}"),
            String::new(),
            "Why attention is required:".to_owned(),
            format!("- {}", focus_details.why_attention),
            String::new(),
            "Recommended response:".to_owned(),
            format!("- {}", focus_details.recommended_response),
            String::new(),
            "Evidence:".to_owned(),
        ];
        if focus_details.evidence_lines.is_empty() {
            lines.push("- No evidence links or artifacts yet.".to_owned());
        } else {
            lines.extend(
                focus_details
                    .evidence_lines
                    .iter()
                    .map(|line| format!("- {line}")),
            );
        }
        CenterPaneState {
            title: format!("Focus Card {}", inbox_item_id.as_str()),
            lines,
        }
    } else {
        CenterPaneState {
            title: format!("Focus Card {}", inbox_item_id.as_str()),
            lines: vec!["Selected inbox item is not available.".to_owned()],
        }
    }
}

fn build_focus_card_details(item: &UiInboxRow, domain: &ProjectionState) -> FocusCardDetails {
    let event_context =
        collect_focus_card_event_context(domain, &item.work_item_id, item.session_id.as_ref());
    let evidence_lines = collect_focus_card_evidence_lines(domain, item, &event_context);

    let mut why_attention = match item.kind {
        InboxItemKind::NeedsDecision => {
            "The worker needs a decision before it can continue implementation.".to_owned()
        }
        InboxItemKind::Blocked => {
            "The worker reported a blocker and cannot self-progress.".to_owned()
        }
        InboxItemKind::NeedsApproval => {
            "The workflow hit a human-approval gate before the next transition.".to_owned()
        }
        InboxItemKind::ReadyForReview => {
            "The work item is prepared for review and final direction.".to_owned()
        }
        InboxItemKind::FYI => {
            "This is a progress digest and does not require an immediate interrupt.".to_owned()
        }
    };

    if matches!(
        item.session_status,
        Some(WorkerSessionStatus::WaitingForUser)
    ) {
        why_attention.push_str(" Session status is WaitingForUser.");
    } else if matches!(item.session_status, Some(WorkerSessionStatus::Blocked)) {
        why_attention.push_str(" Session status is Blocked.");
    }

    let recommended_response = match item.kind {
        InboxItemKind::NeedsDecision => {
            if let Some(prompt) = event_context.latest_needs_input_prompt.as_deref() {
                format!("Answer the worker prompt to unblock progress: {prompt}")
            } else {
                "Provide a clear decision and continue the session.".to_owned()
            }
        }
        InboxItemKind::Blocked => {
            if let Some(reason) = event_context.latest_blocked_reason.as_deref() {
                format!("Address the blocker and resume the session: {reason}")
            } else {
                "Review the latest logs/artifacts, resolve the blocker, then resume.".to_owned()
            }
        }
        InboxItemKind::NeedsApproval => {
            "Review the linked artifacts (PR/tests/diff) and approve the next workflow gate if ready."
                .to_owned()
        }
        InboxItemKind::ReadyForReview => {
            "Review the latest evidence and leave review feedback or advance the item.".to_owned()
        }
        InboxItemKind::FYI => "No immediate action required; review when you batch FYI updates.".to_owned(),
    };

    FocusCardDetails {
        why_attention,
        recommended_response,
        evidence_lines,
    }
}

fn collect_focus_card_event_context(
    domain: &ProjectionState,
    work_item_id: &WorkItemId,
    session_id: Option<&WorkerSessionId>,
) -> FocusCardEventContext {
    let mut context = FocusCardEventContext::default();
    let mut matched_event_count = 0usize;

    for event in domain.events.iter().rev() {
        let matches_focus_context = match session_id {
            Some(session_id) => {
                event.session_id.as_ref() == Some(session_id)
                    || (event.work_item_id.as_ref() == Some(work_item_id)
                        && event.session_id.is_none())
            }
            None => event.work_item_id.as_ref() == Some(work_item_id),
        };
        if !matches_focus_context {
            continue;
        }
        matched_event_count += 1;

        match &event.payload {
            OrchestrationEventPayload::SessionNeedsInput(payload)
                if context.latest_needs_input_prompt.is_none() =>
            {
                context.latest_needs_input_prompt = Some(compact_focus_card_text(&payload.prompt));
            }
            OrchestrationEventPayload::SessionBlocked(payload)
                if context.latest_blocked_reason.is_none() =>
            {
                context.latest_blocked_reason = Some(compact_focus_card_text(&payload.reason));
            }
            OrchestrationEventPayload::SessionCheckpoint(payload)
                if context.latest_checkpoint_summary.is_none() =>
            {
                context.latest_checkpoint_summary = Some(compact_focus_card_text(&payload.summary));
            }
            _ => {}
        }

        if context.is_complete() || matched_event_count >= FOCUS_CARD_EVENT_SCAN_LIMIT {
            break;
        }
    }

    context
}

fn compact_focus_card_text(raw: &str) -> String {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= FOCUS_CARD_TEXT_MAX_CHARS {
        return compact;
    }

    let keep = FOCUS_CARD_TEXT_MAX_CHARS.saturating_sub(3);
    let truncated = compact.chars().take(keep).collect::<String>();
    format!("{truncated}...")
}

fn collect_focus_card_evidence_lines(
    domain: &ProjectionState,
    item: &UiInboxRow,
    event_context: &FocusCardEventContext,
) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(prompt) = event_context.latest_needs_input_prompt.as_deref() {
        lines.push(format!("Latest input request: {prompt}"));
    }
    if let Some(reason) = event_context.latest_blocked_reason.as_deref() {
        lines.push(format!("Latest blocker reason: {reason}"));
    }
    if let Some(summary) = event_context.latest_checkpoint_summary.as_deref() {
        lines.push(format!("Latest checkpoint summary: {summary}"));
    }

    if let Some(work_item) = domain.work_items.get(&item.work_item_id) {
        let mut seen_artifact_ids = HashSet::new();
        let mut shown_artifacts = 0usize;
        let mut omitted_artifacts = 0usize;

        for artifact_id in work_item.artifacts.iter().rev() {
            if !seen_artifact_ids.insert(artifact_id.clone()) {
                continue;
            }
            let Some(artifact) = domain.artifacts.get(artifact_id) else {
                continue;
            };

            if shown_artifacts < FOCUS_CARD_ARTIFACT_LIMIT {
                lines.push(format_artifact_evidence_line(artifact));
                shown_artifacts += 1;
            } else {
                omitted_artifacts += 1;
            }
        }

        if omitted_artifacts > 0 {
            lines.push(format!("{omitted_artifacts} older artifacts not shown."));
        }
    }

    lines
}

fn format_artifact_evidence_line(artifact: &ArtifactProjection) -> String {
    format!(
        "{:?} artifact '{}' -> {}",
        artifact.kind, artifact.label, artifact.uri
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WhichKeyHint {
    key: KeyStroke,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WhichKeyOverlayState {
    prefix: Vec<KeyStroke>,
    group_label: Option<String>,
    hints: Vec<WhichKeyHint>,
}

#[derive(Debug, Clone)]
struct UiShellState {
    status: String,
    domain: ProjectionState,
    selected_inbox_index: Option<usize>,
    selected_inbox_item_id: Option<InboxItemId>,
    view_stack: ViewStack,
    keymap: &'static KeymapTrie,
    mode_key_buffer: Vec<KeyStroke>,
    which_key_overlay: Option<WhichKeyOverlayState>,
    mode: UiMode,
    terminal_escape_pending: bool,
}

impl UiShellState {
    fn new(status: String, domain: ProjectionState) -> Self {
        Self {
            status,
            domain,
            selected_inbox_index: None,
            selected_inbox_item_id: None,
            view_stack: ViewStack::default(),
            keymap: default_keymap_trie(),
            mode_key_buffer: Vec::new(),
            which_key_overlay: None,
            mode: UiMode::Normal,
            terminal_escape_pending: false,
        }
    }

    fn ui_state(&self) -> UiState {
        project_ui_state(
            &self.status,
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
        )
    }

    fn move_selection(&mut self, delta: isize) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            self.selected_inbox_index = None;
            self.selected_inbox_item_id = None;
            return;
        }

        let current = ui_state.selected_inbox_index.unwrap_or(0) as isize;
        let upper_bound = ui_state.inbox_rows.len() as isize - 1;
        let next = (current + delta).clamp(0, upper_bound) as usize;
        self.set_selection(Some(next), &ui_state.inbox_rows);
    }

    fn jump_to_first_item(&mut self) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(0), &ui_state.inbox_rows);
    }

    fn jump_to_last_item(&mut self) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(ui_state.inbox_rows.len() - 1), &ui_state.inbox_rows);
    }

    fn jump_to_batch(&mut self, target: InboxBatchKind) {
        let ui_state = self.ui_state();
        if let Some(index) = ui_state
            .inbox_batch_surfaces
            .iter()
            .find(|surface| surface.kind == target)
            .and_then(UiBatchSurface::selection_index)
        {
            self.set_selection(Some(index), &ui_state.inbox_rows);
        }
    }

    fn cycle_batch(&mut self, delta: isize) {
        let ui_state = self.ui_state();
        let selectable_surfaces = ui_state
            .inbox_batch_surfaces
            .iter()
            .filter(|surface| surface.total_count > 0)
            .collect::<Vec<_>>();

        if selectable_surfaces.is_empty() {
            return;
        }

        let current_surface_idx = ui_state.selected_inbox_index.and_then(|selected_index| {
            let selected_row = ui_state.inbox_rows.get(selected_index)?;
            selectable_surfaces
                .iter()
                .position(|surface| selected_row.batch_kind == surface.kind)
        });

        let current = current_surface_idx.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(selectable_surfaces.len() as isize) as usize;

        if let Some(index) = selectable_surfaces[next].selection_index() {
            self.set_selection(Some(index), &ui_state.inbox_rows);
        }
    }

    fn open_focus_card_for_selected(&mut self) {
        let ui_state = self.ui_state();
        if let Some(inbox_item_id) = ui_state.selected_inbox_item_id {
            self.selected_inbox_item_id = Some(inbox_item_id.clone());
            self.view_stack
                .replace_center(CenterView::FocusCardView { inbox_item_id });
        }
    }

    fn open_terminal_for_selected(&mut self) {
        let ui_state = self.ui_state();
        if let (Some(inbox_item_id), Some(session_id)) = (
            ui_state.selected_inbox_item_id,
            ui_state.selected_session_id,
        ) {
            self.selected_inbox_item_id = Some(inbox_item_id.clone());
            let focus_view = CenterView::FocusCardView { inbox_item_id };
            self.view_stack.replace_center(focus_view);
            self.view_stack
                .push_center(CenterView::TerminalView { session_id });
        }
    }

    fn minimize_center_view(&mut self) {
        let _ = self.view_stack.pop_center();
        if !self.is_terminal_view_active() {
            self.enter_normal_mode();
        }
    }

    fn set_selection(&mut self, selected_index: Option<usize>, rows: &[UiInboxRow]) {
        let valid_selected_index = selected_index.filter(|index| *index < rows.len());
        self.selected_inbox_index = valid_selected_index;
        self.selected_inbox_item_id =
            valid_selected_index.map(|index| rows[index].inbox_item_id.clone());
    }

    fn enter_normal_mode(&mut self) {
        self.mode = UiMode::Normal;
        self.mode_key_buffer.clear();
        self.which_key_overlay = None;
        self.terminal_escape_pending = false;
    }

    fn enter_insert_mode(&mut self) {
        if !self.is_terminal_view_active() {
            self.mode = UiMode::Insert;
            self.mode_key_buffer.clear();
            self.which_key_overlay = None;
            self.terminal_escape_pending = false;
        }
    }

    fn enter_terminal_mode(&mut self) {
        if self.is_terminal_view_active() {
            self.mode = UiMode::Terminal;
            self.mode_key_buffer.clear();
            self.which_key_overlay = None;
            self.terminal_escape_pending = false;
        }
    }

    fn open_terminal_and_enter_mode(&mut self) {
        self.open_terminal_for_selected();
        self.enter_terminal_mode();
    }

    fn begin_terminal_escape_chord(&mut self) {
        if self.mode == UiMode::Terminal {
            self.terminal_escape_pending = true;
        }
    }

    fn forward_terminal_key(&mut self, _key: KeyEvent) {
        // Raw PTY input wiring is introduced in later terminal embedding tickets.
    }

    fn is_terminal_view_active(&self) -> bool {
        matches!(
            self.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        )
    }

    fn refresh_which_key_overlay(&mut self) {
        if self.mode_key_buffer.is_empty() {
            self.which_key_overlay = None;
            return;
        }

        let Some(prefix_hint_view) = self
            .keymap
            .prefix_hints(self.mode, self.mode_key_buffer.as_slice())
        else {
            self.which_key_overlay = None;
            return;
        };

        let hints = prefix_hint_view
            .hints
            .into_iter()
            .map(|hint| WhichKeyHint {
                key: hint.key,
                description: describe_next_key_binding(&hint),
            })
            .collect();

        self.which_key_overlay = Some(WhichKeyOverlayState {
            prefix: self.mode_key_buffer.clone(),
            group_label: prefix_hint_view.label,
            hints,
        });
    }
}

pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Ui {
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    pub fn run(&mut self, status: &str, projection: &ProjectionState) -> io::Result<()> {
        let mut shell_state = UiShellState::new(status.to_owned(), projection.clone());
        loop {
            let ui_state = shell_state.ui_state();
            self.terminal.draw(|frame| {
                let area = frame.area();
                let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
                let [main, footer] = layout.areas(area);
                let main_layout =
                    Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]);
                let [inbox_area, center_area] = main_layout.areas(main);

                let inbox_text = render_inbox_panel(&ui_state);
                frame.render_widget(
                    Paragraph::new(inbox_text)
                        .block(Block::default().title("inbox").borders(Borders::ALL)),
                    inbox_area,
                );

                let center_text = render_center_panel(&ui_state);
                frame.render_widget(
                    Paragraph::new(center_text).block(
                        Block::default()
                            .title(ui_state.center_pane.title.as_str())
                            .borders(Borders::ALL),
                    ),
                    center_area,
                );

                let footer_text = format!(
                    "status: {} | mode: {} | {}",
                    ui_state.status,
                    shell_state.mode.label(),
                    mode_help(shell_state.mode)
                );
                frame.render_widget(
                    Paragraph::new(footer_text)
                        .block(Block::default().title("shell").borders(Borders::ALL)),
                    footer,
                );

                if let Some(which_key) = shell_state.which_key_overlay.as_ref() {
                    render_which_key_overlay(frame, center_area, which_key);
                }
            })?;

            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && handle_key_press(&mut shell_state, key) {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn render_inbox_panel(ui_state: &UiState) -> String {
    if ui_state.inbox_rows.is_empty() {
        return "No inbox items.".to_owned();
    }

    let mut lines = Vec::new();

    let batch_lane_summary = ui_state
        .inbox_batch_surfaces
        .iter()
        .filter(|surface| surface.total_count > 0)
        .map(|surface| {
            format!(
                "[{}] {} {}",
                surface.kind.hotkey(),
                surface.kind.label(),
                surface.unresolved_count
            )
        })
        .collect::<Vec<_>>();
    if !batch_lane_summary.is_empty() {
        lines.push(format!("Batch lanes: {}", batch_lane_summary.join(" | ")));
        lines.push(String::new());
    }

    let mut active_band = None;
    for (index, row) in ui_state.inbox_rows.iter().enumerate() {
        if active_band != Some(row.priority_band) {
            if active_band.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("{}:", row.priority_band.label()));
            active_band = Some(row.priority_band);
        }

        let selected = if Some(index) == ui_state.selected_inbox_index {
            ">"
        } else {
            " "
        };
        let resolved = if row.resolved { "x" } else { " " };
        lines.push(format!(
            "{selected} [{resolved}] {:?}: {}",
            row.kind, row.title
        ));
    }

    lines.join("\n")
}

fn render_center_panel(ui_state: &UiState) -> String {
    let mut lines = Vec::with_capacity(ui_state.center_pane.lines.len() + 2);
    lines.push(format!("Stack: {}", ui_state.center_stack_label()));
    lines.push(String::new());
    lines.extend(ui_state.center_pane.lines.iter().cloned());
    lines.join("\n")
}

fn render_which_key_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    overlay: &WhichKeyOverlayState,
) {
    let content = render_which_key_overlay_text(overlay);
    let Some(popup) = which_key_overlay_popup(anchor_area, content.as_str()) else {
        return;
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("which-key").borders(Borders::ALL)),
        popup,
    );
}

fn which_key_overlay_popup(anchor_area: Rect, content: &str) -> Option<Rect> {
    let line_count = content.lines().count();
    if line_count == 0 {
        return None;
    }

    let max_width = content.lines().map(|line| line.chars().count()).max()?;
    let desired_width = u16::try_from(max_width)
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let desired_height = u16::try_from(line_count)
        .unwrap_or(u16::MAX)
        .saturating_add(2);

    let width = desired_width.min(anchor_area.width);
    let height = desired_height.min(anchor_area.height);
    if width < 4 || height < 3 {
        return None;
    }

    Some(Rect {
        x: anchor_area
            .x
            .saturating_add(anchor_area.width.saturating_sub(width)),
        y: anchor_area
            .y
            .saturating_add(anchor_area.height.saturating_sub(height)),
        width,
        height,
    })
}

fn render_which_key_overlay_text(overlay: &WhichKeyOverlayState) -> String {
    let mut lines = Vec::with_capacity(overlay.hints.len() + 2);
    let prefix = overlay
        .prefix
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(label) = overlay.group_label.as_deref() {
        lines.push(format!("{prefix}  ({label})"));
    } else {
        lines.push(prefix);
    }

    lines.extend(
        overlay
            .hints
            .iter()
            .map(|hint| format!("{:>8}  {}", hint.key, hint.description)),
    );
    lines.join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiCommand {
    EnterNormalMode,
    EnterInsertMode,
    OpenTerminalForSelected,
    StartTerminalEscapeChord,
    QuitShell,
    FocusNextInbox,
    FocusPreviousInbox,
    CycleBatchNext,
    CycleBatchPrevious,
    JumpFirstInbox,
    JumpLastInbox,
    JumpBatchDecideOrUnblock,
    JumpBatchApprovals,
    JumpBatchReviewReady,
    JumpBatchFyiDigest,
    OpenFocusCard,
    MinimizeCenterView,
}

impl UiCommand {
    const ALL: [Self; 17] = [
        Self::EnterNormalMode,
        Self::EnterInsertMode,
        Self::OpenTerminalForSelected,
        Self::StartTerminalEscapeChord,
        Self::QuitShell,
        Self::FocusNextInbox,
        Self::FocusPreviousInbox,
        Self::CycleBatchNext,
        Self::CycleBatchPrevious,
        Self::JumpFirstInbox,
        Self::JumpLastInbox,
        Self::JumpBatchDecideOrUnblock,
        Self::JumpBatchApprovals,
        Self::JumpBatchReviewReady,
        Self::JumpBatchFyiDigest,
        Self::OpenFocusCard,
        Self::MinimizeCenterView,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "ui.mode.normal",
            Self::EnterInsertMode => "ui.mode.insert",
            Self::OpenTerminalForSelected => command_ids::UI_OPEN_TERMINAL_FOR_SELECTED,
            Self::StartTerminalEscapeChord => "ui.mode.terminal_escape_prefix",
            Self::QuitShell => "ui.shell.quit",
            Self::FocusNextInbox => command_ids::UI_FOCUS_NEXT_INBOX,
            Self::FocusPreviousInbox => "ui.focus_previous_inbox",
            Self::CycleBatchNext => "ui.cycle_batch_next",
            Self::CycleBatchPrevious => "ui.cycle_batch_previous",
            Self::JumpFirstInbox => "ui.jump_first_inbox",
            Self::JumpLastInbox => "ui.jump_last_inbox",
            Self::JumpBatchDecideOrUnblock => "ui.jump_batch.decide_or_unblock",
            Self::JumpBatchApprovals => "ui.jump_batch.approvals",
            Self::JumpBatchReviewReady => "ui.jump_batch.review_ready",
            Self::JumpBatchFyiDigest => "ui.jump_batch.fyi_digest",
            Self::OpenFocusCard => "ui.open_focus_card_for_selected",
            Self::MinimizeCenterView => "ui.center.pop",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "Return to Normal mode",
            Self::EnterInsertMode => "Enter Insert mode",
            Self::OpenTerminalForSelected => "Open terminal for selected item",
            Self::StartTerminalEscapeChord => "Terminal escape chord (Ctrl-\\ Ctrl-n)",
            Self::QuitShell => "Quit shell",
            Self::FocusNextInbox => "Focus next inbox item",
            Self::FocusPreviousInbox => "Focus previous inbox item",
            Self::CycleBatchNext => "Cycle to next inbox lane",
            Self::CycleBatchPrevious => "Cycle to previous inbox lane",
            Self::JumpFirstInbox => "Jump to first inbox item",
            Self::JumpLastInbox => "Jump to last inbox item",
            Self::JumpBatchDecideOrUnblock => "Jump to Decide/Unblock lane",
            Self::JumpBatchApprovals => "Jump to Approvals lane",
            Self::JumpBatchReviewReady => "Jump to PR Reviews lane",
            Self::JumpBatchFyiDigest => "Jump to FYI Digest lane",
            Self::OpenFocusCard => "Open focus card for selected item",
            Self::MinimizeCenterView => "Minimize active center view",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }

    fn is_registered(id: &str) -> bool {
        Self::from_id(id).is_some()
    }
}

fn describe_next_key_binding(hint: &keymap::PrefixHint) -> String {
    if let Some(command_id) = hint.command_id.as_deref() {
        return UiCommand::from_id(command_id)
            .map(UiCommand::description)
            .unwrap_or(command_id)
            .to_owned();
    }
    if let Some(prefix_label) = hint.prefix_label.as_deref() {
        return format!("{prefix_label} (prefix)");
    }
    "Prefix".to_owned()
}

fn default_keymap_config() -> KeymapConfig {
    let binding = |keys: &[&str], command: UiCommand| KeyBindingConfig {
        keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        command_id: command.id().to_owned(),
    };

    KeymapConfig {
        modes: vec![
            ModeKeymapConfig {
                mode: UiMode::Normal,
                bindings: vec![
                    binding(&["q"], UiCommand::QuitShell),
                    binding(&["down"], UiCommand::FocusNextInbox),
                    binding(&["j"], UiCommand::FocusNextInbox),
                    binding(&["up"], UiCommand::FocusPreviousInbox),
                    binding(&["k"], UiCommand::FocusPreviousInbox),
                    binding(&["tab"], UiCommand::CycleBatchNext),
                    binding(&["backtab"], UiCommand::CycleBatchPrevious),
                    binding(&["g"], UiCommand::JumpFirstInbox),
                    binding(&["G"], UiCommand::JumpLastInbox),
                    binding(&["1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["2"], UiCommand::JumpBatchApprovals),
                    binding(&["3"], UiCommand::JumpBatchReviewReady),
                    binding(&["4"], UiCommand::JumpBatchFyiDigest),
                    binding(&["enter"], UiCommand::OpenFocusCard),
                    binding(&["t"], UiCommand::OpenTerminalForSelected),
                    binding(&["backspace"], UiCommand::MinimizeCenterView),
                    binding(&["i"], UiCommand::EnterInsertMode),
                    binding(&["z", "1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["z", "2"], UiCommand::JumpBatchApprovals),
                    binding(&["z", "3"], UiCommand::JumpBatchReviewReady),
                    binding(&["z", "4"], UiCommand::JumpBatchFyiDigest),
                ],
                prefixes: vec![KeyPrefixConfig {
                    keys: vec!["z".to_owned()],
                    label: "Batch jumps".to_owned(),
                }],
            },
            ModeKeymapConfig {
                mode: UiMode::Insert,
                bindings: Vec::new(),
                prefixes: Vec::new(),
            },
            ModeKeymapConfig {
                mode: UiMode::Terminal,
                bindings: Vec::new(),
                prefixes: Vec::new(),
            },
        ],
    }
}

fn default_keymap_trie() -> &'static KeymapTrie {
    static KEYMAP: OnceLock<KeymapTrie> = OnceLock::new();
    KEYMAP.get_or_init(|| {
        KeymapTrie::compile(&default_keymap_config(), UiCommand::is_registered)
            .expect("default UI keymap must compile without conflicts")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalPassthrough {
    replay_escape_prefix: bool,
    key: KeyEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutedInput {
    Command(UiCommand),
    TerminalPassthrough(TerminalPassthrough),
    Ignore,
}

fn mode_help(mode: UiMode) -> &'static str {
    match mode {
        UiMode::Normal => {
            "j/k: select | Tab/S-Tab: batch cycle | 1-4 or z{1-4}: batch jump | g/G: first/last | Enter: focus | t: terminal | i: insert | q: quit"
        }
        UiMode::Insert => "Insert input active | Esc/Ctrl-[: Normal",
        UiMode::Terminal => "Terminal pass-through active | Esc or Ctrl-\\ Ctrl-n: Normal",
    }
}

fn handle_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> bool {
    match route_key_press(shell_state, key) {
        RoutedInput::Command(command) => dispatch_command(shell_state, command),
        RoutedInput::TerminalPassthrough(passthrough) => {
            if passthrough.replay_escape_prefix {
                shell_state.forward_terminal_key(KeyEvent::new(
                    KeyCode::Char('\\'),
                    KeyModifiers::CONTROL,
                ));
            }
            shell_state.forward_terminal_key(passthrough.key);
            shell_state.terminal_escape_pending = false;
            false
        }
        RoutedInput::Ignore => false,
    }
}

fn route_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key) {
        return RoutedInput::Command(UiCommand::EnterNormalMode);
    }

    match shell_state.mode {
        UiMode::Normal | UiMode::Insert => route_configured_mode_key(shell_state, key),
        UiMode::Terminal => {
            if shell_state.is_terminal_view_active() {
                route_terminal_mode_key(shell_state, key)
            } else {
                RoutedInput::Command(UiCommand::EnterNormalMode)
            }
        }
    }
}

fn route_configured_mode_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    match shell_state.keymap.route_key_event(
        shell_state.mode,
        &mut shell_state.mode_key_buffer,
        key,
    ) {
        KeymapLookupResult::Command { command_id } => {
            shell_state.which_key_overlay = None;
            UiCommand::from_id(command_id.as_str())
                .map(RoutedInput::Command)
                .unwrap_or(RoutedInput::Ignore)
        }
        KeymapLookupResult::Prefix { .. } => {
            shell_state.refresh_which_key_overlay();
            RoutedInput::Ignore
        }
        KeymapLookupResult::InvalidPrefix | KeymapLookupResult::NoMatch => {
            shell_state.which_key_overlay = None;
            RoutedInput::Ignore
        }
    }
}

fn route_terminal_mode_key(shell_state: &UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.terminal_escape_pending {
        if is_ctrl_char(key, 'n') {
            return RoutedInput::Command(UiCommand::EnterNormalMode);
        }
        return RoutedInput::TerminalPassthrough(TerminalPassthrough {
            replay_escape_prefix: true,
            key,
        });
    }

    if is_ctrl_char(key, '\\') {
        return RoutedInput::Command(UiCommand::StartTerminalEscapeChord);
    }

    RoutedInput::TerminalPassthrough(TerminalPassthrough {
        replay_escape_prefix: false,
        key,
    })
}

fn dispatch_command(shell_state: &mut UiShellState, command: UiCommand) -> bool {
    let _command_id = command.id();
    match command {
        UiCommand::EnterNormalMode => {
            shell_state.enter_normal_mode();
            false
        }
        UiCommand::EnterInsertMode => {
            shell_state.enter_insert_mode();
            false
        }
        UiCommand::OpenTerminalForSelected => {
            shell_state.open_terminal_and_enter_mode();
            false
        }
        UiCommand::StartTerminalEscapeChord => {
            shell_state.begin_terminal_escape_chord();
            false
        }
        UiCommand::QuitShell => true,
        UiCommand::FocusNextInbox => {
            shell_state.move_selection(1);
            false
        }
        UiCommand::FocusPreviousInbox => {
            shell_state.move_selection(-1);
            false
        }
        UiCommand::CycleBatchNext => {
            shell_state.cycle_batch(1);
            false
        }
        UiCommand::CycleBatchPrevious => {
            shell_state.cycle_batch(-1);
            false
        }
        UiCommand::JumpFirstInbox => {
            shell_state.jump_to_first_item();
            false
        }
        UiCommand::JumpLastInbox => {
            shell_state.jump_to_last_item();
            false
        }
        UiCommand::JumpBatchDecideOrUnblock => {
            shell_state.jump_to_batch(InboxBatchKind::DecideOrUnblock);
            false
        }
        UiCommand::JumpBatchApprovals => {
            shell_state.jump_to_batch(InboxBatchKind::Approvals);
            false
        }
        UiCommand::JumpBatchReviewReady => {
            shell_state.jump_to_batch(InboxBatchKind::ReviewReady);
            false
        }
        UiCommand::JumpBatchFyiDigest => {
            shell_state.jump_to_batch(InboxBatchKind::FyiDigest);
            false
        }
        UiCommand::OpenFocusCard => {
            shell_state.open_focus_card_for_selected();
            false
        }
        UiCommand::MinimizeCenterView => {
            shell_state.minimize_center_view();
            false
        }
    }
}

fn is_escape_to_normal(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc) && key.modifiers.is_empty() || is_ctrl_char(key, '[')
}

fn is_ctrl_char(key: KeyEvent, ch: char) -> bool {
    matches!(key.code, KeyCode::Char(code_ch) if code_ch == ch)
        && key.modifiers == KeyModifiers::CONTROL
}

#[cfg(test)]
fn command_id(command: UiCommand) -> &'static str {
    command.id()
}

#[cfg(test)]
fn routed_command(route: RoutedInput) -> Option<UiCommand> {
    match route {
        RoutedInput::Command(command) => Some(command),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        ArtifactId, ArtifactKind, ArtifactProjection, InboxItemProjection,
        OrchestrationEventPayload, OrchestrationEventType, SessionCheckpointPayload,
        SessionNeedsInputPayload, SessionProjection, StoredEventEnvelope, WorkItemProjection,
        WorkflowState,
    };

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn sample_projection(with_session: bool) -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_item_id = InboxItemId::new("inbox-1");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: with_session.then_some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![],
            },
        );

        if with_session {
            projection.sessions.insert(
                session_id.clone(),
                SessionProjection {
                    id: session_id,
                    work_item_id: Some(work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
        }

        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Review PR readiness".to_owned(),
                resolved: false,
            },
        );

        projection
    }

    fn triage_projection() -> ProjectionState {
        let mut projection = ProjectionState::default();
        let rows = vec![
            (
                "wi-decision",
                "inbox-decision",
                InboxItemKind::NeedsDecision,
                "Pick API shape",
            ),
            (
                "wi-approval",
                "inbox-approval",
                InboxItemKind::NeedsApproval,
                "Approve PR ready",
            ),
            (
                "wi-review",
                "inbox-review",
                InboxItemKind::ReadyForReview,
                "Review draft PR",
            ),
            ("wi-fyi", "inbox-fyi", InboxItemKind::FYI, "Progress digest"),
        ];

        for (work_item_raw, inbox_item_raw, kind, title) in rows {
            let work_item_id = WorkItemId::new(work_item_raw);
            let inbox_item_id = InboxItemId::new(inbox_item_raw);

            projection.work_items.insert(
                work_item_id.clone(),
                WorkItemProjection {
                    id: work_item_id.clone(),
                    ticket_id: None,
                    project_id: None,
                    workflow_state: Some(WorkflowState::Implementing),
                    session_id: None,
                    worktree_id: None,
                    inbox_items: vec![inbox_item_id.clone()],
                    artifacts: vec![],
                },
            );

            projection.inbox_items.insert(
                inbox_item_id.clone(),
                InboxItemProjection {
                    id: inbox_item_id,
                    work_item_id,
                    kind,
                    title: title.to_owned(),
                    resolved: false,
                },
            );
        }

        projection
    }

    fn focus_card_projection_with_evidence() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-focus");
        let session_id = WorkerSessionId::new("sess-focus");
        let inbox_item_id = InboxItemId::new("inbox-focus");
        let pr_artifact_id = ArtifactId::new("artifact-pr");
        let log_artifact_id = ArtifactId::new("artifact-log");

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
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![pr_artifact_id.clone(), log_artifact_id.clone()],
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: Some(log_artifact_id.clone()),
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id: work_item_id.clone(),
                kind: InboxItemKind::NeedsDecision,
                title: "Choose API shape".to_owned(),
                resolved: false,
            },
        );
        projection.artifacts.insert(
            pr_artifact_id.clone(),
            ArtifactProjection {
                id: pr_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::PR,
                label: "Draft PR".to_owned(),
                uri: "https://github.com/example/repo/pull/7".to_owned(),
            },
        );
        projection.artifacts.insert(
            log_artifact_id.clone(),
            ArtifactProjection {
                id: log_artifact_id.clone(),
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::LogSnippet,
                label: "Failing test tail".to_owned(),
                uri: "artifact://logs/wi-focus".to_owned(),
            },
        );
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-checkpoint".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-16T09:00:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionCheckpoint,
            payload: OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                session_id: session_id.clone(),
                artifact_id: log_artifact_id,
                summary: "Refactored parser and ran targeted tests".to_owned(),
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-needs-input".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-16T09:01:00Z".to_owned(),
            work_item_id: Some(work_item_id),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionNeedsInput,
            payload: OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id,
                prompt: "Choose API shape: A or B".to_owned(),
            }),
            schema_version: 1,
        });

        projection
    }

    fn focus_card_projection_with_multiple_sessions() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-focus-multi");
        let active_session_id = WorkerSessionId::new("sess-active");
        let prior_session_id = WorkerSessionId::new("sess-prior");
        let inbox_item_id = InboxItemId::new("inbox-focus-multi");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: Some(active_session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![],
            },
        );
        projection.sessions.insert(
            active_session_id.clone(),
            SessionProjection {
                id: active_session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: None,
            },
        );
        projection.sessions.insert(
            prior_session_id.clone(),
            SessionProjection {
                id: prior_session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::Done),
                latest_checkpoint: None,
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id: work_item_id.clone(),
                kind: InboxItemKind::NeedsDecision,
                title: "Pick an implementation".to_owned(),
                resolved: false,
            },
        );
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-active-needs-input".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-16T09:00:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(active_session_id.clone()),
            event_type: OrchestrationEventType::SessionNeedsInput,
            payload: OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id: active_session_id,
                prompt: "Active session question".to_owned(),
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-focus-prior-needs-input".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-16T09:01:00Z".to_owned(),
            work_item_id: Some(work_item_id),
            session_id: Some(prior_session_id.clone()),
            event_type: OrchestrationEventType::SessionNeedsInput,
            payload: OrchestrationEventPayload::SessionNeedsInput(SessionNeedsInputPayload {
                session_id: prior_session_id,
                prompt: "Prior session question".to_owned(),
            }),
            schema_version: 1,
        });

        projection
    }

    fn focus_card_projection_with_many_artifacts() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-many-artifacts");
        let inbox_item_id = InboxItemId::new("inbox-many-artifacts");

        let mut projection = ProjectionState::default();
        let mut artifact_ids = Vec::new();
        for index in 0..8 {
            let artifact_id = ArtifactId::new(format!("artifact-{index}"));
            artifact_ids.push(artifact_id.clone());
            projection.artifacts.insert(
                artifact_id.clone(),
                ArtifactProjection {
                    id: artifact_id,
                    work_item_id: work_item_id.clone(),
                    kind: ArtifactKind::LogSnippet,
                    label: format!("Log {index}"),
                    uri: format!("artifact://logs/{index}"),
                },
            );
        }

        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: artifact_ids,
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Review artifact set".to_owned(),
                resolved: false,
            },
        );

        projection
    }

    #[test]
    fn center_stack_replace_push_and_pop_behavior() {
        let mut stack = ViewStack::default();
        assert_eq!(stack.active_center(), Some(&CenterView::InboxView));

        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-1"),
        });
        assert_eq!(stack.center_views().len(), 1);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::FocusCardView { .. })
        ));

        stack.push_center(CenterView::TerminalView {
            session_id: WorkerSessionId::new("sess-1"),
        });
        assert_eq!(stack.center_views().len(), 2);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));

        assert!(stack.pop_center());
        assert_eq!(stack.center_views().len(), 1);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::FocusCardView { .. })
        ));
        assert!(!stack.pop_center());
    }

    #[test]
    fn ui_state_projects_from_domain_and_center_stack() {
        let projection = sample_projection(true);
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-1"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None);
        assert_eq!(ui_state.selected_inbox_index, Some(0));
        assert_eq!(
            ui_state
                .selected_inbox_item_id
                .as_ref()
                .map(|id| id.as_str())
                .expect("selected inbox"),
            "inbox-1"
        );
        assert_eq!(
            ui_state.center_stack_label(),
            "FocusCard(inbox-1)".to_owned()
        );
        assert!(ui_state
            .center_pane
            .lines
            .iter()
            .any(|line| line.contains("Review PR readiness")));
    }

    #[test]
    fn focus_card_projects_action_ready_context_and_evidence() {
        let projection = focus_card_projection_with_evidence();
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-focus"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");

        assert!(rendered.contains("Why attention is required:"));
        assert!(rendered.contains("Recommended response:"));
        assert!(rendered.contains("Evidence:"));
        assert!(rendered.contains("WaitingForUser"));
        assert!(rendered.contains("Answer the worker prompt"));
        assert!(rendered.contains("Choose API shape: A or B"));
        assert!(rendered.contains("https://github.com/example/repo/pull/7"));
        assert!(rendered.contains("artifact://logs/wi-focus"));
        assert!(rendered.contains("Latest checkpoint summary"));
    }

    #[test]
    fn focus_card_prefers_active_session_context_over_prior_session_events() {
        let projection = focus_card_projection_with_multiple_sessions();
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-focus-multi"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");

        assert!(rendered.contains("Active session question"));
        assert!(!rendered.contains("Prior session question"));
    }

    #[test]
    fn focus_card_limits_artifact_evidence_to_recent_entries() {
        let projection = focus_card_projection_with_many_artifacts();
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-many-artifacts"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");
        let artifact_evidence_count = ui_state
            .center_pane
            .lines
            .iter()
            .filter(|line| line.contains("artifact '"))
            .count();

        assert_eq!(artifact_evidence_count, FOCUS_CARD_ARTIFACT_LIMIT);
        assert!(rendered.contains("artifact://logs/7"));
        assert!(!rendered.contains("artifact://logs/0"));
        assert!(rendered.contains("older artifacts not shown"));
    }

    #[test]
    fn open_terminal_pushes_only_with_session_context() {
        let mut without_session = UiShellState::new("ready".to_owned(), sample_projection(false));
        let before = without_session.view_stack.center_views().to_vec();
        without_session.open_terminal_for_selected();
        assert_eq!(without_session.view_stack.center_views(), before.as_slice());

        let mut with_session = UiShellState::new("ready".to_owned(), sample_projection(true));
        with_session.open_terminal_for_selected();
        assert_eq!(with_session.view_stack.center_views().len(), 2);
        assert!(matches!(
            with_session.view_stack.center_views().first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
        assert!(matches!(
            with_session.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));

        with_session.open_terminal_for_selected();
        assert_eq!(with_session.view_stack.center_views().len(), 2);
    }

    #[test]
    fn minimize_after_open_terminal_returns_to_focus_card() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        shell_state.open_terminal_and_enter_mode();
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::TerminalView { session_id }) if session_id.as_str() == "sess-1"
        ));

        shell_state.minimize_center_view();
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert_eq!(shell_state.view_stack.center_views().len(), 1);
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
    }

    #[test]
    fn open_terminal_normalizes_stack_to_focus_and_terminal() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        shell_state
            .view_stack
            .replace_center(CenterView::FocusCardView {
                inbox_item_id: InboxItemId::new("stale-inbox"),
            });
        shell_state
            .view_stack
            .push_center(CenterView::FocusCardView {
                inbox_item_id: InboxItemId::new("inbox-1"),
            });
        assert_eq!(shell_state.view_stack.center_views().len(), 2);

        shell_state.open_terminal_for_selected();
        assert_eq!(shell_state.view_stack.center_views().len(), 2);
        assert!(matches!(
            shell_state.view_stack.center_views().first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::TerminalView { session_id }) if session_id.as_str() == "sess-1"
        ));
    }

    #[test]
    fn projection_prefers_selected_inbox_item_id_over_stale_index() {
        let work_item_a = WorkItemId::new("wi-a");
        let work_item_b = WorkItemId::new("wi-b");
        let inbox_item_a = InboxItemId::new("inbox-a");
        let inbox_item_b = InboxItemId::new("inbox-b");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_a.clone(),
            WorkItemProjection {
                id: work_item_a.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_a.clone()],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            work_item_b.clone(),
            WorkItemProjection {
                id: work_item_b.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_b.clone()],
                artifacts: vec![],
            },
        );
        projection.inbox_items.insert(
            inbox_item_a.clone(),
            InboxItemProjection {
                id: inbox_item_a.clone(),
                work_item_id: work_item_a,
                kind: InboxItemKind::NeedsApproval,
                title: "First".to_owned(),
                resolved: false,
            },
        );
        projection.inbox_items.insert(
            inbox_item_b.clone(),
            InboxItemProjection {
                id: inbox_item_b.clone(),
                work_item_id: work_item_b,
                kind: InboxItemKind::NeedsApproval,
                title: "Second".to_owned(),
                resolved: false,
            },
        );

        let ui_state = project_ui_state(
            "ready",
            &projection,
            &ViewStack::default(),
            Some(0),
            Some(&inbox_item_b),
        );
        assert_eq!(ui_state.selected_inbox_index, Some(1));
        assert_eq!(
            ui_state
                .selected_inbox_item_id
                .as_ref()
                .map(|item| item.as_str()),
            Some("inbox-b")
        );
    }

    #[test]
    fn inbox_view_projects_priority_bands_and_batch_surfaces() {
        let ui_state = project_ui_state(
            "ready",
            &triage_projection(),
            &ViewStack::default(),
            None,
            None,
        );
        let ordered_kinds = ui_state
            .inbox_rows
            .iter()
            .map(|row| row.kind.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_kinds,
            vec![
                InboxItemKind::NeedsDecision,
                InboxItemKind::NeedsApproval,
                InboxItemKind::ReadyForReview,
                InboxItemKind::FYI,
            ]
        );

        assert_eq!(
            ui_state
                .inbox_rows
                .iter()
                .map(|row| row.priority_band)
                .collect::<Vec<_>>(),
            vec![
                InboxPriorityBand::Urgent,
                InboxPriorityBand::Attention,
                InboxPriorityBand::Attention,
                InboxPriorityBand::Background,
            ]
        );

        assert_eq!(ui_state.inbox_batch_surfaces.len(), 4);
        assert_eq!(ui_state.inbox_batch_surfaces[0].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[1].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[2].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[3].unresolved_count, 1);

        let rendered = render_inbox_panel(&ui_state);
        assert!(rendered.contains("Batch lanes:"));
        assert!(rendered.contains("Urgent:"));
        assert!(rendered.contains("Attention:"));
        assert!(rendered.contains("Background:"));
    }

    #[test]
    fn batch_navigation_can_jump_and_cycle_surfaces() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());

        shell_state.jump_to_batch(InboxBatchKind::ReviewReady);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);

        shell_state.cycle_batch(1);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        shell_state.cycle_batch(-1);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);
    }

    #[test]
    fn keyboard_shortcuts_support_batch_and_range_navigation() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        handle_key_press(&mut shell_state, key(KeyCode::Char('g')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, key(KeyCode::Char('G')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        handle_key_press(&mut shell_state, key(KeyCode::BackTab));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);
    }

    #[test]
    fn keymap_prefix_binding_aliases_batch_jumps() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);
    }

    #[test]
    fn which_key_overlay_shows_next_keys_with_descriptions() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        assert!(shell_state.which_key_overlay.is_none());
        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));

        let overlay = shell_state
            .which_key_overlay
            .as_ref()
            .expect("overlay is shown for valid prefix");
        let rendered = render_which_key_overlay_text(overlay);
        assert!(rendered.contains("z  (Batch jumps)"));
        assert!(rendered.contains("1  Jump to Decide/Unblock lane"));
        assert!(rendered.contains("2  Jump to Approvals lane"));
        assert!(rendered.contains("3  Jump to PR Reviews lane"));
        assert!(rendered.contains("4  Jump to FYI Digest lane"));
    }

    #[test]
    fn which_key_overlay_clears_on_completion_invalid_and_cancel() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        assert!(shell_state.which_key_overlay.is_some());

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        assert!(shell_state.which_key_overlay.is_none());

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        assert!(shell_state.which_key_overlay.is_some());
        handle_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(shell_state.which_key_overlay.is_none());
        assert!(shell_state.mode_key_buffer.is_empty());

        handle_key_press(&mut shell_state, key(KeyCode::Char('z')));
        assert!(shell_state.which_key_overlay.is_some());
        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(shell_state.which_key_overlay.is_none());
    }

    #[test]
    fn which_key_overlay_popup_is_anchored_and_clamped() {
        let anchor = Rect {
            x: 10,
            y: 5,
            width: 24,
            height: 8,
        };
        let content = "z  (Batch jumps)\n1  Jump to Decide/Unblock lane\n2  Jump to Approvals lane";
        let popup = which_key_overlay_popup(anchor, content).expect("overlay popup");

        assert!(popup.width <= anchor.width);
        assert!(popup.height <= anchor.height);
        assert_eq!(popup.x + popup.width, anchor.x + anchor.width);
        assert_eq!(popup.y + popup.height, anchor.y + anchor.height);

        let too_narrow = Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 8,
        };
        assert!(which_key_overlay_popup(too_narrow, content).is_none());

        let too_short = Rect {
            x: 0,
            y: 0,
            width: 24,
            height: 2,
        };
        assert!(which_key_overlay_popup(too_short, content).is_none());
    }

    #[test]
    fn which_key_overlay_popup_handles_large_content_without_overflow() {
        let anchor = Rect {
            x: 0,
            y: 0,
            width: 32,
            height: 12,
        };

        let very_wide = "x".repeat(70_000);
        let popup = which_key_overlay_popup(anchor, very_wide.as_str()).expect("wide popup");
        assert_eq!(popup.width, anchor.width);
        assert_eq!(popup.height, 3);

        let very_tall = "xx\n".repeat(70_000);
        let popup = which_key_overlay_popup(anchor, very_tall.as_str()).expect("tall popup");
        assert_eq!(popup.width, 4);
        assert_eq!(popup.height, anchor.height);
    }

    #[test]
    fn keyboard_shortcuts_ignore_control_modified_chars() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('q')));
        assert!(!should_quit);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('j')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);
    }

    #[test]
    fn batch_jump_prefers_unresolved_then_falls_back_to_first_any() {
        let mut projection = ProjectionState::default();
        let resolved_approval = InboxItemId::new("inbox-approval-resolved");
        let unresolved_approval = InboxItemId::new("inbox-approval-unresolved");
        let resolved_work_item_id = WorkItemId::new("wi-approval-resolved");
        let unresolved_work_item_id = WorkItemId::new("wi-approval-unresolved");

        projection.work_items.insert(
            resolved_work_item_id.clone(),
            WorkItemProjection {
                id: resolved_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![resolved_approval.clone()],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            unresolved_work_item_id.clone(),
            WorkItemProjection {
                id: unresolved_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![unresolved_approval.clone()],
                artifacts: vec![],
            },
        );
        projection.inbox_items.insert(
            resolved_approval.clone(),
            InboxItemProjection {
                id: resolved_approval,
                work_item_id: resolved_work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Resolved approval".to_owned(),
                resolved: true,
            },
        );
        projection.inbox_items.insert(
            unresolved_approval.clone(),
            InboxItemProjection {
                id: unresolved_approval.clone(),
                work_item_id: unresolved_work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Unresolved approval".to_owned(),
                resolved: false,
            },
        );

        let mut shell_state = UiShellState::new("ready".to_owned(), projection.clone());
        shell_state.jump_to_batch(InboxBatchKind::Approvals);
        let selected = shell_state.ui_state();
        assert_eq!(
            selected
                .selected_inbox_item_id
                .as_ref()
                .map(|id| id.as_str())
                .expect("selected approval"),
            unresolved_approval.as_str()
        );

        projection
            .inbox_items
            .get_mut(&unresolved_approval)
            .expect("unresolved approval exists")
            .resolved = true;
        let mut shell_state = UiShellState::new("ready".to_owned(), projection);
        shell_state.jump_to_batch(InboxBatchKind::Approvals);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsApproval);
        let selected_title = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .title
            .clone();
        assert!(selected_title.contains("approval"));
    }

    #[test]
    fn set_selection_ignores_out_of_bounds_index() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let rows = shell_state.ui_state().inbox_rows;
        shell_state.set_selection(Some(usize::MAX), &rows);
        assert_eq!(shell_state.selected_inbox_index, None);
        assert_eq!(shell_state.selected_inbox_item_id, None);
    }

    #[test]
    fn mode_commands_have_stable_ids() {
        assert_eq!(command_id(UiCommand::EnterNormalMode), "ui.mode.normal");
        assert_eq!(command_id(UiCommand::EnterInsertMode), "ui.mode.insert");
        assert_eq!(
            command_id(UiCommand::OpenTerminalForSelected),
            command_ids::UI_OPEN_TERMINAL_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::FocusNextInbox),
            command_ids::UI_FOCUS_NEXT_INBOX
        );
    }

    #[test]
    fn command_registry_round_trips_ids() {
        let all_commands = [
            UiCommand::EnterNormalMode,
            UiCommand::EnterInsertMode,
            UiCommand::OpenTerminalForSelected,
            UiCommand::StartTerminalEscapeChord,
            UiCommand::QuitShell,
            UiCommand::FocusNextInbox,
            UiCommand::FocusPreviousInbox,
            UiCommand::CycleBatchNext,
            UiCommand::CycleBatchPrevious,
            UiCommand::JumpFirstInbox,
            UiCommand::JumpLastInbox,
            UiCommand::JumpBatchDecideOrUnblock,
            UiCommand::JumpBatchApprovals,
            UiCommand::JumpBatchReviewReady,
            UiCommand::JumpBatchFyiDigest,
            UiCommand::OpenFocusCard,
            UiCommand::MinimizeCenterView,
        ];

        for command in all_commands {
            let id = command.id();
            assert_eq!(UiCommand::from_id(id), Some(command));
        }
    }

    #[test]
    fn esc_returns_to_normal_without_quitting() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.enter_insert_mode();
        assert_eq!(shell_state.mode, UiMode::Insert);

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn insert_mode_routes_navigation_keys_to_ignore() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.move_selection(2);
        let before_index = shell_state.ui_state().selected_inbox_index;

        let enter_insert = handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert!(!enter_insert);
        assert_eq!(shell_state.mode, UiMode::Insert);

        handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        handle_key_press(&mut shell_state, key(KeyCode::Down));
        let after_index = shell_state.ui_state().selected_inbox_index;
        assert_eq!(before_index, after_index);
    }

    #[test]
    fn insert_mode_is_not_entered_while_terminal_view_is_active() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('t')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(shell_state.is_terminal_view_active());

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert!(shell_state.is_terminal_view_active());

        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn ctrl_left_bracket_returns_to_normal_mode() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.enter_insert_mode();
        assert_eq!(shell_state.mode, UiMode::Insert);

        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('[')));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn terminal_mode_supports_escape_chord_and_passthrough_routing() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('t')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(shell_state.is_terminal_view_active());

        let routed = route_key_press(&mut shell_state, key(KeyCode::Char('j')));
        assert!(matches!(
            routed,
            RoutedInput::TerminalPassthrough(TerminalPassthrough {
                replay_escape_prefix: false,
                ..
            })
        ));

        let start_chord = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('\\')));
        assert!(!start_chord);
        assert!(shell_state.terminal_escape_pending);
        assert_eq!(shell_state.mode, UiMode::Terminal);

        let finish_chord = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('n')));
        assert!(!finish_chord);
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert!(!shell_state.terminal_escape_pending);
    }

    #[test]
    fn terminal_mode_without_terminal_view_recovers_to_normal() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.mode = UiMode::Terminal;
        assert!(!shell_state.is_terminal_view_active());

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsApproval);
    }

    #[test]
    fn terminal_escape_prefix_replays_when_chord_not_completed() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('t')));
        assert_eq!(shell_state.mode, UiMode::Terminal);

        handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('\\')));
        let routed = route_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(matches!(
            routed,
            RoutedInput::TerminalPassthrough(TerminalPassthrough {
                replay_escape_prefix: true,
                key: KeyEvent {
                    code: KeyCode::Char('x'),
                    ..
                }
            })
        ));
    }

    #[test]
    fn normal_mode_router_maps_expected_commands() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('j')))),
            Some(UiCommand::FocusNextInbox)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('i')))),
            Some(UiCommand::EnterInsertMode)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('q')))),
            Some(UiCommand::QuitShell)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Esc))),
            Some(UiCommand::EnterNormalMode)
        );
    }
}
