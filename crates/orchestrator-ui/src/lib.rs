use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use orchestrator_core::{
    attention_inbox_snapshot, command_ids, ArtifactKind, ArtifactProjection, AttentionBatchKind,
    AttentionEngineConfig, AttentionPriorityBand, Command, CommandRegistry, CoreError, InboxItemId,
    InboxItemKind, LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider, LlmRateLimitState,
    LlmResponseStream, LlmRole, LlmTokenUsage, OrchestrationEventPayload, ProjectionState,
    SessionProjection,
    SelectedTicketFlowResult, SupervisorQueryArgs, SupervisorQueryContextArgs, TicketId,
    TicketSummary, UntypedCommandInvocation, ProjectId, WorkItemId, WorkerSessionId,
    WorkerSessionStatus, WorkflowState,
};
use orchestrator_runtime::{
    BackendEvent, BackendKind, BackendOutputEvent, RuntimeError, RuntimeSessionId, SessionHandle, SpawnSpec,
    WorkerBackend,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Terminal;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

mod keymap;

pub use keymap::{
    key_stroke_from_event, KeyBindingConfig, KeyPrefixConfig, KeyStroke, KeymapCompileError,
    KeymapConfig, KeymapLookupResult, KeymapTrie, ModeKeymapConfig,
};

const TICKET_PICKER_EVENT_CHANNEL_CAPACITY: usize = 32;
const TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY: usize = 32;
const TICKET_PICKER_PRIORITY_STATES_ENV: &str = "ORCHESTRATOR_TICKET_PICKER_PRIORITY_STATES";
const TICKET_PICKER_PRIORITY_STATES_DEFAULT: &[&str] =
    &["In Progress", "Final Approval", "Todo", "Backlog"];

#[async_trait]
pub trait TicketPickerProvider: Send + Sync {
    async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError>;
    async fn start_or_resume_ticket(
        &self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) -> Result<SelectedTicketFlowResult, CoreError>;
    async fn reload_projection(&self) -> Result<ProjectionState, CoreError>;
    async fn mark_session_crashed(
        &self,
        _session_id: WorkerSessionId,
        _reason: String,
    ) -> Result<(), CoreError> {
        Ok(())
    }
}

pub type SupervisorCommandContext = SupervisorQueryContextArgs;

#[async_trait]
pub trait SupervisorCommandDispatcher: Send + Sync {
    async fn dispatch_supervisor_command(
        &self,
        invocation: UntypedCommandInvocation,
        context: SupervisorCommandContext,
    ) -> Result<(String, LlmResponseStream), CoreError>;

    async fn cancel_supervisor_command(&self, stream_id: &str) -> Result<(), CoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CenterView {
    InboxView,
    FocusCardView {
        inbox_item_id: InboxItemId,
    },
    TerminalView {
        session_id: WorkerSessionId,
    },
    InspectorView {
        work_item_id: WorkItemId,
        inspector: ArtifactInspectorKind,
    },
    SupervisorChatView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactInspectorKind {
    Diff,
    Test,
    PullRequest,
    Chat,
}

impl ArtifactInspectorKind {
    fn stack_label(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Test => "test",
            Self::PullRequest => "pr",
            Self::Chat => "chat",
        }
    }

    fn pane_title(self) -> &'static str {
        match self {
            Self::Diff => "Diff Inspector",
            Self::Test => "Test Inspector",
            Self::PullRequest => "PR Inspector",
            Self::Chat => "Chat Inspector",
        }
    }
}

impl CenterView {
    fn label(&self) -> String {
        match self {
            Self::InboxView => "Inbox".to_owned(),
            Self::FocusCardView { inbox_item_id } => {
                format!("FocusCard({})", inbox_item_id.as_str())
            }
            Self::TerminalView { session_id } => format!("Terminal({})", session_id.as_str()),
            Self::InspectorView {
                work_item_id,
                inspector,
            } => format!(
                "Inspector({}:{})",
                inspector.stack_label(),
                work_item_id.as_str()
            ),
            Self::SupervisorChatView => "SupervisorChat".to_owned(),
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

type InboxPriorityBand = AttentionPriorityBand;
type InboxBatchKind = AttentionBatchKind;

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

pub(crate) fn project_ui_state(
    status: &str,
    domain: &ProjectionState,
    view_stack: &ViewStack,
    preferred_selected_inbox: Option<usize>,
    preferred_selected_inbox_item_id: Option<&InboxItemId>,
    terminal_view_state: Option<&TerminalViewState>,
) -> UiState {
    let attention_snapshot =
        attention_inbox_snapshot(domain, &AttentionEngineConfig::default(), &[]);
    let inbox_rows = attention_snapshot
        .items
        .into_iter()
        .map(|item| UiInboxRow {
            inbox_item_id: item.inbox_item_id,
            work_item_id: item.work_item_id,
            kind: item.kind,
            priority_score: item.priority_score,
            priority_band: item.priority_band,
            batch_kind: item.batch_kind,
            title: item.title,
            resolved: item.resolved,
            workflow_state: item.workflow_state,
            session_id: item.session_id,
            session_status: item.session_status,
        })
        .collect::<Vec<_>>();
    let inbox_batch_surfaces = attention_snapshot
        .batch_surfaces
        .into_iter()
        .map(|surface| UiBatchSurface {
            kind: surface.kind,
            unresolved_count: surface.unresolved_count,
            total_count: surface.total_count,
            first_unresolved_index: surface.first_unresolved_index,
            first_any_index: surface.first_any_index,
        })
        .collect::<Vec<_>>();

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
        project_center_pane(&active_center, &inbox_rows, &inbox_batch_surfaces, domain, terminal_view_state);

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

fn project_center_pane(
    active_center: &CenterView,
    inbox_rows: &[UiInboxRow],
    inbox_batch_surfaces: &[UiBatchSurface],
    domain: &ProjectionState,
    terminal_view_state: Option<&TerminalViewState>,
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
            lines.push("c: toggle global supervisor chat".to_owned());
            lines.push("t: open terminal for selected item".to_owned());
            lines.push("v d/t/p/c: open diff/test/PR/chat inspector".to_owned());
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
            let mut lines = Vec::new();

            match terminal_view_state {
                Some(terminal_state) => {
                    if let Some(error) = terminal_state.error.as_deref() {
                        lines.push(format!(
                            "[stream error] {}",
                            compact_focus_card_text(error)
                        ));
                        lines.push(String::new());
                    }
                    if terminal_state.lines.is_empty() && terminal_state.output_fragment.is_empty() {
                        lines.push("No terminal output available yet.".to_owned());
                    } else {
                        lines.extend(terminal_state.lines.iter().cloned());
                        if !terminal_state.output_fragment.is_empty() {
                            lines.push(terminal_state.output_fragment.clone());
                        }
                    }
                }
                None => {
                    lines.push("No terminal output available yet.".to_owned());
                }
            }

            CenterPaneState {
                title: format!("Terminal {}", session_id.as_str()),
                lines,
            }
        }
        CenterView::InspectorView {
            work_item_id,
            inspector,
        } => project_artifact_inspector_pane(*inspector, work_item_id, domain),
        CenterView::SupervisorChatView => CenterPaneState {
            title: "Supervisor Chat".to_owned(),
            lines: vec![
                "Global supervisor Q&A (standalone from selected ticket).".to_owned(),
                "Insert mode: type a question and press Enter to send.".to_owned(),
                "Esc: return to Normal mode.".to_owned(),
                "c: close chat panel and restore previous context.".to_owned(),
                "Ctrl-c: cancel active supervisor stream.".to_owned(),
            ],
        },
    }
}

const INSPECTOR_ARTIFACT_LIMIT: usize = 5;
const INSPECTOR_CHAT_EVENT_LIMIT: usize = 8;
const INSPECTOR_EVENT_SCAN_LIMIT: usize = 512;
const SUPERVISOR_STREAM_CHANNEL_CAPACITY: usize = 128;
const SUPERVISOR_STREAM_MAX_TRANSCRIPT_CHARS: usize = 24_000;
const SUPERVISOR_STREAM_RENDER_LINE_LIMIT: usize = 80;
const SUPERVISOR_STREAM_HIGH_COST_TOTAL_TOKENS: u32 = 900;
const SUPERVISOR_STREAM_LOW_TOKEN_HEADROOM: u32 = 120;
const DEFAULT_SUPERVISOR_MODEL: &str = "openai/gpt-4o-mini";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorStreamLifecycle {
    Connecting,
    Streaming,
    Cancelling,
    Completed,
    Cancelled,
    Error,
}

impl SupervisorStreamLifecycle {
    fn label(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Streaming => "streaming",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Connecting | Self::Streaming | Self::Cancelling)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorResponseState {
    Nominal,
    NoContext,
    AuthUnavailable,
    BackendUnavailable,
    RateLimited,
    HighCost,
}

impl SupervisorResponseState {
    fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::NoContext => "no-context",
            Self::AuthUnavailable => "auth-unavailable",
            Self::BackendUnavailable => "backend-unavailable",
            Self::RateLimited => "rate-limited",
            Self::HighCost => "high-cost",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TerminalViewState {
    lines: Vec<String>,
    error: Option<String>,
    output_fragment: String,
    workflow_stage: TerminalWorkflowStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TerminalWorkflowStage {
    #[default]
    Planning,
    Implementation,
    Review,
    Complete,
}

impl TerminalWorkflowStage {
    fn advance_instruction(self) -> Option<(Self, &'static str)> {
        match self {
            Self::Planning => Some((
                Self::Implementation,
                "Workflow transition approved: Planning -> Implementation. End planning mode and begin implementation in this worktree now.",
            )),
            Self::Implementation => Some((
                Self::Review,
                "Workflow transition approved: Implementation -> Review. Pause implementation and provide a review-ready summary with evidence, tests, and open risks.",
            )),
            Self::Review => Some((
                Self::Complete,
                "Workflow transition approved: Review -> Complete. Finalize the session with a completion summary, verification status, and remaining follow-ups.",
            )),
            Self::Complete => None,
        }
    }
}

#[derive(Debug)]
enum TerminalSessionEvent {
    Output {
        session_id: WorkerSessionId,
        output: BackendOutputEvent,
    },
    StreamFailed {
        session_id: WorkerSessionId,
        error: RuntimeError,
    },
    StreamEnded {
        session_id: WorkerSessionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SupervisorStreamEvent {
    Started {
        stream_id: String,
    },
    Delta {
        text: String,
    },
    RateLimit {
        state: LlmRateLimitState,
    },
    Usage {
        usage: LlmTokenUsage,
    },
    Finished {
        reason: LlmFinishReason,
        usage: Option<LlmTokenUsage>,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SupervisorStreamTarget {
    Inspector { work_item_id: WorkItemId },
    GlobalChatPanel,
}

#[derive(Debug)]
struct ActiveSupervisorChatStream {
    target: SupervisorStreamTarget,
    receiver: mpsc::Receiver<SupervisorStreamEvent>,
    stream_id: Option<String>,
    lifecycle: SupervisorStreamLifecycle,
    response_state: SupervisorResponseState,
    transcript: String,
    pending_delta: String,
    pending_chunk_count: usize,
    last_flush_coalesced_chunks: usize,
    last_rate_limit: Option<LlmRateLimitState>,
    usage: Option<LlmTokenUsage>,
    error_message: Option<String>,
    state_message: Option<String>,
    cooldown_hint: Option<String>,
    pending_cancel: bool,
}

impl ActiveSupervisorChatStream {
    fn new(
        target: SupervisorStreamTarget,
        receiver: mpsc::Receiver<SupervisorStreamEvent>,
    ) -> Self {
        Self {
            target,
            receiver,
            stream_id: None,
            lifecycle: SupervisorStreamLifecycle::Connecting,
            response_state: SupervisorResponseState::Nominal,
            transcript: String::new(),
            pending_delta: String::new(),
            pending_chunk_count: 0,
            last_flush_coalesced_chunks: 0,
            last_rate_limit: None,
            usage: None,
            error_message: None,
            state_message: None,
            cooldown_hint: None,
            pending_cancel: false,
        }
    }

    fn terminal_state(
        target: SupervisorStreamTarget,
        response_state: SupervisorResponseState,
        message: impl Into<String>,
    ) -> Self {
        let (_sender, receiver) = mpsc::channel(1);
        let mut stream = Self::new(target, receiver);
        stream.lifecycle = SupervisorStreamLifecycle::Error;
        stream.response_state = response_state;
        stream.state_message = Some(supervisor_state_message(response_state).to_owned());
        stream.error_message = Some(message.into());
        stream
    }

    fn set_response_state(
        &mut self,
        state: SupervisorResponseState,
        state_message: Option<String>,
        cooldown_hint: Option<String>,
    ) {
        self.response_state = state;
        self.state_message = state_message;
        self.cooldown_hint = cooldown_hint;
    }

    fn flush_pending_delta(&mut self) {
        if self.pending_delta.is_empty() {
            self.last_flush_coalesced_chunks = 0;
            return;
        }

        self.last_flush_coalesced_chunks = self.pending_chunk_count.max(1);
        self.transcript.push_str(&self.pending_delta);
        self.pending_delta.clear();
        self.pending_chunk_count = 0;
        trim_to_trailing_chars(&mut self.transcript, SUPERVISOR_STREAM_MAX_TRANSCRIPT_CHARS);
    }

    fn render_lines(&self) -> Vec<String> {
        let mut lines = vec![
            String::new(),
            "Live supervisor stream:".to_owned(),
            format!("- State: {}", self.lifecycle.label()),
            format!("- Supervisor state: {}", self.response_state.label()),
        ];

        if self.last_flush_coalesced_chunks > 1 {
            lines.push(format!(
                "- Backpressure: coalesced {} chunks in last draw tick.",
                self.last_flush_coalesced_chunks
            ));
        }
        if !self.pending_delta.is_empty() {
            lines.push(format!(
                "- Buffering: {} chars across {} chunks awaiting next draw tick.",
                self.pending_delta.chars().count(),
                self.pending_chunk_count.max(1)
            ));
        }
        if let Some(rate_limit) = self.last_rate_limit.as_ref() {
            lines.push(format!(
                "- Rate limit: requests={} tokens={}",
                rate_limit
                    .requests_remaining
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                rate_limit
                    .tokens_remaining
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            ));
            if let Some(reset_at) = rate_limit.reset_at.as_deref() {
                lines.push(format!("- Rate limit reset: {reset_at}"));
            }
        }
        if let Some(usage) = self.usage.as_ref() {
            lines.push(format!(
                "- Token usage: input={} output={} total={}",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            ));
        }
        if let Some(error_message) = self.error_message.as_deref() {
            lines.push(format!(
                "- Error: {}",
                compact_focus_card_text(error_message)
            ));
        }
        if let Some(state_message) = self.state_message.as_deref() {
            lines.push(format!(
                "- Guidance: {}",
                compact_focus_card_text(state_message)
            ));
        }
        if let Some(cooldown_hint) = self.cooldown_hint.as_deref() {
            lines.push(format!("- Cooldown: {cooldown_hint}"));
        }
        if self.response_state != SupervisorResponseState::Nominal {
            lines.extend(self.response_state_guidance_lines());
        }

        lines.push(String::new());
        lines.push("Streaming response:".to_owned());

        if self.transcript.trim().is_empty() {
            lines.push("- Waiting for streamed output...".to_owned());
            return lines;
        }

        let transcript_lines = self
            .transcript
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let total_lines = transcript_lines.len();
        let start = total_lines.saturating_sub(SUPERVISOR_STREAM_RENDER_LINE_LIMIT);
        for line in transcript_lines.iter().skip(start) {
            lines.push(format!("  {line}"));
        }
        if total_lines > SUPERVISOR_STREAM_RENDER_LINE_LIMIT {
            lines.push(format!(
                "  ... {} older lines omitted",
                total_lines - SUPERVISOR_STREAM_RENDER_LINE_LIMIT
            ));
        }

        lines
    }

    fn response_state_guidance_lines(&self) -> Vec<String> {
        let mut lines = vec!["- Retry guidance:".to_owned()];
        match self.response_state {
            SupervisorResponseState::Nominal => return Vec::new(),
            SupervisorResponseState::NoContext => {
                lines.push(
                    "  Select a work item/session or submit a non-empty global question."
                        .to_owned(),
                );
            }
            SupervisorResponseState::AuthUnavailable => {
                lines.push("  Verify OpenRouter credentials and re-run the query.".to_owned());
            }
            SupervisorResponseState::BackendUnavailable => {
                lines.push("  Check network/runtime availability, then retry.".to_owned());
            }
            SupervisorResponseState::RateLimited => {
                lines.push("  Wait for cooldown and retry with narrower context.".to_owned());
            }
            SupervisorResponseState::HighCost => {
                lines.push(
                    "  Use a tighter scope and fewer freeform questions to reduce token usage."
                        .to_owned(),
                );
            }
        }

        lines.push("- Safe fallback prompts:".to_owned());
        for prompt in self.fallback_prompts() {
            lines.push(format!("  {prompt}"));
        }
        lines
    }

    fn fallback_prompts(&self) -> [&'static str; 3] {
        match &self.target {
            SupervisorStreamTarget::Inspector { .. } => [
                "What is the current status of this session?",
                "What is blocking this ticket?",
                "Freeform `What needs me now?`",
            ],
            SupervisorStreamTarget::GlobalChatPanel => [
                "Freeform `What needs my attention globally?`",
                "Freeform `What changed in the last 30 minutes?`",
                "What should I do next?",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct GlobalSupervisorChatReturnContext {
    selected_inbox_index: Option<usize>,
    selected_inbox_item_id: Option<InboxItemId>,
}

fn trim_to_trailing_chars(text: &mut String, max_chars: usize) {
    if text.chars().count() <= max_chars {
        return;
    }

    let trimmed = text
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    *text = trimmed;
}

fn project_artifact_inspector_pane(
    inspector: ArtifactInspectorKind,
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> CenterPaneState {
    let mut lines = vec![format!("Work item: {}", work_item_id.as_str())];
    match inspector {
        ArtifactInspectorKind::Diff => {
            lines.extend(project_diff_inspector_lines(work_item_id, domain));
        }
        ArtifactInspectorKind::Test => {
            lines.extend(project_test_inspector_lines(work_item_id, domain));
        }
        ArtifactInspectorKind::PullRequest => {
            lines.extend(project_pr_inspector_lines(work_item_id, domain));
        }
        ArtifactInspectorKind::Chat => {
            lines.extend(project_chat_inspector_lines(work_item_id, domain));
        }
    }
    lines.push(String::new());
    lines.push("Backspace: minimize to previous view".to_owned());

    CenterPaneState {
        title: format!("{} {}", inspector.pane_title(), work_item_id.as_str()),
        lines,
    }
}

fn project_diff_inspector_lines(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Vec<String> {
    let mut lines = vec!["Diff artifacts:".to_owned()];
    let artifacts =
        collect_work_item_artifacts(work_item_id, domain, INSPECTOR_ARTIFACT_LIMIT, |artifact| {
            artifact.kind == ArtifactKind::Diff
        });

    if artifacts.is_empty() {
        lines.push("- No diff artifacts available yet.".to_owned());
        return lines;
    }

    for artifact in artifacts {
        lines.push(format!("- {} -> {}", artifact.label, artifact.uri));
        if let Some(diffstat) = diffstat_summary_line(artifact) {
            lines.push(format!("  {diffstat}"));
        }
    }

    lines
}

fn project_test_inspector_lines(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Vec<String> {
    let mut lines = vec!["Test artifacts:".to_owned()];
    let artifacts = collect_work_item_artifacts(
        work_item_id,
        domain,
        INSPECTOR_ARTIFACT_LIMIT,
        is_test_artifact,
    );

    if artifacts.is_empty() {
        lines.push("- No test artifacts available yet.".to_owned());
    } else {
        for artifact in artifacts {
            lines.push(format!("- {} -> {}", artifact.label, artifact.uri));
            if let Some(tail) = test_tail_summary_line(artifact) {
                lines.push(format!("  {tail}"));
            }
        }
    }

    if let Some(summary) = latest_checkpoint_summary_for_work_item(work_item_id, domain) {
        lines.push(format!("Latest checkpoint summary: {summary}"));
    }
    if let Some(reason) = latest_blocked_reason_for_work_item(work_item_id, domain) {
        lines.push(format!("Latest blocker reason: {reason}"));
    }

    lines
}

fn project_pr_inspector_lines(work_item_id: &WorkItemId, domain: &ProjectionState) -> Vec<String> {
    let mut lines = vec!["PR artifacts:".to_owned()];
    let artifacts = collect_work_item_artifacts(
        work_item_id,
        domain,
        INSPECTOR_ARTIFACT_LIMIT,
        is_pr_artifact,
    );

    if artifacts.is_empty() {
        lines.push("- No PR artifacts available yet.".to_owned());
        return lines;
    }

    for artifact in artifacts {
        lines.push(format!("- {} -> {}", artifact.label, artifact.uri));
        if let Some(metadata) = pr_metadata_summary_line(artifact) {
            lines.push(format!("  {metadata}"));
        }
    }

    lines
}

fn project_chat_inspector_lines(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Vec<String> {
    let mut lines = vec!["Supervisor output:".to_owned()];
    let event_lines = collect_chat_event_lines(work_item_id, domain);
    if event_lines.is_empty() {
        lines.push("- No supervisor transcript events captured yet.".to_owned());
    } else {
        lines.extend(event_lines.into_iter().map(|line| format!("- {line}")));
    }
    if let Some(summary) = latest_supervisor_query_metrics_line(work_item_id, domain) {
        lines.push(format!("- {summary}"));
    }

    lines.push(String::new());
    lines.push("Chat artifacts:".to_owned());
    let artifacts = collect_work_item_artifacts(
        work_item_id,
        domain,
        INSPECTOR_ARTIFACT_LIMIT,
        is_chat_artifact,
    );
    if artifacts.is_empty() {
        lines.push("- No chat artifacts available yet.".to_owned());
    } else {
        lines.extend(
            artifacts
                .iter()
                .map(|artifact| format!("- {} -> {}", artifact.label, artifact.uri)),
        );
    }

    lines
}

fn collect_work_item_artifacts<'a, F>(
    work_item_id: &WorkItemId,
    domain: &'a ProjectionState,
    limit: usize,
    predicate: F,
) -> Vec<&'a ArtifactProjection>
where
    F: Fn(&ArtifactProjection) -> bool,
{
    let Some(work_item) = domain.work_items.get(work_item_id) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut artifacts = Vec::new();
    for artifact_id in work_item.artifacts.iter().rev() {
        if !seen.insert(artifact_id.clone()) {
            continue;
        }
        let Some(artifact) = domain.artifacts.get(artifact_id) else {
            continue;
        };
        if artifact.work_item_id == *work_item_id && predicate(artifact) {
            artifacts.push(artifact);
            if artifacts.len() >= limit {
                break;
            }
        }
    }

    artifacts
}

fn diffstat_summary_line(artifact: &ArtifactProjection) -> Option<String> {
    let files = first_uri_param(artifact.uri.as_str(), &["files", "changed"]);
    let additions = first_uri_param(artifact.uri.as_str(), &["insertions", "added", "adds"]);
    let deletions = first_uri_param(artifact.uri.as_str(), &["deletions", "removed", "dels"]);

    if files.is_some() || additions.is_some() || deletions.is_some() {
        return Some(format!(
            "Diffstat: {} files changed, +{}/-{}",
            files.as_deref().unwrap_or("unknown"),
            additions.as_deref().unwrap_or("0"),
            deletions.as_deref().unwrap_or("0")
        ));
    }

    let label = compact_focus_card_text(artifact.label.as_str());
    if looks_like_diffstat(label.as_str()) {
        return Some(format!("Diffstat: {label}"));
    }

    None
}

fn test_tail_summary_line(artifact: &ArtifactProjection) -> Option<String> {
    let tail = first_uri_param(artifact.uri.as_str(), &["tail", "snippet", "summary"])?;
    Some(format!(
        "Latest test tail: {}",
        compact_focus_card_text(tail.as_str())
    ))
}

fn pr_metadata_summary_line(artifact: &ArtifactProjection) -> Option<String> {
    let uri = artifact.uri.as_str();
    let pull_index = uri.find("/pull/")?;
    let after_pull = &uri[pull_index + "/pull/".len()..];
    let pr_number = after_pull
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if pr_number.is_empty() {
        return None;
    }

    let repo_segment = uri
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(uri)
        .split('?')
        .next()
        .unwrap_or(uri);
    let draft = first_uri_param(uri, &["draft"]);
    let draft_suffix = if draft.as_deref().is_some_and(is_truthy) {
        " (draft)"
    } else {
        ""
    };

    Some(format!(
        "PR metadata: #{pr_number}{draft_suffix} from {repo_segment}"
    ))
}

fn first_uri_param(uri: &str, keys: &[&str]) -> Option<String> {
    let (_, query) = uri.split_once('?')?;
    for pair in query.split('&') {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        if keys.iter().any(|key| raw_key.eq_ignore_ascii_case(key)) {
            return Some(decode_query_component(raw_value));
        }
    }
    None
}

fn decode_query_component(raw: &str) -> String {
    let mut decoded = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hi = decode_hex_nibble(bytes[index + 1]);
                let lo = decode_hex_nibble(bytes[index + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    decoded.push((hi << 4) | lo);
                    index += 3;
                    continue;
                }
                decoded.push(bytes[index]);
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(decoded.as_slice()).into_owned()
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_truthy(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

fn looks_like_diffstat(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("files changed")
        || (lower.contains("insertion") && lower.contains("deletion"))
        || (lower.contains('+') && lower.contains('-') && lower.contains("file"))
}

fn is_test_artifact(artifact: &ArtifactProjection) -> bool {
    if artifact.kind == ArtifactKind::TestRun {
        return true;
    }
    if artifact.kind != ArtifactKind::LogSnippet && artifact.kind != ArtifactKind::Link {
        return false;
    }

    contains_any_case_insensitive(
        artifact.label.as_str(),
        &["test", "pytest", "cargo test", "failing", "junit"],
    ) || contains_any_case_insensitive(artifact.uri.as_str(), &["test", "junit", "report", "tail"])
}

fn is_pr_artifact(artifact: &ArtifactProjection) -> bool {
    artifact.kind == ArtifactKind::PR
        || (artifact.kind == ArtifactKind::Link
            && contains_any_case_insensitive(
                artifact.uri.as_str(),
                &["/pull/", "pullrequest", "pull-request"],
            ))
}

fn is_chat_artifact(artifact: &ArtifactProjection) -> bool {
    if artifact.kind == ArtifactKind::Export {
        return true;
    }

    contains_any_case_insensitive(artifact.label.as_str(), &["supervisor", "chat", "response"])
        || contains_any_case_insensitive(
            artifact.uri.as_str(),
            &["supervisor", "chat", "conversation", "assistant"],
        )
}

fn contains_any_case_insensitive(haystack: &str, needles: &[&str]) -> bool {
    let haystack_lower = haystack.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| haystack_lower.contains(&needle.to_ascii_lowercase()))
}

fn collect_chat_event_lines(work_item_id: &WorkItemId, domain: &ProjectionState) -> Vec<String> {
    let mut lines = Vec::new();
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }

        let message = match &event.payload {
            OrchestrationEventPayload::SessionNeedsInput(payload) => Some(format!(
                "worker: {}",
                compact_focus_card_text(payload.prompt.as_str())
            )),
            OrchestrationEventPayload::UserResponded(payload) => Some(format!(
                "you: {}",
                compact_focus_card_text(payload.message.as_str())
            )),
            OrchestrationEventPayload::SessionCheckpoint(payload) => Some(format!(
                "worker checkpoint: {}",
                compact_focus_card_text(payload.summary.as_str())
            )),
            OrchestrationEventPayload::SessionBlocked(payload) => Some(format!(
                "worker blocked: {}",
                compact_focus_card_text(payload.reason.as_str())
            )),
            OrchestrationEventPayload::SupervisorQueryStarted(payload) => {
                let descriptor = payload
                    .template
                    .as_deref()
                    .map(|template| format!("template={template}"))
                    .or_else(|| payload.query.as_deref().map(|_| "freeform".to_owned()))
                    .unwrap_or_else(|| "unknown".to_owned());
                Some(format!(
                    "supervisor query started: {descriptor} ({})",
                    payload.query_id
                ))
            }
            OrchestrationEventPayload::SupervisorQueryCancelled(payload) => Some(format!(
                "supervisor query cancel requested: {:?} ({})",
                payload.source, payload.query_id
            )),
            OrchestrationEventPayload::SupervisorQueryFinished(payload) => Some(format!(
                "supervisor query finished {:?}: {}ms, {} chunks, {} chars",
                payload.finish_reason,
                payload.duration_ms,
                payload.chunk_count,
                payload.output_chars
            )),
            _ => None,
        };

        if let Some(message) = message {
            lines.push(format!("{} | {message}", event.occurred_at));
        }
        if lines.len() >= INSPECTOR_CHAT_EVENT_LIMIT {
            break;
        }
    }

    lines.reverse();
    lines
}

fn latest_supervisor_query_metrics_line(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Option<String> {
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }

        let OrchestrationEventPayload::SupervisorQueryFinished(payload) = &event.payload else {
            continue;
        };

        let usage = payload
            .usage
            .as_ref()
            .map(|usage| {
                format!(
                    " usage(input={} output={} total={})",
                    usage.input_tokens, usage.output_tokens, usage.total_tokens
                )
            })
            .unwrap_or_default();
        let cancellation = payload
            .cancellation_source
            .as_ref()
            .map(|source| format!(" cancellation={source:?}"))
            .unwrap_or_default();

        return Some(format!(
            "Latest query metrics: id={} reason={:?} duration={}ms chunks={} chars={}{}{}",
            payload.query_id,
            payload.finish_reason,
            payload.duration_ms,
            payload.chunk_count,
            payload.output_chars,
            usage,
            cancellation
        ));
    }

    None
}

fn build_supervisor_chat_request(
    selected_row: &UiInboxRow,
    domain: &ProjectionState,
) -> LlmChatRequest {
    let workflow = selected_row
        .workflow_state
        .as_ref()
        .map(|state| format!("{state:?}"))
        .unwrap_or_else(|| "Unknown".to_owned());
    let session = match (&selected_row.session_id, &selected_row.session_status) {
        (Some(session_id), Some(status)) => format!("{} ({status:?})", session_id.as_str()),
        (Some(session_id), None) => format!("{} (Unknown)", session_id.as_str()),
        (None, _) => "None".to_owned(),
    };
    let chat_event_lines = collect_chat_event_lines(&selected_row.work_item_id, domain);
    let mut prompt_lines = vec![
        "You are answering for the orchestrator supervisor chat pane.".to_owned(),
        "Use only the supplied context. If uncertain, say Unknown.".to_owned(),
        String::new(),
        format!("Work item: {}", selected_row.work_item_id.as_str()),
        format!("Inbox title: {}", selected_row.title),
        format!("Inbox kind: {:?}", selected_row.kind),
        format!("Workflow: {workflow}"),
        format!("Session: {session}"),
        String::new(),
        "Recent transcript events:".to_owned(),
    ];
    if chat_event_lines.is_empty() {
        prompt_lines.push("- none".to_owned());
    } else {
        prompt_lines.extend(chat_event_lines.into_iter().map(|line| format!("- {line}")));
    }
    prompt_lines.push(String::new());
    prompt_lines.push("Respond with:".to_owned());
    prompt_lines.push("- Current activity (1-2 bullets)".to_owned());
    prompt_lines.push("- What needs me now (ordered bullets)".to_owned());
    prompt_lines.push("- Recommended response to send to worker (single message)".to_owned());

    LlmChatRequest {
        model: supervisor_model_from_env(),
        tools: Vec::new(),
        messages: vec![
            LlmMessage {
                role: LlmRole::System,
                content: "You are the orchestrator supervisor. Keep responses terse, operational, and grounded in provided context."
                    .to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: prompt_lines.join("\n"),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
        temperature: Some(0.2),
        tool_choice: None,
        max_output_tokens: Some(700),
    }
}

fn build_global_supervisor_chat_request(query: &str) -> LlmChatRequest {
    LlmChatRequest {
        model: supervisor_model_from_env(),
        tools: Vec::new(),
        messages: vec![
            LlmMessage {
                role: LlmRole::System,
                content: "You are the orchestrator supervisor. Answer concisely and operationally. If information is missing, say Unknown."
                    .to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: query.to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
        temperature: Some(0.2),
        tool_choice: None,
        max_output_tokens: Some(700),
    }
}

fn supervisor_model_from_env() -> String {
    std::env::var("ORCHESTRATOR_SUPERVISOR_MODEL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SUPERVISOR_MODEL.to_owned())
}

fn classify_supervisor_stream_error(message: &str) -> SupervisorResponseState {
    let lower = message.to_ascii_lowercase();
    if lower.contains("429") || lower.contains("rate limit") {
        SupervisorResponseState::RateLimited
    } else if lower.contains("missing selected item")
        || lower.contains("select an inbox item")
        || lower.contains("non-empty question")
        || lower.contains("malformed context")
    {
        SupervisorResponseState::NoContext
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("auth")
        || lower.contains("api key")
    {
        SupervisorResponseState::AuthUnavailable
    } else if lower.contains("network")
        || lower.contains("timeout")
        || lower.contains("connect")
        || lower.contains("transport")
        || lower.contains("dns")
        || lower.contains("tls")
        || lower.contains("no llm provider")
        || lower.contains("tokio runtime")
        || lower.contains("dependency unavailable")
    {
        SupervisorResponseState::BackendUnavailable
    } else {
        SupervisorResponseState::BackendUnavailable
    }
}

fn response_state_warning_label(state: SupervisorResponseState) -> &'static str {
    match state {
        SupervisorResponseState::Nominal => "supervisor",
        SupervisorResponseState::NoContext => "no-context",
        SupervisorResponseState::AuthUnavailable => "auth",
        SupervisorResponseState::BackendUnavailable => "backend",
        SupervisorResponseState::RateLimited => "rate-limit",
        SupervisorResponseState::HighCost => "high-cost",
    }
}

fn supervisor_state_message(state: SupervisorResponseState) -> &'static str {
    match state {
        SupervisorResponseState::Nominal => "Supervisor stream is healthy.",
        SupervisorResponseState::NoContext => {
            "No usable context was available for this supervisor request."
        }
        SupervisorResponseState::AuthUnavailable => {
            "Supervisor authentication failed. Check configured API credentials."
        }
        SupervisorResponseState::BackendUnavailable => {
            "Supervisor backend is currently unavailable."
        }
        SupervisorResponseState::RateLimited => "Supervisor is rate-limited. Retry after cooldown.",
        SupervisorResponseState::HighCost => {
            "Supervisor response cost is high. Use a tighter scope or narrower questions."
        }
    }
}

fn parse_rate_limit_cooldown_hint(message: &str) -> Option<String> {
    let lowered = message.to_ascii_lowercase();
    if let Some(index) = lowered.find("rate limit reset at ") {
        let suffix = &message[index + "rate limit reset at ".len()..];
        let reset_at = suffix
            .split('.')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('`');
        if !reset_at.is_empty() {
            return Some(format!("rate limit reset at {reset_at}"));
        }
    }

    if let Some(index) = lowered.find("retry after ") {
        let suffix = &message[index + "retry after ".len()..];
        let cooldown = suffix
            .split('.')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('`');
        if !cooldown.is_empty() {
            return Some(format!("retry after {cooldown}"));
        }
    }
    None
}

fn usage_trips_high_cost_state(usage: &LlmTokenUsage) -> bool {
    usage.total_tokens >= SUPERVISOR_STREAM_HIGH_COST_TOTAL_TOKENS
}

fn latest_checkpoint_summary_for_work_item(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Option<String> {
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }
        if let OrchestrationEventPayload::SessionCheckpoint(payload) = &event.payload {
            return Some(compact_focus_card_text(payload.summary.as_str()));
        }
    }
    None
}

fn latest_blocked_reason_for_work_item(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Option<String> {
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }
        if let OrchestrationEventPayload::SessionBlocked(payload) = &event.payload {
            return Some(compact_focus_card_text(payload.reason.as_str()));
        }
    }
    None
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
        lines.push(String::new());
        lines.push("Shortcuts: t terminal | v d diff | v t tests | v p PR | v c chat".to_owned());
        lines.push("Backspace: minimize top view".to_owned());
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TicketStatusGroup {
    status: String,
    tickets: Vec<TicketSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TicketProjectGroup {
    project: String,
    collapsed: bool,
    status_groups: Vec<TicketStatusGroup>,
}

impl TicketProjectGroup {
    fn ticket_count(&self) -> usize {
        self.status_groups
            .iter()
            .map(|status_group| status_group.tickets.len())
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TicketPickerRowRef {
    ProjectHeader {
        project_index: usize,
    },
    Ticket {
        project_index: usize,
        status_index: usize,
        ticket_index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TicketPickerOverlayState {
    visible: bool,
    loading: bool,
    starting_ticket_id: Option<TicketId>,
    error: Option<String>,
    project_groups: Vec<TicketProjectGroup>,
    ticket_rows: Vec<TicketPickerRowRef>,
    selected_row_index: Option<usize>,
    repository_prompt_ticket: Option<TicketSummary>,
    repository_prompt_project_id: Option<String>,
    repository_prompt_input: String,
    repository_prompt_missing_mapping: bool,
}

impl TicketPickerOverlayState {
    fn selected_row(&self) -> Option<&TicketPickerRowRef> {
        self.selected_row_index
            .and_then(|index| self.ticket_rows.get(index))
    }

    fn selected_project_index(&self) -> Option<usize> {
        match self.selected_row()? {
            TicketPickerRowRef::ProjectHeader { project_index } => Some(*project_index),
            TicketPickerRowRef::Ticket { project_index, .. } => Some(*project_index),
        }
    }

    fn selected_ticket(&self) -> Option<&TicketSummary> {
        let row = self.selected_row()?;
        let TicketPickerRowRef::Ticket {
            project_index,
            status_index,
            ticket_index,
        } = row
        else {
            return None;
        };
        self.project_groups
            .get(*project_index)
            .and_then(|project_group| project_group.status_groups.get(*status_index))
            .and_then(|status_group| status_group.tickets.get(*ticket_index))
    }

    fn open(&mut self) {
        self.visible = true;
        self.loading = true;
        self.error = None;
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
    }

    fn close(&mut self) {
        self.visible = false;
        self.loading = false;
        self.starting_ticket_id = None;
        self.error = None;
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
    }

    fn has_repository_prompt(&self) -> bool {
        self.repository_prompt_ticket.is_some()
    }

    fn start_repository_prompt(
        &mut self,
        ticket: TicketSummary,
        project_id: String,
        repository_path_hint: Option<String>,
    ) {
        self.repository_prompt_ticket = Some(ticket);
        self.repository_prompt_project_id = Some(project_id);
        if let Some(repository_path_hint) = repository_path_hint {
            self.repository_prompt_input = repository_path_hint;
            self.repository_prompt_missing_mapping = false;
        } else {
            self.repository_prompt_missing_mapping = true;
        }
    }

    fn cancel_repository_prompt(&mut self) {
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
    }

    fn apply_tickets(&mut self, tickets: Vec<TicketSummary>, priority_states: &[String]) {
        let selected_project_index = self.selected_project_index();
        let selected_ticket_id = self
            .selected_ticket()
            .map(|ticket| ticket.ticket_id.clone());
        let collapsed_projects = self
            .project_groups
            .iter()
            .filter(|project_group| project_group.collapsed)
            .map(|project_group| normalize_ticket_project(project_group.project.as_str()))
            .collect::<HashSet<_>>();
        self.project_groups =
            group_tickets_by_project(tickets, priority_states, &collapsed_projects);
        self.rebuild_ticket_rows(selected_ticket_id.as_ref(), selected_project_index);
    }

    fn move_selection(&mut self, delta: isize) {
        if self.ticket_rows.is_empty() {
            self.selected_row_index = None;
            return;
        }

        let current = self.selected_row_index.unwrap_or(0) as isize;
        let upper = self.ticket_rows.len() as isize - 1;
        self.selected_row_index = Some((current + delta).clamp(0, upper) as usize);
    }

    fn fold_selected_project(&mut self) {
        self.set_selected_project_collapsed(true);
    }

    fn unfold_selected_project(&mut self) {
        self.set_selected_project_collapsed(false);
    }

    fn set_selected_project_collapsed(&mut self, collapsed: bool) {
        let Some(project_index) = self.selected_project_index() else {
            return;
        };
        let Some(project_group) = self.project_groups.get_mut(project_index) else {
            return;
        };
        if project_group.collapsed == collapsed {
            return;
        }
        project_group.collapsed = collapsed;
        self.rebuild_ticket_rows(None, Some(project_index));
    }

    fn rebuild_ticket_rows(
        &mut self,
        preferred_ticket_id: Option<&TicketId>,
        preferred_project_index: Option<usize>,
    ) {
        self.ticket_rows =
            self.project_groups
                .iter()
                .enumerate()
                .flat_map(|(project_index, project_group)| {
                    let mut rows = vec![TicketPickerRowRef::ProjectHeader { project_index }];
                    if !project_group.collapsed {
                        rows.extend(project_group.status_groups.iter().enumerate().flat_map(
                            move |(status_index, status_group)| {
                                status_group.tickets.iter().enumerate().map(
                                    move |(ticket_index, _)| TicketPickerRowRef::Ticket {
                                        project_index,
                                        status_index,
                                        ticket_index,
                                    },
                                )
                            },
                        ));
                    }
                    rows
                })
                .collect();

        if self.ticket_rows.is_empty() {
            self.selected_row_index = None;
            return;
        }

        if let Some(preferred_ticket_id) = preferred_ticket_id {
            if let Some(index) = self.ticket_rows.iter().position(|row| {
                let TicketPickerRowRef::Ticket {
                    project_index,
                    status_index,
                    ticket_index,
                } = row
                else {
                    return false;
                };
                self.project_groups
                    .get(*project_index)
                    .and_then(|project_group| project_group.status_groups.get(*status_index))
                    .and_then(|status_group| status_group.tickets.get(*ticket_index))
                    .map(|ticket| ticket.ticket_id == *preferred_ticket_id)
                    .unwrap_or_default()
            }) {
                self.selected_row_index = Some(index);
                return;
            }
        }

        if let Some(preferred_project_index) = preferred_project_index {
            if let Some(index) = self.ticket_rows.iter().position(|row| {
                matches!(
                    row,
                    TicketPickerRowRef::ProjectHeader { project_index }
                        if *project_index == preferred_project_index
                )
            }) {
                self.selected_row_index = Some(index);
                return;
            }
        }

        self.selected_row_index = Some(0);
    }
}

#[derive(Debug, Clone)]
enum TicketPickerEvent {
    TicketsLoaded {
        tickets: Vec<TicketSummary>,
    },
    TicketsLoadFailed {
        message: String,
    },
    TicketStarted {
        projection: Option<ProjectionState>,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketStartFailed {
        message: String,
    },
    TicketStartRequiresRepository {
        ticket: TicketSummary,
        project_id: String,
        repository_path_hint: Option<String>,
        message: String,
    },
}

struct UiShellState {
    base_status: String,
    status_warning: Option<String>,
    domain: ProjectionState,
    selected_inbox_index: Option<usize>,
    selected_inbox_item_id: Option<InboxItemId>,
    view_stack: ViewStack,
    keymap: &'static KeymapTrie,
    mode_key_buffer: Vec<KeyStroke>,
    which_key_overlay: Option<WhichKeyOverlayState>,
    mode: UiMode,
    terminal_escape_pending: bool,
    supervisor_provider: Option<Arc<dyn LlmProvider>>,
    supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
    supervisor_chat_stream: Option<ActiveSupervisorChatStream>,
    global_supervisor_chat_draft: String,
    global_supervisor_chat_last_query: Option<String>,
    global_supervisor_chat_return_context: Option<GlobalSupervisorChatReturnContext>,
    ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
    ticket_picker_sender: Option<mpsc::Sender<TicketPickerEvent>>,
    ticket_picker_receiver: Option<mpsc::Receiver<TicketPickerEvent>>,
    ticket_picker_overlay: TicketPickerOverlayState,
    ticket_picker_priority_states: Vec<String>,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
    selected_session_index: Option<usize>,
    terminal_session_sender: Option<mpsc::Sender<TerminalSessionEvent>>,
    terminal_session_receiver: Option<mpsc::Receiver<TerminalSessionEvent>>,
    terminal_session_states: HashMap<WorkerSessionId, TerminalViewState>,
    terminal_session_streamed: HashSet<WorkerSessionId>,
    terminal_compose_draft: String,
}

impl UiShellState {
    #[cfg(test)]
    fn new(status: String, domain: ProjectionState) -> Self {
        Self::new_with_integrations(status, domain, None, None, None, None)
    }
    #[cfg(test)]
    fn new_with_supervisor(
        status: String,
        domain: ProjectionState,
        supervisor_provider: Option<Arc<dyn LlmProvider>>,
    ) -> Self {
        Self::new_with_integrations(
            status,
            domain,
            supervisor_provider,
            None,
            None,
            None,
        )
    }

    fn new_with_integrations(
        status: String,
        domain: ProjectionState,
        supervisor_provider: Option<Arc<dyn LlmProvider>>,
        supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
        ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
        worker_backend: Option<Arc<dyn WorkerBackend>>,
    ) -> Self {
        let (ticket_picker_sender, ticket_picker_receiver) = if ticket_picker_provider.is_some() {
            let (sender, receiver) = mpsc::channel(TICKET_PICKER_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let (terminal_session_sender, terminal_session_receiver) =
            if worker_backend.is_some() {
                let (sender, receiver) =
                    mpsc::channel(TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY);
                (Some(sender), Some(receiver))
            } else {
                (None, None)
            };

        Self {
            base_status: status,
            status_warning: None,
            domain,
            selected_inbox_index: None,
            selected_inbox_item_id: None,
            view_stack: ViewStack::default(),
            keymap: default_keymap_trie(),
            mode_key_buffer: Vec::new(),
            which_key_overlay: None,
            mode: UiMode::Normal,
            terminal_escape_pending: false,
            supervisor_provider,
            supervisor_command_dispatcher,
            supervisor_chat_stream: None,
            global_supervisor_chat_draft: String::new(),
            global_supervisor_chat_last_query: None,
            global_supervisor_chat_return_context: None,
            ticket_picker_provider,
            ticket_picker_sender,
            ticket_picker_receiver,
            ticket_picker_overlay: TicketPickerOverlayState::default(),
            ticket_picker_priority_states: ticket_picker_priority_states_from_env(),
            worker_backend,
            selected_session_index: None,
            terminal_session_sender,
            terminal_session_receiver,
            terminal_session_states: HashMap::new(),
            terminal_session_streamed: HashSet::new(),
            terminal_compose_draft: String::new(),
        }
    }

    fn ui_state(&self) -> UiState {
        let status = self.status_text();
        let terminal_view_state = self
            .active_terminal_session_id()
            .and_then(|session_id| self.terminal_session_states.get(session_id));
        let mut ui_state = project_ui_state(
            status.as_str(),
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
            terminal_view_state,
        );
        self.append_global_supervisor_chat_state(&mut ui_state);
        self.append_live_supervisor_chat(&mut ui_state);
        ui_state
    }

    fn status_text(&self) -> String {
        match self.status_warning.as_deref() {
            Some(warning) => format!("{} | warning: {warning}", self.base_status),
            None => self.base_status.clone(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let ui_state = self.ui_state();
        if ui_state.selected_inbox_item_id.is_none() && self.move_session_selection(delta) {
            return;
        }
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
            if self.move_to_first_session() {
                return;
            }
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(0), &ui_state.inbox_rows);
    }

    fn jump_to_last_item(&mut self) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            if self.move_to_last_session() {
                return;
            }
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
        let selected_inbox_item_id = ui_state.selected_inbox_item_id.clone();
        let has_selected_inbox_item = selected_inbox_item_id.is_some();
        let mut terminal_session_id = None;
        if let (Some(inbox_item_id), Some(session_id)) = (
            selected_inbox_item_id.as_ref(),
            ui_state.selected_session_id,
        ) {
            self.open_focus_and_push_center(
                inbox_item_id.clone(),
                CenterView::TerminalView {
                    session_id: session_id.clone(),
                },
            );
            terminal_session_id = Some(session_id.clone());
        } else if let Some(session_id) = self.selected_session_id_for_panel() {
            if let Some(inbox_item_id) = self.inbox_item_id_for_session(&session_id) {
                self.open_focus_and_push_center(
                    inbox_item_id,
                    CenterView::TerminalView {
                        session_id: session_id.clone(),
                    },
                );
                terminal_session_id = Some(session_id);
            } else {
                let _ = self.view_stack.push_center(CenterView::TerminalView {
                    session_id: session_id.clone(),
                });
                terminal_session_id = Some(session_id);
            }
        } else {
            match self.spawn_manual_terminal_session() {
                Ok(Some(session_id)) => {
                    terminal_session_id = Some(session_id.clone());
                    if let Some(inbox_item_id) = selected_inbox_item_id {
                        self.open_focus_and_push_center(
                            inbox_item_id,
                            CenterView::TerminalView { session_id },
                        );
                    } else {
                        let _ = self.view_stack.push_center(CenterView::TerminalView { session_id });
                    }
                }
                Ok(None) => {
                    if has_selected_inbox_item {
                        self.status_warning = Some(
                            "terminal unavailable: selected inbox item has no active session"
                                .to_owned(),
                        );
                    } else {
                        self.status_warning =
                            Some("terminal unavailable: no open session is currently selected".to_owned());
                    }
                }
                Err(error) => {
                    self.status_warning = Some(format!("terminal unavailable: {error}"));
                }
            }
        }

        if let Some(session_id) = terminal_session_id {
            self.ensure_terminal_stream(session_id);
        }
    }

    fn spawn_manual_terminal_session(&mut self) -> Result<Option<WorkerSessionId>, String> {
        let Some(backend) = self.worker_backend.clone() else {
            return Ok(None);
        };

        let workdir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let session_id = WorkerSessionId::new(format!(
            "manual-{nanos}",
            nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since_epoch| since_epoch.as_nanos())
                .unwrap_or(0)
        ));
        let spec = SpawnSpec {
            session_id: session_id.clone().into(),
            workdir,
            model: None,
            instruction_prelude: None,
            environment: Vec::new(),
        };

        let spawn_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("terminal spawn runtime unavailable: {error}"))?;

            runtime
                .block_on(async move { backend.spawn(spec).await })
                .map_err(|error| error.to_string())
        });
        let handle = spawn_thread
            .join()
            .map_err(|_| "terminal spawn worker thread panicked".to_owned())?
            .map_err(|error| format!("terminal spawn failed: {error}"))?;

        if handle.session_id.as_str() != session_id.as_str() {
            return Err(format!(
                "worker backend returned unexpected session id: expected '{expected}', got '{actual}'",
                expected = session_id.as_str(),
                actual = handle.session_id.as_str()
            ));
        }

        Ok(Some(session_id))
    }

    fn selected_session_id_for_terminal_action(&self) -> Option<WorkerSessionId> {
        self.active_terminal_session_id()
            .cloned()
            .or_else(|| self.selected_session_id_for_panel())
    }

    fn kill_selected_session(&mut self) {
        let Some(session_id) = self.selected_session_id_for_terminal_action() else {
            self.status_warning =
                Some("terminal kill unavailable: no terminal session is currently selected".to_owned());
            return;
        };

        let Some(backend) = self.worker_backend.clone() else {
            self.status_warning =
                Some("terminal kill unavailable: no worker backend configured".to_owned());
            return;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.status_warning =
                Some("terminal kill unavailable: cannot resolve backend session handle".to_owned());
            return;
        };

        if let Some(session) = self.domain.sessions.get_mut(&session_id) {
            session.status = Some(WorkerSessionStatus::Crashed);
        }
        if let Some(ticket_provider) = self.ticket_picker_provider.clone() {
            let session_id_for_store = session_id.clone();
            let reason =
                "killed from terminal session panel".to_owned();
            match TokioHandle::try_current() {
                Ok(runtime) => {
                    runtime.spawn(async move {
                        let _ = ticket_provider
                            .mark_session_crashed(session_id_for_store, reason)
                            .await;
                    });
                }
                Err(_) => {}
            }
        }
        self.terminal_session_states.remove(&session_id);
        self.terminal_session_streamed.remove(&session_id);
        if self.active_terminal_session_id() == Some(&session_id) {
            let _ = self.view_stack.pop_center();
        }

        let kill_target = session_id;
        let kill_backend = backend;
        let kill_handle = handle;
        match TokioHandle::try_current() {
            Ok(runtime) => {
                let session_id = kill_target.as_str().to_owned();
                runtime.spawn(async move {
                    let _ = kill_backend.kill(&kill_handle).await;
                });
                self.status_warning = Some(format!("sending terminal kill for session {session_id}"));
            }
            Err(_) => {
                let backend = kill_backend.clone();
                let handle = kill_handle.clone();
                let spawn_result = std::thread::Builder::new()
                    .name("orchestrator-terminal-kill".to_owned())
                    .spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build();
                        if let Ok(runtime) = runtime {
                            runtime.block_on(async move {
                                let _ = backend.kill(&handle).await;
                            });
                        }
                    });

                match spawn_result {
                    Ok(_) => {
                        self.status_warning = Some(format!(
                            "sending terminal kill for session {}",
                            kill_target.as_str()
                        ));
                    }
                    Err(error) => {
                        self.status_warning = Some(format!(
                            "terminal kill unavailable: cannot spawn kill worker thread: {error}"
                        ));
                    }
                }
            }
        }
    }

    fn session_panel_rows(&self) -> Vec<(WorkerSessionId, String, String)> {
        session_panel_rows(&self.domain)
    }

    fn session_ids_for_navigation(&self) -> Vec<WorkerSessionId> {
        self.session_panel_rows()
            .into_iter()
            .map(|(session_id, _repo, _line)| session_id)
            .collect()
    }

    fn selected_session_id_for_panel(&self) -> Option<WorkerSessionId> {
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            return None;
        }
        let index = self.selected_session_index.unwrap_or(0).min(session_ids.len() - 1);
        session_ids.get(index).cloned()
    }

    fn move_session_selection(&mut self, delta: isize) -> bool {
        let session_ids = self.session_ids_for_navigation();
        let len = session_ids.len();
        if len == 0 {
            self.selected_session_index = None;
            return false;
        }

        let mut index = self.selected_session_index.unwrap_or(0);
        if index >= len {
            index = len - 1;
        }
        let next = (index as isize + delta).rem_euclid(len as isize) as usize;
        self.selected_session_index = Some(next);
        true
    }

    fn move_to_first_session(&mut self) -> bool {
        if self.session_ids_for_navigation().is_empty() {
            self.selected_session_index = None;
            false
        } else {
            self.selected_session_index = Some(0);
            true
        }
    }

    fn move_to_last_session(&mut self) -> bool {
        let len = self.session_ids_for_navigation().len();
        if len == 0 {
            self.selected_session_index = None;
            false
        } else {
            self.selected_session_index = Some(len - 1);
            true
        }
    }

    fn inbox_item_id_for_session(&self, session_id: &WorkerSessionId) -> Option<InboxItemId> {
        self.domain
            .work_items
            .values()
            .find_map(|work_item| {
                if work_item.session_id.as_ref() == Some(session_id) {
                    work_item.inbox_items.first().cloned()
                } else {
                    None
                }
            })
            .and_then(|inbox_item_id| {
                self.domain
                    .inbox_items
                    .get(&inbox_item_id)
                    .cloned()
                    .map(|_| inbox_item_id)
            })
    }

    fn active_terminal_session_id(&self) -> Option<&WorkerSessionId> {
        match self.view_stack.active_center() {
            Some(CenterView::TerminalView { session_id }) => Some(session_id),
            _ => None,
        }
    }

    fn terminal_session_handle(&self, session_id: &WorkerSessionId) -> Option<SessionHandle> {
        Some(SessionHandle {
            session_id: RuntimeSessionId::from(session_id.clone()),
            backend: self.worker_backend.as_ref()?.kind(),
        })
    }

    fn open_inspector_for_selected(&mut self, inspector: ArtifactInspectorKind) {
        let ui_state = self.ui_state();
        if let (Some(inbox_item_id), Some(work_item_id)) = (
            ui_state.selected_inbox_item_id,
            ui_state.selected_work_item_id,
        ) {
            self.open_focus_and_push_center(
                inbox_item_id,
                CenterView::InspectorView {
                    work_item_id,
                    inspector,
                },
            );
        }
    }

    fn open_chat_inspector_for_selected(&mut self) {
        self.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        self.start_supervisor_stream_for_selected();
    }

    fn toggle_global_supervisor_chat(&mut self) {
        if self.is_global_supervisor_chat_active() {
            self.close_global_supervisor_chat();
        } else {
            self.open_global_supervisor_chat();
        }
    }

    fn open_global_supervisor_chat(&mut self) {
        if self.is_global_supervisor_chat_active() {
            self.enter_insert_mode();
            return;
        }

        self.global_supervisor_chat_return_context = Some(GlobalSupervisorChatReturnContext {
            selected_inbox_index: self.selected_inbox_index,
            selected_inbox_item_id: self.selected_inbox_item_id.clone(),
        });
        let _ = self.view_stack.push_center(CenterView::SupervisorChatView);
        self.enter_insert_mode();
    }

    fn close_global_supervisor_chat(&mut self) {
        if !self.is_global_supervisor_chat_active() {
            return;
        }

        if self.is_active_supervisor_stream_visible() {
            self.cancel_supervisor_stream();
        }

        let _ = self.view_stack.pop_center();
        self.enter_normal_mode();

        if let Some(context) = self.global_supervisor_chat_return_context.take() {
            self.selected_inbox_index = context.selected_inbox_index;
            self.selected_inbox_item_id = context.selected_inbox_item_id;
        }
    }

    fn open_focus_and_push_center(&mut self, inbox_item_id: InboxItemId, top_view: CenterView) {
        self.selected_inbox_item_id = Some(inbox_item_id.clone());
        let focus_view = CenterView::FocusCardView { inbox_item_id };
        self.view_stack.replace_center(focus_view);
        self.view_stack.push_center(top_view);
    }

    fn minimize_center_view(&mut self) {
        let _ = self.view_stack.pop_center();
        if !self.is_terminal_view_active() {
            self.enter_normal_mode();
        }
    }

    fn open_ticket_picker(&mut self) {
        if self.ticket_picker_provider.is_none() {
            self.status_warning =
                Some("ticket picker unavailable: no ticket provider configured".to_owned());
            return;
        }

        self.enter_normal_mode();
        self.ticket_picker_overlay.open();
        self.spawn_ticket_picker_load();
    }

    fn close_ticket_picker(&mut self) {
        self.ticket_picker_overlay.close();
    }

    fn move_ticket_picker_selection(&mut self, delta: isize) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.move_selection(delta);
    }

    fn fold_ticket_picker_selected_project(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.fold_selected_project();
    }

    fn unfold_ticket_picker_selected_project(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.unfold_selected_project();
    }

    fn start_selected_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        let Some(ticket) = self.ticket_picker_overlay.selected_ticket().cloned() else {
            return;
        };
        if self.ticket_picker_overlay.starting_ticket_id.is_some() {
            return;
        }
        self.spawn_ticket_picker_start_with_override(ticket, None);
    }

    fn start_selected_ticket_from_picker_with_override(
        &mut self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) {
        if self.ticket_picker_overlay.starting_ticket_id.is_some() {
            return;
        }
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.starting_ticket_id = Some(ticket.ticket_id.clone());
        self.spawn_ticket_picker_start_with_override(ticket, repository_override);
    }

    fn submit_ticket_picker_repository_prompt(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        let Some(ticket) = self
            .ticket_picker_overlay
            .repository_prompt_ticket
            .as_ref()
            .cloned()
        else {
            return;
        };
        let repository_path = self.ticket_picker_overlay.repository_prompt_input.trim();
        if repository_path.is_empty() {
            self.ticket_picker_overlay.error = Some("repository path cannot be empty".to_owned());
            return;
        }
        let Some(repository_path) = expand_tilde_path(repository_path) else {
            self.ticket_picker_overlay
                .error = Some("could not expand repository path: HOME is not set".to_owned());
            return;
        };
        self.start_selected_ticket_from_picker_with_override(
            ticket,
            Some(repository_path),
        );
    }

    fn cancel_ticket_picker_repository_prompt(&mut self) {
        self.ticket_picker_overlay.cancel_repository_prompt();
    }

    fn append_repository_prompt_char(&mut self, ch: char) {
        self.ticket_picker_overlay.repository_prompt_input.push(ch);
    }

    fn pop_repository_prompt_char(&mut self) {
        self.ticket_picker_overlay.repository_prompt_input.pop();
    }

    fn spawn_ticket_picker_load(&mut self) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.ticket_picker_overlay.loading = false;
            self.ticket_picker_overlay.error =
                Some("ticket provider unavailable while loading tickets".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.ticket_picker_overlay.loading = false;
            self.ticket_picker_overlay.error =
                Some("ticket picker event channel unavailable while loading tickets".to_owned());
            return;
        };

        self.ticket_picker_overlay.loading = true;

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_ticket_picker_load_task(provider, sender).await;
                });
            }
            Err(_) => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error =
                    Some("tokio runtime unavailable; cannot load tickets".to_owned());
            }
        }
    }

    fn spawn_ticket_picker_start_with_override(
        &mut self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.ticket_picker_overlay.starting_ticket_id = None;
            self.ticket_picker_overlay.error =
                Some("ticket provider unavailable while starting ticket".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.ticket_picker_overlay.starting_ticket_id = None;
            self.ticket_picker_overlay.error =
                Some("ticket picker event channel unavailable while starting ticket".to_owned());
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_ticket_picker_start_task(
                        provider,
                        ticket,
                        repository_override,
                        sender,
                    )
                    .await;
                });
            }
            Err(_) => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.error =
                    Some("tokio runtime unavailable; cannot start ticket".to_owned());
            }
        }
    }

    fn tick_ticket_picker(&mut self) {
        self.poll_ticket_picker_events();
    }

    fn tick_terminal_view(&mut self) {
        self.poll_terminal_session_events();
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            self.ensure_terminal_stream(session_id);
        }
    }

    fn poll_terminal_session_events(&mut self) {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.terminal_session_receiver.as_mut() else {
                return;
            };

            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status_warning =
                            Some("terminal stream channel closed unexpectedly".to_owned());
                        break;
                    }
                }
            }
        }

        for event in events {
            match event {
                TerminalSessionEvent::Output { session_id, output } => {
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    append_terminal_assistant_output(view, output.bytes);
                    view.error = None;
                }
                TerminalSessionEvent::StreamFailed { session_id, error } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    view.error = Some(error.to_string());
                    view.lines.clear();
                    view.output_fragment.clear();
                    if let RuntimeError::SessionNotFound(_) = error {
                        self.recover_terminal_session_on_not_found(&session_id);
                    }
                }
                TerminalSessionEvent::StreamEnded { session_id } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    flush_terminal_output_fragment(view);
                }
            }
        }
    }

    fn ensure_terminal_stream(&mut self, session_id: WorkerSessionId) {
        if self.terminal_session_streamed.contains(&session_id) {
            return;
        }
        let Some(backend) = self.worker_backend.clone() else {
            self.terminal_session_streamed.remove(&session_id);
            return;
        };
        let Some(sender) = self.terminal_session_sender.clone() else {
            self.terminal_session_streamed.remove(&session_id);
            self.status_warning =
                Some("terminal stream channel unavailable".to_owned());
            return;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.terminal_session_streamed.remove(&session_id);
            self.status_warning =
                Some("terminal stream unavailable: cannot build session handle".to_owned());
            return;
        };
        self.terminal_session_streamed.insert(session_id.clone());

        match TokioHandle::try_current() {
            Ok(runtime) => {
                let stream_session_id = session_id;
                runtime.spawn(async move {
                    let mut stream = match backend.subscribe(&handle).await {
                        Ok(stream) => stream,
                        Err(error) => {
                            let _ = sender
                                .send(TerminalSessionEvent::StreamFailed {
                                    session_id: stream_session_id,
                                    error,
                                })
                                .await;
                            return;
                        }
                    };

                    loop {
                        match stream.next_event().await {
                            Ok(Some(BackendEvent::Output(output))) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::Output {
                                        session_id: stream_session_id.clone(),
                                        output,
                                    })
                                    .await;
                            }
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::StreamEnded {
                                        session_id: stream_session_id,
                                    })
                                    .await;
                                return;
                            }
                            Err(error) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::StreamFailed {
                                        session_id: stream_session_id,
                                        error,
                                    })
                                    .await;
                                return;
                            }
                        }
                    }
                });
            }
            Err(_) => {
                self.terminal_session_streamed.remove(&session_id);
                self.status_warning =
                    Some("terminal stream unavailable: tokio runtime unavailable".to_owned());
            }
        }
    }

    fn recover_terminal_session_on_not_found(&mut self, session_id: &WorkerSessionId) {
        if self.terminal_session_event_is_stale(session_id) {
            return;
        }

        match self.spawn_manual_terminal_session() {
            Ok(Some(new_session_id)) => {
                let started_session = new_session_id.clone();
                let focus_session = self
                    .inbox_item_id_for_session(session_id)
                    .or_else(|| self.ui_state().selected_inbox_item_id.clone());

                if let Some(inbox_item_id) = focus_session {
                    self.open_focus_and_push_center(
                        inbox_item_id,
                        CenterView::TerminalView {
                            session_id: new_session_id,
                        },
                    );
                } else {
                    let _ = self.view_stack.pop_center();
                    let _ = self.view_stack.push_center(CenterView::TerminalView {
                        session_id: new_session_id,
                    });
                }
                self.ensure_terminal_stream(started_session);
                self.status_warning = Some(format!(
                    "terminal session {} was not found; opened a fresh terminal",
                    session_id.as_str()
                ));
            }
            Ok(None) => {
                self.status_warning = Some(
                    "terminal session unavailable: cannot spawn replacement terminal".to_owned(),
                );
            }
            Err(error) => {
                self.status_warning =
                    Some(format!("terminal replacement failed: {error}"));
            }
        }
    }

    fn terminal_session_event_is_stale(&self, session_id: &WorkerSessionId) -> bool {
        if !self.is_terminal_view_active() {
            return true;
        }

        match self.active_terminal_session_id() {
            Some(active_session_id) => active_session_id != session_id,
            None => true,
        }
    }

    fn poll_ticket_picker_events(&mut self) {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.ticket_picker_receiver.as_mut() else {
                return;
            };

            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status_warning =
                            Some("ticket picker event channel closed unexpectedly".to_owned());
                        break;
                    }
                }
            }
        }

        for event in events {
            self.apply_ticket_picker_event(event);
        }
    }

    fn apply_ticket_picker_event(&mut self, event: TicketPickerEvent) {
        match event {
            TicketPickerEvent::TicketsLoaded { tickets } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = None;
                self.ticket_picker_overlay
                    .apply_tickets(tickets, &self.ticket_picker_priority_states);
            }
            TicketPickerEvent::TicketsLoadFailed { message } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = Some(message.clone());
                self.status_warning = Some(format!(
                    "ticket picker load warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::TicketStarted {
                projection,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.cancel_repository_prompt();
                self.ticket_picker_overlay.error = None;
                if let Some(projection) = projection {
                    self.domain = projection;
                }
                if let Some(tickets) = tickets {
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, &self.ticket_picker_priority_states);
                }
                if let Some(message) = warning {
                    self.status_warning = Some(format!(
                        "ticket picker start warning: {}",
                        compact_focus_card_text(message.as_str())
                    ));
                }
            }
            TicketPickerEvent::TicketStartRequiresRepository {
                ticket,
                project_id,
                repository_path_hint,
                message,
            } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.start_repository_prompt(
                    ticket,
                    project_id,
                    repository_path_hint,
                );
                self.ticket_picker_overlay.error = Some(message);
            }
            TicketPickerEvent::TicketStartFailed { message } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.cancel_repository_prompt();
                self.ticket_picker_overlay.error = Some(message.clone());
                self.status_warning = Some(format!(
                    "ticket picker start warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
        }
    }

    fn is_ticket_picker_visible(&self) -> bool {
        self.ticket_picker_overlay.visible
    }

    fn submit_global_supervisor_chat_query(&mut self) {
        let query = self.global_supervisor_chat_draft.trim().to_owned();
        if query.is_empty() {
            let message =
                "supervisor query unavailable: enter a non-empty question before submitting";
            self.status_warning = Some(message.to_owned());
            self.set_supervisor_terminal_state(
                SupervisorStreamTarget::GlobalChatPanel,
                SupervisorResponseState::NoContext,
                message,
            );
            return;
        }

        let started = if let Some(dispatcher) = self.supervisor_command_dispatcher.clone() {
            self.start_supervisor_stream_with_dispatcher_for_global_query(dispatcher, query.clone())
        } else if let Some(provider) = self.supervisor_provider.clone() {
            let request = build_global_supervisor_chat_request(query.as_str());
            self.start_supervisor_stream_with_provider(
                SupervisorStreamTarget::GlobalChatPanel,
                provider,
                request,
            )
        } else {
            let message =
                "supervisor stream unavailable: no LLM provider or command dispatcher configured";
            self.status_warning = Some(message.to_owned());
            self.set_supervisor_terminal_state(
                SupervisorStreamTarget::GlobalChatPanel,
                SupervisorResponseState::BackendUnavailable,
                message,
            );
            false
        };

        if started {
            self.global_supervisor_chat_draft.clear();
            self.global_supervisor_chat_last_query = Some(query);
        }
    }

    fn apply_global_chat_insert_key(&mut self, key: KeyEvent) -> bool {
        if !self.is_global_supervisor_chat_active() || self.mode != UiMode::Insert {
            return false;
        }

        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.submit_global_supervisor_chat_query();
                true
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.global_supervisor_chat_draft.pop();
                true
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.global_supervisor_chat_draft.push(ch);
                true
            }
            _ => false,
        }
    }

    fn apply_terminal_compose_key(&mut self, key: KeyEvent) -> bool {
        if self.mode != UiMode::Terminal || !self.is_terminal_view_active() {
            return false;
        }

        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL => {
                self.submit_terminal_compose_input();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                self.terminal_compose_draft.push('\n');
                true
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.terminal_compose_draft.pop();
                true
            }
            KeyCode::Tab if key.modifiers.is_empty() => {
                self.terminal_compose_draft.push('\t');
                true
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.terminal_compose_draft.push(ch);
                true
            }
            _ => false,
        }
    }

    fn start_supervisor_stream_for_selected(&mut self) {
        let ui_state = project_ui_state(
            self.base_status.as_str(),
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
            None,
        );
        let Some(selected_row) = ui_state
            .selected_inbox_index
            .and_then(|index| ui_state.inbox_rows.get(index))
            .cloned()
        else {
            self.status_warning = Some(
                "supervisor query unavailable: select an inbox item before opening chat".to_owned(),
            );
            return;
        };

        if let Some(dispatcher) = self.supervisor_command_dispatcher.clone() {
            let _ = self.start_supervisor_stream_with_dispatcher(dispatcher, selected_row);
            return;
        }

        let target = SupervisorStreamTarget::Inspector {
            work_item_id: selected_row.work_item_id.clone(),
        };
        let Some(provider) = self.supervisor_provider.clone() else {
            let message =
                "supervisor stream unavailable: no LLM provider or command dispatcher configured";
            self.status_warning = Some(message.to_owned());
            self.set_supervisor_terminal_state(
                target,
                SupervisorResponseState::BackendUnavailable,
                message,
            );
            return;
        };

        let request = build_supervisor_chat_request(&selected_row, &self.domain);
        let _ = self.start_supervisor_stream_with_provider(target, provider, request);
    }

    fn start_supervisor_stream_with_dispatcher_for_global_query(
        &mut self,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
        query: String,
    ) -> bool {
        let invocation = match CommandRegistry::default().to_untyped_invocation(
            &Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query,
                context: Some(SupervisorQueryContextArgs {
                    selected_work_item_id: None,
                    selected_session_id: None,
                    scope: Some("global".to_owned()),
                }),
            }),
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                let message = format!(
                    "supervisor query unavailable: failed to build command invocation ({error})"
                );
                let state = classify_supervisor_stream_error(message.as_str());
                self.status_warning = Some(message.clone());
                self.set_supervisor_terminal_state(
                    SupervisorStreamTarget::GlobalChatPanel,
                    state,
                    message,
                );
                return false;
            }
        };

        let context = SupervisorCommandContext {
            selected_work_item_id: None,
            selected_session_id: None,
            scope: Some("global".to_owned()),
        };

        self.start_supervisor_stream_with_dispatcher_invocation(
            SupervisorStreamTarget::GlobalChatPanel,
            dispatcher,
            invocation,
            context,
        )
    }

    fn start_supervisor_stream_with_dispatcher(
        &mut self,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
        selected_row: UiInboxRow,
    ) -> bool {
        let invocation = match CommandRegistry::default().to_untyped_invocation(
            &Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query: "What is the current status of this ticket?".to_owned(),
                context: None,
            }),
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                let message = format!(
                    "supervisor query unavailable: failed to build command invocation ({error})"
                );
                let state = classify_supervisor_stream_error(message.as_str());
                self.status_warning = Some(message.clone());
                self.set_supervisor_terminal_state(
                    SupervisorStreamTarget::Inspector {
                        work_item_id: selected_row.work_item_id.clone(),
                    },
                    state,
                    message,
                );
                return false;
            }
        };

        let context = SupervisorCommandContext {
            selected_work_item_id: Some(selected_row.work_item_id.as_str().to_owned()),
            selected_session_id: selected_row
                .session_id
                .as_ref()
                .map(|session_id| session_id.as_str().to_owned()),
            scope: selected_row
                .session_id
                .as_ref()
                .map(|session_id| format!("session:{}", session_id.as_str()))
                .or_else(|| Some(format!("work_item:{}", selected_row.work_item_id.as_str()))),
        };

        self.start_supervisor_stream_with_dispatcher_invocation(
            SupervisorStreamTarget::Inspector {
                work_item_id: selected_row.work_item_id.clone(),
            },
            dispatcher,
            invocation,
            context,
        )
    }

    fn start_supervisor_stream_with_provider(
        &mut self,
        target: SupervisorStreamTarget,
        provider: Arc<dyn LlmProvider>,
        request: LlmChatRequest,
    ) -> bool {
        let Some(handle) = self.supervisor_runtime_handle() else {
            self.set_supervisor_terminal_state(
                target,
                SupervisorResponseState::BackendUnavailable,
                "supervisor stream unavailable: tokio runtime is not active",
            );
            return false;
        };

        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        self.replace_supervisor_stream(target, receiver);
        self.status_warning = None;
        handle.spawn(async move {
            run_supervisor_stream_task(provider, request, sender).await;
        });
        true
    }

    fn start_supervisor_stream_with_dispatcher_invocation(
        &mut self,
        target: SupervisorStreamTarget,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
        invocation: UntypedCommandInvocation,
        context: SupervisorCommandContext,
    ) -> bool {
        let Some(handle) = self.supervisor_runtime_handle() else {
            self.set_supervisor_terminal_state(
                target,
                SupervisorResponseState::BackendUnavailable,
                "supervisor stream unavailable: tokio runtime is not active",
            );
            return false;
        };

        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        self.replace_supervisor_stream(target, receiver);
        self.status_warning = None;
        handle.spawn(async move {
            run_supervisor_command_task(dispatcher, invocation, context, sender).await;
        });
        true
    }

    fn supervisor_runtime_handle(&mut self) -> Option<TokioHandle> {
        match TokioHandle::try_current() {
            Ok(handle) => Some(handle),
            Err(_) => {
                self.status_warning =
                    Some("supervisor stream unavailable: tokio runtime is not active".to_owned());
                None
            }
        }
    }

    fn set_supervisor_terminal_state(
        &mut self,
        target: SupervisorStreamTarget,
        response_state: SupervisorResponseState,
        message: impl Into<String>,
    ) {
        if let Some(previous_stream) = self.supervisor_chat_stream.take() {
            if previous_stream.lifecycle.is_active() {
                if let Some(stream_id) = previous_stream.stream_id {
                    self.spawn_supervisor_cancel(stream_id);
                }
            }
        }
        self.supervisor_chat_stream = Some(ActiveSupervisorChatStream::terminal_state(
            target,
            response_state,
            message,
        ));
    }

    fn replace_supervisor_stream(
        &mut self,
        target: SupervisorStreamTarget,
        receiver: mpsc::Receiver<SupervisorStreamEvent>,
    ) {
        if let Some(previous_stream) = self.supervisor_chat_stream.take() {
            if previous_stream.lifecycle.is_active() {
                if let Some(stream_id) = previous_stream.stream_id {
                    self.spawn_supervisor_cancel(stream_id);
                }
            }
        }
        self.supervisor_chat_stream = Some(ActiveSupervisorChatStream::new(target, receiver));
    }

    fn cancel_supervisor_stream(&mut self) {
        let Some(stream) = self.supervisor_chat_stream.as_mut() else {
            return;
        };
        if !stream.lifecycle.is_active() {
            return;
        }

        stream.lifecycle = SupervisorStreamLifecycle::Cancelling;
        stream.pending_cancel = true;
        let stream_id = stream.stream_id.clone();

        if let Some(stream_id) = stream_id {
            stream.pending_cancel = false;
            self.spawn_supervisor_cancel(stream_id);
        }
    }

    fn spawn_supervisor_cancel(&mut self, stream_id: String) {
        if let Some(dispatcher) = self.supervisor_command_dispatcher.clone() {
            match TokioHandle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = dispatcher
                            .cancel_supervisor_command(stream_id.as_str())
                            .await;
                    });
                }
                Err(_) => {
                    self.status_warning = Some(
                        "supervisor cancel unavailable: tokio runtime is not active".to_owned(),
                    );
                }
            }
            return;
        }

        let Some(provider) = self.supervisor_provider.clone() else {
            self.status_warning =
                Some("supervisor cancel unavailable: no LLM provider configured".to_owned());
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = provider.cancel_stream(stream_id.as_str()).await;
                });
            }
            Err(_) => {
                self.status_warning =
                    Some("supervisor cancel unavailable: tokio runtime is not active".to_owned());
            }
        }
    }

    fn is_active_supervisor_stream_visible(&self) -> bool {
        let Some(stream) = self.supervisor_chat_stream.as_ref() else {
            return false;
        };
        if !stream.lifecycle.is_active() {
            return false;
        }

        match (&stream.target, self.view_stack.active_center()) {
            (
                SupervisorStreamTarget::Inspector {
                    work_item_id: stream_work_item_id,
                },
                Some(CenterView::InspectorView {
                    work_item_id,
                    inspector: ArtifactInspectorKind::Chat,
                }),
            ) => work_item_id == stream_work_item_id,
            (SupervisorStreamTarget::GlobalChatPanel, Some(CenterView::SupervisorChatView)) => true,
            _ => false,
        }
    }

    fn tick_supervisor_stream(&mut self) {
        self.poll_supervisor_stream_events();
        if let Some(stream) = self.supervisor_chat_stream.as_mut() {
            stream.flush_pending_delta();
        }
    }

    fn poll_supervisor_stream_events(&mut self) {
        let mut cancel_stream_id: Option<String> = None;
        let mut warning_message: Option<String> = None;

        {
            let Some(stream) = self.supervisor_chat_stream.as_mut() else {
                return;
            };

            loop {
                match stream.receiver.try_recv() {
                    Ok(SupervisorStreamEvent::Started { stream_id }) => {
                        stream.stream_id = Some(stream_id.clone());
                        if stream.lifecycle != SupervisorStreamLifecycle::Cancelling {
                            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
                        }
                        stream.response_state = SupervisorResponseState::Nominal;
                        stream.state_message = None;
                        stream.cooldown_hint = None;
                        if stream.pending_cancel {
                            stream.pending_cancel = false;
                            cancel_stream_id = Some(stream_id);
                        }
                    }
                    Ok(SupervisorStreamEvent::Delta { text }) => {
                        if !text.is_empty() {
                            stream.pending_delta.push_str(text.as_str());
                            stream.pending_chunk_count += 1;
                        }
                    }
                    Ok(SupervisorStreamEvent::RateLimit { state }) => {
                        let exhausted = state.requests_remaining.is_some_and(|value| value == 0)
                            || state.tokens_remaining.is_some_and(|value| value == 0);
                        let low_headroom = state
                            .tokens_remaining
                            .is_some_and(|value| value <= SUPERVISOR_STREAM_LOW_TOKEN_HEADROOM);
                        stream.last_rate_limit = Some(state.clone());
                        if exhausted {
                            stream.set_response_state(
                                SupervisorResponseState::RateLimited,
                                Some(
                                    "Provider quota is exhausted for the current window."
                                        .to_owned(),
                                ),
                                state
                                    .reset_at
                                    .as_deref()
                                    .map(|reset_at| format!("rate limit reset at {reset_at}")),
                            );
                            warning_message = Some(
                                "supervisor rate limit reached; wait for cooldown before retry"
                                    .to_owned(),
                            );
                        } else if low_headroom
                            && stream.response_state == SupervisorResponseState::Nominal
                        {
                            stream.set_response_state(
                                SupervisorResponseState::HighCost,
                                Some(
                                    "Remaining token headroom is low; tighten follow-up scope."
                                        .to_owned(),
                                ),
                                state
                                    .reset_at
                                    .as_deref()
                                    .map(|reset_at| format!("rate limit reset at {reset_at}")),
                            );
                        }
                    }
                    Ok(SupervisorStreamEvent::Usage { usage }) => {
                        if usage_trips_high_cost_state(&usage)
                            && stream.response_state != SupervisorResponseState::RateLimited
                        {
                            stream.set_response_state(
                                SupervisorResponseState::HighCost,
                                Some(format!(
                                    "Response consumed {} tokens; prefer tighter prompts.",
                                    usage.total_tokens
                                )),
                                None,
                            );
                        }
                        stream.usage = Some(usage);
                    }
                    Ok(SupervisorStreamEvent::Finished { reason, usage }) => {
                        if let Some(usage) = usage {
                            if usage_trips_high_cost_state(&usage)
                                && stream.response_state != SupervisorResponseState::RateLimited
                            {
                                stream.set_response_state(
                                    SupervisorResponseState::HighCost,
                                    Some(format!(
                                        "Response consumed {} tokens; prefer tighter prompts.",
                                        usage.total_tokens
                                    )),
                                    None,
                                );
                            }
                            stream.usage = Some(usage);
                        }
                        stream.lifecycle = match reason {
                            LlmFinishReason::Cancelled => SupervisorStreamLifecycle::Cancelled,
                            LlmFinishReason::Error => SupervisorStreamLifecycle::Error,
                            _ => SupervisorStreamLifecycle::Completed,
                        };
                        if reason == LlmFinishReason::Error {
                            stream.set_response_state(
                                SupervisorResponseState::BackendUnavailable,
                                Some(
                                    "The supervisor stream ended unexpectedly; safe to retry."
                                        .to_owned(),
                                ),
                                None,
                            );
                            warning_message =
                                Some("supervisor stream ended with error finish reason".to_owned());
                        }
                    }
                    Ok(SupervisorStreamEvent::Failed { message }) => {
                        let response_state = classify_supervisor_stream_error(&message);
                        let cooldown_hint =
                            if response_state == SupervisorResponseState::RateLimited {
                                parse_rate_limit_cooldown_hint(message.as_str())
                            } else {
                                None
                            };
                        stream.error_message = Some(message.clone());
                        stream.lifecycle = SupervisorStreamLifecycle::Error;
                        stream.set_response_state(
                            response_state,
                            Some(supervisor_state_message(response_state).to_owned()),
                            cooldown_hint,
                        );
                        warning_message = Some(message);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if stream.lifecycle == SupervisorStreamLifecycle::Connecting
                            || stream.lifecycle == SupervisorStreamLifecycle::Streaming
                            || stream.lifecycle == SupervisorStreamLifecycle::Cancelling
                        {
                            stream.lifecycle = SupervisorStreamLifecycle::Error;
                            stream.set_response_state(
                                SupervisorResponseState::BackendUnavailable,
                                Some(
                                    "Supervisor transport closed unexpectedly; retry is safe."
                                        .to_owned(),
                                ),
                                None,
                            );
                            warning_message =
                                Some("supervisor stream channel closed unexpectedly".to_owned());
                        }
                        break;
                    }
                }
            }
        }

        if let Some(stream_id) = cancel_stream_id {
            self.spawn_supervisor_cancel(stream_id);
        }
        if let Some(message) = warning_message {
            let state = classify_supervisor_stream_error(message.as_str());
            self.status_warning = Some(format!(
                "supervisor {} warning: {}",
                response_state_warning_label(state),
                compact_focus_card_text(message.as_str())
            ));
        }
    }

    fn append_live_supervisor_chat(&self, ui_state: &mut UiState) {
        let Some(stream) = self.supervisor_chat_stream.as_ref() else {
            return;
        };
        match (self.view_stack.active_center(), &stream.target) {
            (
                Some(CenterView::InspectorView {
                    work_item_id,
                    inspector: ArtifactInspectorKind::Chat,
                }),
                SupervisorStreamTarget::Inspector {
                    work_item_id: stream_work_item_id,
                },
            ) if work_item_id == stream_work_item_id => {
                ui_state.center_pane.lines.extend(stream.render_lines());
            }
            (Some(CenterView::SupervisorChatView), SupervisorStreamTarget::GlobalChatPanel) => {
                ui_state.center_pane.lines.extend(stream.render_lines());
            }
            _ => {}
        }
    }

    fn append_global_supervisor_chat_state(&self, ui_state: &mut UiState) {
        if !self.is_global_supervisor_chat_active() {
            return;
        }

        ui_state.center_pane.lines.push(String::new());
        if let Some(query) = self.global_supervisor_chat_last_query.as_deref() {
            ui_state
                .center_pane
                .lines
                .push(format!("Last query: {}", compact_focus_card_text(query)));
        }
        ui_state.center_pane.lines.push(format!(
            "Draft: {}",
            if self.global_supervisor_chat_draft.is_empty() {
                "<type in Insert mode>".to_owned()
            } else {
                self.global_supervisor_chat_draft.clone()
            }
        ));
    }

    fn is_global_supervisor_chat_active(&self) -> bool {
        matches!(
            self.view_stack.active_center(),
            Some(CenterView::SupervisorChatView)
        )
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

    fn submit_terminal_compose_input(&mut self) {
        if !self.is_terminal_view_active() {
            return;
        }

        if self.terminal_compose_draft.trim().is_empty() {
            self.status_warning =
                Some("terminal input unavailable: compose a non-empty message before sending".to_owned());
            return;
        }

        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            self.status_warning = Some(
                "terminal input unavailable: no active terminal session selected".to_owned(),
            );
            return;
        };
        let Some(backend) = self.worker_backend.clone() else {
            self.status_warning =
                Some("terminal input unavailable: no worker backend configured".to_owned());
            return;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.status_warning = Some(
                "terminal input unavailable: cannot resolve backend session handle".to_owned(),
            );
            return;
        };

        let user_message = self.terminal_compose_draft.clone();
        let mut payload = user_message.clone();
        if !payload.ends_with('\n') {
            payload.push('\n');
        }
        let bytes = payload.into_bytes();

        if backend.kind() != BackendKind::Codex {
            let view = self
                .terminal_session_states
                .entry(session_id.clone())
                .or_default();
            append_terminal_user_message(view, user_message.as_str());
            view.error = None;
        }

        match TokioHandle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let _ = backend.send_input(&handle, bytes.as_slice()).await;
                });
                self.terminal_compose_draft.clear();
            }
            Err(_) => {
                self.status_warning = Some("terminal input unavailable: tokio runtime unavailable".to_owned());
            }
        }
    }

    fn advance_terminal_workflow_stage(&mut self) {
        if !self.is_terminal_view_active() {
            self.status_warning = Some(
                "workflow advance unavailable: open a terminal session first".to_owned(),
            );
            return;
        }

        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            self.status_warning = Some(
                "workflow advance unavailable: no active terminal session selected".to_owned(),
            );
            return;
        };
        let Some(backend) = self.worker_backend.clone() else {
            self.status_warning =
                Some("workflow advance unavailable: no worker backend configured".to_owned());
            return;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.status_warning = Some(
                "workflow advance unavailable: cannot resolve backend session handle".to_owned(),
            );
            return;
        };

        let instruction = {
            let view = self
                .terminal_session_states
                .entry(session_id.clone())
                .or_default();
            let Some((next_stage, instruction)) = view.workflow_stage.advance_instruction() else {
                self.status_warning =
                    Some("workflow advance ignored: session is already in Complete".to_owned());
                return;
            };
            append_terminal_system_message(view, instruction);
            view.error = None;
            view.workflow_stage = next_stage;
            instruction.to_owned()
        };

        let mut payload = instruction;
        if !payload.ends_with('\n') {
            payload.push('\n');
        }
        let bytes = payload.into_bytes();
        match TokioHandle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let _ = backend.send_input(&handle, bytes.as_slice()).await;
                });
            }
            Err(_) => {
                self.status_warning =
                    Some("workflow advance unavailable: tokio runtime unavailable".to_owned());
            }
        }
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

async fn run_supervisor_stream_task(
    provider: Arc<dyn LlmProvider>,
    request: LlmChatRequest,
    sender: mpsc::Sender<SupervisorStreamEvent>,
) {
    let (stream_id, stream) = match provider.stream_chat(request).await {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(SupervisorStreamEvent::Failed {
                    message: error.to_string(),
                })
                .await;
            return;
        }
    };

    relay_supervisor_stream(stream_id, stream, sender).await;
}

async fn run_supervisor_command_task(
    dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    invocation: UntypedCommandInvocation,
    context: SupervisorCommandContext,
    sender: mpsc::Sender<SupervisorStreamEvent>,
) {
    let (stream_id, stream) = match dispatcher
        .dispatch_supervisor_command(invocation, context)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(SupervisorStreamEvent::Failed {
                    message: error.to_string(),
                })
                .await;
            return;
        }
    };

    relay_supervisor_stream(stream_id, stream, sender).await;
}

async fn relay_supervisor_stream(
    stream_id: String,
    mut stream: LlmResponseStream,
    sender: mpsc::Sender<SupervisorStreamEvent>,
) {
    if sender
        .send(SupervisorStreamEvent::Started { stream_id })
        .await
        .is_err()
    {
        return;
    }

    loop {
        match stream.next_chunk().await {
            Ok(Some(chunk)) => {
                let delta = chunk.delta;
                let finish_reason = chunk.finish_reason;
                let usage = chunk.usage;
                let rate_limit = chunk.rate_limit;

                if !delta.is_empty()
                    && sender
                        .send(SupervisorStreamEvent::Delta { text: delta })
                        .await
                        .is_err()
                {
                    return;
                }

                if let Some(rate_limit) = rate_limit {
                    if sender
                        .send(SupervisorStreamEvent::RateLimit { state: rate_limit })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }

                if finish_reason.is_none() {
                    if let Some(usage) = usage.clone() {
                        if sender
                            .send(SupervisorStreamEvent::Usage { usage })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }

                if let Some(reason) = finish_reason {
                    let _ = sender
                        .send(SupervisorStreamEvent::Finished { reason, usage })
                        .await;
                    return;
                }
            }
            Ok(None) => {
                let _ = sender
                    .send(SupervisorStreamEvent::Finished {
                        reason: LlmFinishReason::Stop,
                        usage: None,
                    })
                    .await;
                return;
            }
            Err(error) => {
                let _ = sender
                    .send(SupervisorStreamEvent::Failed {
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
        }
    }
}

async fn run_ticket_picker_load_task(
    provider: Arc<dyn TicketPickerProvider>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.list_unfinished_tickets().await {
        Ok(tickets) => {
            let _ = sender
                .send(TicketPickerEvent::TicketsLoaded { tickets })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::TicketsLoadFailed {
                    message: error.to_string(),
                })
                .await;
        }
    }
}

async fn run_ticket_picker_start_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket: TicketSummary,
    repository_override: Option<PathBuf>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let started_ticket = ticket.clone();

    if let Err(error) = provider
        .start_or_resume_ticket(ticket, repository_override)
        .await
    {
        match &error {
            CoreError::MissingProjectRepositoryMapping { project, .. } => {
                let _ = sender
                    .send(TicketPickerEvent::TicketStartRequiresRepository {
                        ticket: started_ticket,
                        project_id: project.clone(),
                        repository_path_hint: None,
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
            CoreError::InvalidMappedRepository {
                project,
                repository_path,
                ..
            } => {
                let _ = sender
                    .send(TicketPickerEvent::TicketStartRequiresRepository {
                        ticket: started_ticket,
                        project_id: project.clone(),
                        repository_path_hint: Some(repository_path.clone()),
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
            error => {
                let _ = sender
                    .send(TicketPickerEvent::TicketStartFailed {
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
        }
    }

    let mut warning = Vec::new();
    let projection = match provider.reload_projection().await {
        Ok(projection) => Some(projection),
        Err(error) => {
            warning.push(format!("failed to reload projection: {error}"));
            None
        }
    };
    let tickets = match provider.list_unfinished_tickets().await {
        Ok(tickets) => Some(tickets),
        Err(error) => {
            warning.push(format!("failed to refresh tickets: {error}"));
            None
        }
    };

    let _ = sender
        .send(TicketPickerEvent::TicketStarted {
            projection,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

fn expand_tilde_path(raw: &str) -> Option<PathBuf> {
    if raw == "~" {
        return resolve_shell_home().map(PathBuf::from);
    }

    if let Some(suffix) = raw.strip_prefix("~/") {
        return resolve_shell_home().map(|home| PathBuf::from(home).join(suffix));
    }

    if let Some(suffix) = raw.strip_prefix("~\\") {
        return resolve_shell_home().map(|home| PathBuf::from(home).join(suffix));
    }

    Some(PathBuf::from(raw))
}

fn resolve_shell_home() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
}

fn render_ticket_picker_overlay_text(overlay: &TicketPickerOverlayState) -> String {
    let mut lines = vec![
        "j/k or arrows: move | h/Left: fold | l/Right: unfold | Enter: start | Esc: close"
            .to_owned(),
    ];

    if overlay.loading {
        lines.push("Loading unfinished tickets...".to_owned());
    }
    if let Some(starting_ticket_id) = overlay.starting_ticket_id.as_ref() {
        lines.push(format!("Starting {}...", starting_ticket_id.as_str()));
    }
    if let Some(error) = overlay.error.as_ref() {
        lines.push(format!(
            "Error: {}",
            compact_focus_card_text(error.as_str())
        ));
    }

    if overlay.has_repository_prompt() {
        lines.push(String::new());
        let project_id = overlay
            .repository_prompt_project_id
            .as_deref()
            .unwrap_or("selected project");
        if overlay.repository_prompt_missing_mapping {
            lines.push(format!(
                "Repository mapping missing for project '{project_id}'.",
            ));
        } else {
            lines.push(format!(
                "Repository path could not be resolved for project '{project_id}'.",
            ));
        }
        lines.push(
            "Enter local repository path, then press Enter. Esc to cancel.".to_owned(),
        );
        lines.push(format!("Path: {}", overlay.repository_prompt_input));
        lines.push(String::new());
    }

    if overlay.project_groups.is_empty() {
        lines.push("No unfinished tickets found.".to_owned());
        return lines.join("\n");
    }

    for (project_index, project_group) in overlay.project_groups.iter().enumerate() {
        let project_selected = overlay
            .selected_row()
            .map(|row| {
                matches!(
                    row,
                    TicketPickerRowRef::ProjectHeader {
                        project_index: selected_project_index,
                    } if *selected_project_index == project_index
                )
            })
            .unwrap_or_default();
        let selected_prefix = if project_selected { ">" } else { " " };
        let fold_marker = if project_group.collapsed {
            "[+]"
        } else {
            "[-]"
        };
        lines.push(format!(
            "{selected_prefix}{fold_marker} {} ({})",
            project_group.project,
            project_group.ticket_count()
        ));

        if project_group.collapsed {
            lines.push(String::new());
            continue;
        }

        for (status_index, status_group) in project_group.status_groups.iter().enumerate() {
            lines.push(format!(
                "   {} ({})",
                status_group.status,
                status_group.tickets.len()
            ));
            for (ticket_index, ticket) in status_group.tickets.iter().enumerate() {
                let is_selected = overlay
                    .selected_row()
                    .map(|row| {
                        matches!(
                            row,
                            TicketPickerRowRef::Ticket {
                                project_index: selected_project_index,
                                status_index: selected_status_index,
                                ticket_index: selected_ticket_index,
                            } if *selected_project_index == project_index
                                && *selected_status_index == status_index
                                && *selected_ticket_index == ticket_index
                        )
                    })
                    .unwrap_or_default();
                let selected_prefix = if is_selected { ">" } else { " " };
                let starting_prefix = if overlay
                    .starting_ticket_id
                    .as_ref()
                    .map(|id| id == &ticket.ticket_id)
                    .unwrap_or_default()
                {
                    "*"
                } else {
                    " "
                };
                lines.push(format!(
                    "{selected_prefix}{starting_prefix}    {}: {}",
                    ticket.identifier,
                    compact_focus_card_text(ticket.title.as_str())
                ));
            }
        }
        lines.push(String::new());
    }

    if lines.last().map(|line| line.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    lines.join("\n")
}

fn route_ticket_picker_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.ticket_picker_overlay.has_repository_prompt() {
        return route_ticket_picker_repository_prompt_key(shell_state, key);
    }

    if is_escape_to_normal(key) {
        return RoutedInput::Command(UiCommand::CloseTicketPicker);
    }

    if !key.modifiers.is_empty() {
        return RoutedInput::Ignore;
    }

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => RoutedInput::Command(UiCommand::TicketPickerMoveNext),
        KeyCode::Char('k') | KeyCode::Up => {
            RoutedInput::Command(UiCommand::TicketPickerMovePrevious)
        }
        KeyCode::Char('h') | KeyCode::Left => {
            RoutedInput::Command(UiCommand::TicketPickerFoldProject)
        }
        KeyCode::Char('l') | KeyCode::Right => {
            RoutedInput::Command(UiCommand::TicketPickerUnfoldProject)
        }
        KeyCode::Enter => RoutedInput::Command(UiCommand::TicketPickerStartSelected),
        _ => RoutedInput::Ignore,
    }
}

fn route_ticket_picker_repository_prompt_key(
    shell_state: &mut UiShellState,
    key: KeyEvent,
) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.cancel_ticket_picker_repository_prompt();
        return RoutedInput::Ignore;
    }

    if matches!(key.code, KeyCode::Enter) {
        shell_state.submit_ticket_picker_repository_prompt();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Backspace | KeyCode::Delete => {
                shell_state.pop_repository_prompt_char();
                return RoutedInput::Ignore;
            }
            KeyCode::Char(ch) => {
                shell_state.append_repository_prompt_char(ch);
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
}

pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    supervisor_provider: Option<Arc<dyn LlmProvider>>,
    supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
    ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
}

impl Ui {
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            supervisor_provider: None,
            supervisor_command_dispatcher: None,
            ticket_picker_provider: None,
            worker_backend: None,
        })
    }

    pub fn with_supervisor_provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.supervisor_provider = Some(provider);
        self
    }

    pub fn with_supervisor_command_dispatcher(
        mut self,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    ) -> Self {
        self.supervisor_command_dispatcher = Some(dispatcher);
        self
    }

    pub fn with_ticket_picker_provider(mut self, provider: Arc<dyn TicketPickerProvider>) -> Self {
        self.ticket_picker_provider = Some(provider);
        self
    }

    pub fn with_worker_backend(mut self, backend: Arc<dyn WorkerBackend>) -> Self {
        self.worker_backend = Some(backend);
        self
    }

    pub fn run(&mut self, status: &str, projection: &ProjectionState) -> io::Result<()> {
        let mut shell_state = UiShellState::new_with_integrations(
            status.to_owned(),
            projection.clone(),
            self.supervisor_provider.clone(),
            self.supervisor_command_dispatcher.clone(),
            self.ticket_picker_provider.clone(),
            self.worker_backend.clone(),
        );
        loop {
            shell_state.tick_supervisor_stream();
            shell_state.tick_ticket_picker();
            shell_state.tick_terminal_view();
            let ui_state = shell_state.ui_state();
            self.terminal.draw(|frame| {
                let area = frame.area();
                let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
                let [main, footer] = layout.areas(area);
                let main_layout =
                    Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]);
                let [left_area, center_area] = main_layout.areas(main);
                let left_layout = Layout::vertical([
                    Constraint::Percentage(45),
                    Constraint::Percentage(55),
                ]);
                let [sessions_area, inbox_area] = left_layout.areas(left_area);

                let sessions_text = render_sessions_panel(
                    &shell_state.domain,
                    shell_state.selected_session_id_for_panel().as_ref(),
                );
                frame.render_widget(
                    Paragraph::new(sessions_text)
                        .block(Block::default().title("sessions").borders(Borders::ALL)),
                    sessions_area,
                );

                let inbox_text = render_inbox_panel(&ui_state);
                frame.render_widget(
                    Paragraph::new(inbox_text)
                        .block(Block::default().title("inbox").borders(Borders::ALL)),
                    inbox_area,
                );

                let center_text = render_center_panel(&ui_state);
                if let Some(session_id) = shell_state.active_terminal_session_id() {
                    let center_layout =
                        Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(6)]);
                    let [terminal_meta_area, terminal_output_area, terminal_input_area] =
                        center_layout.areas(center_area);
                    let meta_text = render_terminal_top_bar(&shell_state.domain, session_id);
                    frame.render_widget(
                        Paragraph::new(meta_text)
                            .block(Block::default().title("terminal").borders(Borders::ALL)),
                        terminal_meta_area,
                    );

                    let output_text = render_terminal_output_panel(&ui_state);
                    let content_height =
                        estimate_wrapped_line_count(output_text.as_str(), terminal_output_area.width);
                    let viewport_height = terminal_output_area.height.saturating_sub(2);
                    let scroll_y = content_height.saturating_sub(viewport_height);
                    frame.render_widget(
                        Paragraph::new(output_text)
                            .wrap(Wrap { trim: false })
                            .scroll((scroll_y, 0))
                            .block(Block::default().title("output").borders(Borders::ALL)),
                        terminal_output_area,
                    );

                    let input_text =
                        render_terminal_input_panel(shell_state.terminal_compose_draft.as_str());
                    frame.render_widget(
                        Paragraph::new(input_text)
                            .wrap(Wrap { trim: false })
                            .block(
                                Block::default()
                                    .title("input (Enter send, Shift+Enter newline)")
                                    .borders(Borders::ALL),
                            ),
                        terminal_input_area,
                    );
                    if shell_state.mode == UiMode::Terminal {
                        if let Some((cursor_x, cursor_y)) = terminal_input_cursor(
                            terminal_input_area,
                            shell_state.terminal_compose_draft.as_str(),
                        ) {
                            frame.set_cursor_position((cursor_x, cursor_y));
                        }
                    }
                } else {
                    frame.render_widget(
                        Paragraph::new(center_text).block(
                            Block::default()
                                .title(ui_state.center_pane.title.as_str())
                                .borders(Borders::ALL),
                        ),
                        center_area,
                    );
                }

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
                if shell_state.ticket_picker_overlay.visible {
                    render_ticket_picker_overlay(frame, main, &shell_state.ticket_picker_overlay);
                }
            })?;

            if shell_state.ticket_picker_overlay.has_repository_prompt()
                || (shell_state.mode == UiMode::Terminal && shell_state.is_terminal_view_active())
            {
                let _ = io::stdout()
                    .execute(Show)
                    .and_then(|stdout| stdout.execute(SetCursorStyle::BlinkingBlock));
            } else {
                let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
            }

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
        let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
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

fn session_panel_rows(domain: &ProjectionState) -> Vec<(WorkerSessionId, String, String)> {
    let mut work_item_repo = HashMap::new();
    for event in &domain.events {
        if let OrchestrationEventPayload::WorktreeCreated(payload) = &event.payload {
            work_item_repo.insert(
                payload.work_item_id.clone(),
                repository_name_from_path(payload.path.as_str()),
            );
        }
    }

    let mut sessions_by_repo: HashMap<String, Vec<(WorkerSessionId, String)>> = HashMap::new();
    for session in domain.sessions.values() {
        if !is_open_session_status(session.status.as_ref()) {
            continue;
        }
        if let Some(work_item_id) = session.work_item_id.as_ref() {
            if domain
                .work_items
                .get(work_item_id)
                .and_then(|work_item| work_item.session_id.as_ref())
                != Some(&session.id)
            {
                continue;
            }
        }

        let repo = repository_label_for_session(session, domain, &work_item_repo);
        let status = session_status_label(session.status.as_ref());
        let work_item = session
            .work_item_id
            .as_ref()
                .map(|work_item_id| work_item_id.as_str())
                .unwrap_or("unknown");
        let line = format!("  [{}] {} - {}", status, session.id.as_str(), work_item);
        sessions_by_repo.entry(repo).or_default().push((session.id.clone(), line));
    }

    if sessions_by_repo.is_empty() {
        return Vec::new();
    }

    let mut rows = Vec::new();
    let mut repos = sessions_by_repo.keys().cloned().collect::<Vec<_>>();
    repos.sort_unstable();
    for repo in repos {
        let mut sessions = sessions_by_repo
            .remove(&repo)
            .unwrap_or_default();
        sessions.sort_unstable_by(|left, right| left.1.cmp(&right.1));
        for (session_id, line) in sessions {
            rows.push((session_id, repo.clone(), line));
        }
    }

    rows
}

fn render_sessions_panel(
    domain: &ProjectionState,
    selected_session_id: Option<&WorkerSessionId>,
) -> String {
    let session_rows = session_panel_rows(domain);
    if session_rows.is_empty() {
        return "No open sessions.".to_owned();
    }

    let mut lines = Vec::new();
    let mut previous_repo: Option<String> = None;
    for (session_id, repo, line) in session_rows {
        let is_selected = selected_session_id == Some(&session_id);
        let marker = if is_selected { ">" } else { " " };
        if previous_repo.as_deref() != Some(repo.as_str()) {
            if previous_repo.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("{repo}:"));
            previous_repo = Some(repo);
        }
        lines.push(format!("{marker}{line}"));
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

fn render_terminal_top_bar(domain: &ProjectionState, session_id: &WorkerSessionId) -> String {
    let text = if let Some(session) = domain.sessions.get(session_id) {
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
        format!(
            "session: {} | status: {} | checkpoint: {}",
            session_id.as_str(),
            status,
            checkpoint
        )
    } else {
        format!(
            "session: {} | status: unavailable | checkpoint: none",
            session_id.as_str()
        )
    };
    sanitize_terminal_display_text(text.as_str())
}

fn render_terminal_output_panel(ui_state: &UiState) -> String {
    let raw = if ui_state.center_pane.lines.is_empty() {
        "No terminal output available yet.".to_owned()
    } else {
        ui_state.center_pane.lines.join("\n")
    };
    render_markdown_for_terminal(sanitize_terminal_display_text(raw.as_str()).as_str())
}

fn render_terminal_input_panel(draft: &str) -> String {
    if draft.is_empty() {
        "Type a message to the harness here.\nPress Enter to send.".to_owned()
    } else {
        sanitize_terminal_display_text(draft)
    }
}

fn terminal_input_cursor(area: Rect, draft: &str) -> Option<(u16, u16)> {
    if area.width <= 2 || area.height <= 2 {
        return None;
    }

    let content_width = usize::from(area.width.saturating_sub(2)).max(1);
    let max_row = usize::from(area.height.saturating_sub(3));
    let mut row: usize = 0;
    let mut col: usize = 0;

    for ch in draft.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            row = row.saturating_add(1);
            col = 0;
            continue;
        }

        col = col.saturating_add(1);
        if col >= content_width {
            row = row.saturating_add(1);
            col = 0;
        }
    }

    let clamped_row = row.min(max_row);
    let max_col = content_width.saturating_sub(1);
    let clamped_col = col.min(max_col);
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(u16::try_from(clamped_col).ok()?);
    let cursor_y = area
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(clamped_row).ok()?);
    Some((cursor_x, cursor_y))
}

fn estimate_wrapped_line_count(text: &str, area_width: u16) -> u16 {
    let width = area_width.saturating_sub(2).max(1) as usize;
    let mut total: u16 = 0;
    for line in text.split('\n') {
        let chars = line.chars().count();
        let wrapped = if chars == 0 {
            1
        } else {
            1 + (chars.saturating_sub(1) / width)
        } as u16;
        total = total.saturating_add(wrapped);
    }
    total.max(1)
}

fn render_markdown_for_terminal(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let mut rendered = Vec::new();
    let mut in_code_block = false;
    for raw_line in input.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed_start = line.trim_start();

        if trimmed_start.starts_with("```") {
            if in_code_block {
                rendered.push("[/code]".to_owned());
                rendered.push(String::new());
            } else {
                rendered.push(String::new());
                rendered.push("[code]".to_owned());
            }
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            rendered.push(format!("    {line}"));
            continue;
        }

        rendered.push(render_markdown_line(line));
    }

    if rendered.is_empty() {
        String::new()
    } else {
        rendered.join("\n")
    }
}

fn render_markdown_line(line: &str) -> String {
    if line.starts_with("> ") || line.starts_with("< ") {
        return line.to_owned();
    }

    let indent = line.len().saturating_sub(line.trim_start().len());
    let indent_prefix = " ".repeat(indent);
    let trimmed = line.trim_start();

    if trimmed.is_empty() {
        return String::new();
    }

    if is_markdown_rule(trimmed) {
        return format!("{indent_prefix}{}", "-".repeat(32));
    }

    if let Some(text) = trim_markdown_heading(trimmed) {
        return format!("{indent_prefix}{}", normalize_markdown_inline(text));
    }

    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return format!("{indent_prefix}- {}", normalize_markdown_inline(rest));
    }

    if let Some(rest) = trimmed.strip_prefix("> ") {
        return format!("{indent_prefix}| {}", normalize_markdown_inline(rest));
    }

    if let Some((marker, rest)) = split_markdown_numbered_item(trimmed) {
        return format!(
            "{indent_prefix}{marker} {}",
            normalize_markdown_inline(rest)
        );
    }

    format!("{indent_prefix}{}", normalize_markdown_inline(trimmed))
}

fn is_markdown_rule(line: &str) -> bool {
    let stripped = line.trim();
    matches!(stripped, "---" | "***" | "___")
}

fn trim_markdown_heading(line: &str) -> Option<&str> {
    let mut level = 0usize;
    for ch in line.chars() {
        if ch == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level == 0 || level > 6 {
        return None;
    }
    line.get(level..)
        .map(str::trim_start)
        .filter(|value| !value.is_empty())
}

fn split_markdown_numbered_item(line: &str) -> Option<(&str, &str)> {
    let dot_idx = line.find(". ")?;
    if dot_idx == 0 {
        return None;
    }
    let marker = &line[..dot_idx];
    if !marker.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((&line[..dot_idx + 1], &line[dot_idx + 2..]))
}

fn normalize_markdown_inline(input: &str) -> String {
    let with_links = rewrite_markdown_links(input);
    with_links
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
}

fn rewrite_markdown_links(input: &str) -> String {
    let mut output = String::new();
    let mut remaining = input;

    loop {
        let Some(open) = remaining.find('[') else {
            output.push_str(remaining);
            break;
        };
        output.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(mid) = after_open.find("](") else {
            output.push('[');
            output.push_str(after_open);
            break;
        };
        let text = &after_open[..mid];
        let after_mid = &after_open[mid + 2..];
        let Some(close) = after_mid.find(')') else {
            output.push('[');
            output.push_str(after_open);
            break;
        };
        let url = &after_mid[..close];
        output.push_str(text);
        if !url.is_empty() {
            output.push_str(" (");
            output.push_str(url);
            output.push(')');
        }
        remaining = &after_mid[close + 1..];
    }

    output
}

fn sanitize_terminal_display_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\t' => output.push(ch),
            '\r' => output.push('\n'),
            _ if ch.is_control() => {}
            _ => output.push(ch),
        }
    }
    output
}

fn repository_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| "unknown-repo".to_owned(), ToOwned::to_owned)
}

fn repository_label_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    work_item_repo: &HashMap<WorkItemId, String>,
) -> String {
    session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| work_item_repo.get(work_item_id).cloned())
        .or_else(|| {
            session
                .work_item_id
                .as_ref()
                .and_then(|work_item_id| domain.work_items.get(work_item_id))
                .and_then(|work_item| work_item.project_id.as_ref())
                .map(|project_id: &ProjectId| project_id.as_str().to_owned())
        })
        .unwrap_or_else(|| "unknown-repo".to_owned())
}

fn is_open_session_status(status: Option<&WorkerSessionStatus>) -> bool {
    !matches!(
        status,
        Some(WorkerSessionStatus::Done) | Some(WorkerSessionStatus::Crashed)
    )
}

fn session_status_label(status: Option<&WorkerSessionStatus>) -> &'static str {
    match status {
        Some(WorkerSessionStatus::Running) => "running",
        Some(WorkerSessionStatus::WaitingForUser) => "waiting",
        Some(WorkerSessionStatus::Blocked) => "blocked",
        Some(WorkerSessionStatus::Done) => "done",
        Some(WorkerSessionStatus::Crashed) => "crashed",
        None => "unknown",
    }
}

fn render_ticket_picker_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    overlay: &TicketPickerOverlayState,
) {
    let content = render_ticket_picker_overlay_text(overlay);
    let Some(popup) = ticket_picker_popup(anchor_area) else {
        return;
    };
    if let Some((cursor_x, cursor_y)) = ticket_picker_repository_prompt_cursor(popup, overlay) {
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("start ticket").borders(Borders::ALL)),
        popup,
    );
}

fn ticket_picker_repository_prompt_cursor(popup: Rect, overlay: &TicketPickerOverlayState) -> Option<(u16, u16)> {
    if !overlay.has_repository_prompt() {
        return None;
    }

    let mut line_index = 1usize;
    if overlay.loading {
        line_index += 1;
    }
    if overlay.starting_ticket_id.is_some() {
        line_index += 1;
    }
    if overlay.error.is_some() {
        line_index += 1;
    }

    // Blank spacer
    line_index += 1;
    // Repository project info line
    line_index += 1;
    // Prompt guidance line
    line_index += 1;
    // Cursor line is the repository path input line.

    let inner_height = popup.height.saturating_sub(2);
    if line_index >= usize::from(inner_height) {
        return None;
    }

    let prefix_len = "Path: ".chars().count();
    let input_len = overlay.repository_prompt_input.chars().count();
    let cursor_offset = prefix_len + input_len;

    let inner_width = popup.width.saturating_sub(2);
    let max_offset = inner_width.saturating_sub(1);
    let cursor_offset = cursor_offset.min(usize::from(max_offset));

    let cursor_x = popup.x.saturating_add(1).saturating_add(u16::try_from(cursor_offset).ok()?);
    let cursor_y = popup.y
        .saturating_add(1)
        .saturating_add(u16::try_from(line_index).ok()?);

    Some((cursor_x, cursor_y))
}

fn ticket_picker_popup(anchor_area: Rect) -> Option<Rect> {
    if anchor_area.width < 20 || anchor_area.height < 10 {
        return None;
    }

    let width = ((anchor_area.width as f32) * 0.78).round() as u16;
    let height = ((anchor_area.height as f32) * 0.82).round() as u16;
    let width = width.clamp(20, anchor_area.width.saturating_sub(2));
    let height = height.clamp(10, anchor_area.height.saturating_sub(2));

    Some(Rect {
        x: anchor_area.x + (anchor_area.width.saturating_sub(width)) / 2,
        y: anchor_area.y + (anchor_area.height.saturating_sub(height)) / 2,
        width,
        height,
    })
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

fn ticket_picker_priority_states_from_env() -> Vec<String> {
    match std::env::var(TICKET_PICKER_PRIORITY_STATES_ENV) {
        Ok(raw) => {
            let parsed = raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if parsed.is_empty() {
                TICKET_PICKER_PRIORITY_STATES_DEFAULT
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect()
            } else {
                parsed
            }
        }
        Err(_) => TICKET_PICKER_PRIORITY_STATES_DEFAULT
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    }
}

fn normalize_ticket_state(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_ticket_project(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn is_unfinished_ticket_state(state: &str) -> bool {
    let normalized = normalize_ticket_state(state);
    !matches!(
        normalized.as_str(),
        "done" | "completed" | "canceled" | "cancelled"
    )
}

fn ticket_project_name(ticket: &TicketSummary) -> String {
    ticket
        .project
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("No Project")
        .to_owned()
}

fn group_tickets_by_project(
    tickets: Vec<TicketSummary>,
    priority_states: &[String],
    collapsed_projects: &HashSet<String>,
) -> Vec<TicketProjectGroup> {
    let mut projects = tickets
        .into_iter()
        .filter(|ticket| is_unfinished_ticket_state(ticket.state.as_str()))
        .fold(
            Vec::<(String, Vec<TicketSummary>)>::new(),
            |mut project_buckets, ticket| {
                let project_name = ticket_project_name(&ticket);
                if let Some(existing) = project_buckets
                    .iter_mut()
                    .find(|(name, _)| name.eq_ignore_ascii_case(project_name.as_str()))
                {
                    existing.1.push(ticket);
                } else {
                    project_buckets.push((project_name, vec![ticket]));
                }
                project_buckets
            },
        );

    projects.sort_by(|left, right| {
        normalize_ticket_project(left.0.as_str()).cmp(&normalize_ticket_project(right.0.as_str()))
    });

    projects
        .into_iter()
        .map(|(project, project_tickets)| TicketProjectGroup {
            collapsed: collapsed_projects.contains(&normalize_ticket_project(project.as_str())),
            project,
            status_groups: group_tickets_by_status(project_tickets, priority_states),
        })
        .collect()
}

fn group_tickets_by_status(
    tickets: Vec<TicketSummary>,
    priority_states: &[String],
) -> Vec<TicketStatusGroup> {
    let mut groups =
        tickets
            .into_iter()
            .fold(Vec::<TicketStatusGroup>::new(), |mut groups, ticket| {
                if let Some(existing) = groups
                    .iter_mut()
                    .find(|group| group.status.eq_ignore_ascii_case(ticket.state.as_str()))
                {
                    existing.tickets.push(ticket);
                } else {
                    groups.push(TicketStatusGroup {
                        status: ticket.state.clone(),
                        tickets: vec![ticket],
                    });
                }
                groups
            });

    for group in &mut groups {
        group.tickets.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.identifier.cmp(&right.identifier))
        });
    }

    groups.sort_by(|left, right| {
        left.status
            .to_ascii_lowercase()
            .cmp(&right.status.to_ascii_lowercase())
    });

    let mut ordered = Vec::with_capacity(groups.len());
    for priority_state in priority_states {
        if let Some(index) = groups.iter().position(|group| {
            normalize_ticket_state(group.status.as_str())
                == normalize_ticket_state(priority_state.as_str())
        }) {
            ordered.push(groups.remove(index));
        }
    }
    ordered.extend(groups);
    ordered
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiCommand {
    EnterNormalMode,
    EnterInsertMode,
    ToggleGlobalSupervisorChat,
    OpenTerminalForSelected,
    OpenDiffInspectorForSelected,
    OpenTestInspectorForSelected,
    OpenPrInspectorForSelected,
    OpenChatInspectorForSelected,
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
    OpenTicketPicker,
    CloseTicketPicker,
    TicketPickerMoveNext,
    TicketPickerMovePrevious,
    TicketPickerFoldProject,
    TicketPickerUnfoldProject,
    TicketPickerStartSelected,
    OpenFocusCard,
    AdvanceTerminalWorkflowStage,
    KillSelectedSession,
    MinimizeCenterView,
}

impl UiCommand {
    const ALL: [Self; 31] = [
        Self::EnterNormalMode,
        Self::EnterInsertMode,
        Self::ToggleGlobalSupervisorChat,
        Self::OpenTerminalForSelected,
        Self::OpenDiffInspectorForSelected,
        Self::OpenTestInspectorForSelected,
        Self::OpenPrInspectorForSelected,
        Self::OpenChatInspectorForSelected,
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
        Self::OpenTicketPicker,
        Self::CloseTicketPicker,
        Self::TicketPickerMoveNext,
        Self::TicketPickerMovePrevious,
        Self::TicketPickerFoldProject,
        Self::TicketPickerUnfoldProject,
        Self::TicketPickerStartSelected,
        Self::OpenFocusCard,
        Self::AdvanceTerminalWorkflowStage,
        Self::KillSelectedSession,
        Self::MinimizeCenterView,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "ui.mode.normal",
            Self::EnterInsertMode => "ui.mode.insert",
            Self::ToggleGlobalSupervisorChat => "ui.supervisor_chat.toggle",
            Self::OpenTerminalForSelected => command_ids::UI_OPEN_TERMINAL_FOR_SELECTED,
            Self::OpenDiffInspectorForSelected => command_ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
            Self::OpenTestInspectorForSelected => command_ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
            Self::OpenPrInspectorForSelected => command_ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
            Self::OpenChatInspectorForSelected => command_ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
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
            Self::OpenTicketPicker => "ui.ticket_picker.open",
            Self::CloseTicketPicker => "ui.ticket_picker.close",
            Self::TicketPickerMoveNext => "ui.ticket_picker.move_next",
            Self::TicketPickerMovePrevious => "ui.ticket_picker.move_previous",
            Self::TicketPickerFoldProject => "ui.ticket_picker.fold_project",
            Self::TicketPickerUnfoldProject => "ui.ticket_picker.unfold_project",
            Self::TicketPickerStartSelected => "ui.ticket_picker.start_selected",
            Self::OpenFocusCard => "ui.open_focus_card_for_selected",
            Self::AdvanceTerminalWorkflowStage => "ui.terminal.workflow.advance",
            Self::KillSelectedSession => "ui.terminal.kill_selected_session",
            Self::MinimizeCenterView => "ui.center.pop",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "Return to Normal mode",
            Self::EnterInsertMode => "Enter Insert mode",
            Self::ToggleGlobalSupervisorChat => "Toggle global supervisor chat panel",
            Self::OpenTerminalForSelected => "Open terminal for selected item",
            Self::OpenDiffInspectorForSelected => "Open diff inspector for selected item",
            Self::OpenTestInspectorForSelected => "Open test inspector for selected item",
            Self::OpenPrInspectorForSelected => "Open PR inspector for selected item",
            Self::OpenChatInspectorForSelected => "Open chat inspector for selected item",
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
            Self::OpenTicketPicker => "Open ticket picker",
            Self::CloseTicketPicker => "Close ticket picker",
            Self::TicketPickerMoveNext => "Move to next ticket picker row",
            Self::TicketPickerMovePrevious => "Move to previous ticket picker row",
            Self::TicketPickerFoldProject => "Fold selected project in ticket picker",
            Self::TicketPickerUnfoldProject => "Unfold selected project in ticket picker",
            Self::TicketPickerStartSelected => "Start selected ticket",
            Self::OpenFocusCard => "Open focus card for selected item",
            Self::AdvanceTerminalWorkflowStage => "Advance terminal workflow stage",
            Self::KillSelectedSession => "Kill selected terminal session",
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
                    binding(&["s"], UiCommand::OpenTicketPicker),
                    binding(&["c"], UiCommand::ToggleGlobalSupervisorChat),
                    binding(&["enter"], UiCommand::OpenFocusCard),
                    binding(&["t"], UiCommand::OpenTerminalForSelected),
                    binding(&["w"], UiCommand::AdvanceTerminalWorkflowStage),
                    binding(&["x"], UiCommand::KillSelectedSession),
                    binding(&["backspace"], UiCommand::MinimizeCenterView),
                    binding(&["i"], UiCommand::EnterInsertMode),
                    binding(&["z", "1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["z", "2"], UiCommand::JumpBatchApprovals),
                    binding(&["z", "3"], UiCommand::JumpBatchReviewReady),
                    binding(&["z", "4"], UiCommand::JumpBatchFyiDigest),
                    binding(&["v", "d"], UiCommand::OpenDiffInspectorForSelected),
                    binding(&["v", "t"], UiCommand::OpenTestInspectorForSelected),
                    binding(&["v", "p"], UiCommand::OpenPrInspectorForSelected),
                    binding(&["v", "c"], UiCommand::OpenChatInspectorForSelected),
                ],
                prefixes: vec![
                    KeyPrefixConfig {
                        keys: vec!["z".to_owned()],
                        label: "Batch jumps".to_owned(),
                    },
                    KeyPrefixConfig {
                        keys: vec!["v".to_owned()],
                        label: "Artifact inspectors".to_owned(),
                    },
                ],
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoutedInput {
    Command(UiCommand),
    UnsupportedCommand { command_id: String },
    Ignore,
}

fn mode_help(mode: UiMode) -> &'static str {
    match mode {
        UiMode::Normal => {
            "j/k: select | Tab/S-Tab: batch cycle | 1-4 or z{1-4}: batch jump | g/G: first/last | s: start ticket | c: supervisor chat | Enter: focus | t: terminal | w: advance session workflow | x: kill selected session | v{d/t/p/c}: inspectors | i: insert | q: quit"
        }
        UiMode::Insert => "Insert input active | Esc/Ctrl-[: Normal",
        UiMode::Terminal => "Terminal compose active | Enter send | Shift+Enter newline | Esc or Ctrl-\\ Ctrl-n: Normal | then w: advance workflow",
    }
}

fn append_terminal_output(state: &mut TerminalViewState, bytes: Vec<u8>) {
    let chunk = sanitize_terminal_display_text(String::from_utf8_lossy(&bytes).as_ref());
    if chunk.is_empty() {
        return;
    }

    let mut combined = String::new();
    combined.push_str(state.output_fragment.as_str());
    combined.push_str(chunk.as_str());
    state.output_fragment.clear();

    let mut lines = combined.split('\n').collect::<Vec<_>>();
    if lines.is_empty() {
        return;
    }

    if !combined.ends_with('\n') {
        state.output_fragment = lines.pop().unwrap_or_default().to_owned();
    }

    for raw_line in lines {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let marker = if is_outgoing_transcript_line(line) { '>' } else { '<' };
        state.lines.push(format!("{marker} {line}"));
    }
}

fn append_terminal_assistant_output(state: &mut TerminalViewState, bytes: Vec<u8>) {
    append_terminal_output(state, bytes);
}

fn append_terminal_user_message(state: &mut TerminalViewState, message: &str) {
    flush_terminal_output_fragment(state);
    let text = sanitize_terminal_display_text(message);
    if text.trim().is_empty() {
        return;
    }

    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if index == 0 {
            state.lines.push(format!("> you: {line}"));
        } else {
            state.lines.push(format!("> {line}"));
        }
    }
}

fn append_terminal_system_message(state: &mut TerminalViewState, message: &str) {
    flush_terminal_output_fragment(state);
    let text = sanitize_terminal_display_text(message);
    if text.trim().is_empty() {
        return;
    }

    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if index == 0 {
            state.lines.push(format!("> system: {line}"));
        } else {
            state.lines.push(format!("> {line}"));
        }
    }
}

fn is_outgoing_transcript_line(line: &str) -> bool {
    let normalized = line.trim_start();
    normalized.starts_with("system:")
        || normalized.starts_with("you:")
        || normalized.starts_with("user:")
}

fn flush_terminal_output_fragment(state: &mut TerminalViewState) {
    let fragment = std::mem::take(&mut state.output_fragment);
    let line = fragment.trim_end_matches('\r');
    if line.trim().is_empty() {
        return;
    }

    let marker = if is_outgoing_transcript_line(line) { '>' } else { '<' };
    state.lines.push(format!("{marker} {line}"));
}

fn handle_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> bool {
    match route_key_press(shell_state, key) {
        RoutedInput::Command(command) => dispatch_command(shell_state, command),
        RoutedInput::UnsupportedCommand { command_id } => {
            shell_state.status_warning = Some(format!(
                "unsupported command mapping '{}' in {} mode keymap",
                command_id,
                shell_state.mode.label()
            ));
            false
        }
        RoutedInput::Ignore => false,
    }
}

fn route_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.is_ticket_picker_visible() {
        return route_ticket_picker_key(shell_state, key);
    }

    if is_escape_to_normal(key) {
        if shell_state.is_active_supervisor_stream_visible()
            && !shell_state.is_global_supervisor_chat_active()
        {
            shell_state.cancel_supervisor_stream();
        }
        return RoutedInput::Command(UiCommand::EnterNormalMode);
    }
    if is_ctrl_char(key, 'c') && shell_state.is_active_supervisor_stream_visible() {
        shell_state.cancel_supervisor_stream();
        return RoutedInput::Ignore;
    }
    if shell_state.apply_global_chat_insert_key(key) {
        return RoutedInput::Ignore;
    }
    if shell_state.apply_terminal_compose_key(key) {
        return RoutedInput::Ignore;
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
                .unwrap_or(RoutedInput::UnsupportedCommand { command_id })
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

fn route_terminal_mode_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.terminal_escape_pending {
        if is_ctrl_char(key, 'n') {
            return RoutedInput::Command(UiCommand::EnterNormalMode);
        }
        shell_state.terminal_escape_pending = false;
        return RoutedInput::Ignore;
    }

    if is_ctrl_char(key, '\\') {
        return RoutedInput::Command(UiCommand::StartTerminalEscapeChord);
    }

    RoutedInput::Ignore
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
        UiCommand::ToggleGlobalSupervisorChat => {
            shell_state.toggle_global_supervisor_chat();
            false
        }
        UiCommand::OpenTerminalForSelected => {
            shell_state.open_terminal_and_enter_mode();
            false
        }
        UiCommand::OpenDiffInspectorForSelected => {
            shell_state.open_inspector_for_selected(ArtifactInspectorKind::Diff);
            false
        }
        UiCommand::OpenTestInspectorForSelected => {
            shell_state.open_inspector_for_selected(ArtifactInspectorKind::Test);
            false
        }
        UiCommand::OpenPrInspectorForSelected => {
            shell_state.open_inspector_for_selected(ArtifactInspectorKind::PullRequest);
            false
        }
        UiCommand::OpenChatInspectorForSelected => {
            shell_state.open_chat_inspector_for_selected();
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
        UiCommand::OpenTicketPicker => {
            shell_state.open_ticket_picker();
            false
        }
        UiCommand::CloseTicketPicker => {
            shell_state.close_ticket_picker();
            false
        }
        UiCommand::TicketPickerMoveNext => {
            shell_state.move_ticket_picker_selection(1);
            false
        }
        UiCommand::TicketPickerMovePrevious => {
            shell_state.move_ticket_picker_selection(-1);
            false
        }
        UiCommand::TicketPickerFoldProject => {
            shell_state.fold_ticket_picker_selected_project();
            false
        }
        UiCommand::TicketPickerUnfoldProject => {
            shell_state.unfold_ticket_picker_selected_project();
            false
        }
        UiCommand::TicketPickerStartSelected => {
            shell_state.start_selected_ticket_from_picker();
            false
        }
        UiCommand::OpenFocusCard => {
            shell_state.open_focus_card_for_selected();
            false
        }
        UiCommand::AdvanceTerminalWorkflowStage => {
            shell_state.advance_terminal_workflow_stage();
            false
        }
        UiCommand::KillSelectedSession => {
            shell_state.kill_selected_session();
            false
        }
        UiCommand::MinimizeCenterView => {
            if shell_state.is_global_supervisor_chat_active() {
                shell_state.close_global_supervisor_chat();
            } else {
                shell_state.minimize_center_view();
            }
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
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use orchestrator_core::{
        ArtifactId, ArtifactKind, ArtifactProjection, CoreError, InboxItemProjection,
        LlmProviderKind, LlmResponseStream, LlmResponseSubscription, LlmStreamChunk,
        OrchestrationEventPayload, OrchestrationEventType, SessionBlockedPayload,
        SessionCheckpointPayload, SessionNeedsInputPayload, SessionProjection, StoredEventEnvelope,
        SupervisorQueryFinishedPayload, UserRespondedPayload, WorkItemProjection, WorkflowState,
    };
    use orchestrator_runtime::{
        BackendCapabilities, BackendEvent, BackendKind, RuntimeResult, RuntimeSessionId,
        SessionHandle, SessionLifecycle, WorkerEventStream,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug)]
    struct TestLlmStream {
        chunks: VecDeque<Result<LlmStreamChunk, CoreError>>,
    }

    #[async_trait]
    impl LlmResponseSubscription for TestLlmStream {
        async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
            match self.chunks.pop_front() {
                Some(Ok(chunk)) => Ok(Some(chunk)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        }
    }

    #[derive(Debug)]
    struct TestLlmProvider {
        chunks: Mutex<Option<Vec<Result<LlmStreamChunk, CoreError>>>>,
        cancelled_streams: Mutex<Vec<String>>,
    }

    impl TestLlmProvider {
        fn new(chunks: Vec<Result<LlmStreamChunk, CoreError>>) -> Self {
            Self {
                chunks: Mutex::new(Some(chunks)),
                cancelled_streams: Mutex::new(Vec::new()),
            }
        }

        fn cancelled_streams(&self) -> Vec<String> {
            self.cancelled_streams
                .lock()
                .expect("cancelled stream lock")
                .clone()
        }
    }

    #[async_trait]
    impl LlmProvider for TestLlmProvider {
        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::Other("test".to_owned())
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn stream_chat(
            &self,
            _request: LlmChatRequest,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            let chunks = self
                .chunks
                .lock()
                .expect("stream chunk lock")
                .take()
                .unwrap_or_default();
            let stream = TestLlmStream {
                chunks: chunks.into(),
            };
            Ok(("test-stream".to_owned(), Box::new(stream)))
        }

        async fn cancel_stream(&self, stream_id: &str) -> Result<(), CoreError> {
            self.cancelled_streams
                .lock()
                .expect("cancelled stream lock")
                .push(stream_id.to_owned());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TestSupervisorDispatcher {
        requests: Mutex<Vec<(UntypedCommandInvocation, SupervisorCommandContext)>>,
        chunks: Mutex<Option<Vec<Result<LlmStreamChunk, CoreError>>>>,
        cancelled_streams: Mutex<Vec<String>>,
    }

    impl TestSupervisorDispatcher {
        fn new(chunks: Vec<Result<LlmStreamChunk, CoreError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                chunks: Mutex::new(Some(chunks)),
                cancelled_streams: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(UntypedCommandInvocation, SupervisorCommandContext)> {
            self.requests
                .lock()
                .expect("dispatcher request lock")
                .clone()
        }

        fn cancelled_streams(&self) -> Vec<String> {
            self.cancelled_streams
                .lock()
                .expect("dispatcher cancel lock")
                .clone()
        }
    }

    #[async_trait]
    impl SupervisorCommandDispatcher for TestSupervisorDispatcher {
        async fn dispatch_supervisor_command(
            &self,
            invocation: UntypedCommandInvocation,
            context: SupervisorCommandContext,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            self.requests
                .lock()
                .expect("dispatcher request lock")
                .push((invocation, context));
            let chunks = self
                .chunks
                .lock()
                .expect("dispatcher stream lock")
                .take()
                .unwrap_or_default();
            Ok((
                "dispatcher-stream".to_owned(),
                Box::new(TestLlmStream {
                    chunks: chunks.into(),
                }),
            ))
        }

        async fn cancel_supervisor_command(&self, stream_id: &str) -> Result<(), CoreError> {
            self.cancelled_streams
                .lock()
                .expect("dispatcher cancel lock")
                .push(stream_id.to_owned());
            Ok(())
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[derive(Default, Debug)]
    struct ManualTerminalBackend {
        spawned_session_ids: Arc<Mutex<Vec<RuntimeSessionId>>>,
    }

    impl ManualTerminalBackend {
        fn spawned_session_ids(&self) -> Vec<RuntimeSessionId> {
            self.spawned_session_ids
                .lock()
                .expect("spawned session IDs lock")
                .clone()
        }
    }

    struct EmptyEventStream;

    #[async_trait]
    impl orchestrator_runtime::WorkerEventSubscription for EmptyEventStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            Ok(None)
        }
    }

    #[async_trait]
    impl SessionLifecycle for ManualTerminalBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.spawned_session_ids
                .lock()
                .expect("spawned session IDs lock")
                .push(spec.session_id.clone());
            Ok(SessionHandle {
                session_id: spec.session_id,
                backend: BackendKind::OpenCode,
            })
        }

        async fn kill(&self, _session: &SessionHandle) -> RuntimeResult<()> {
            Ok(())
        }

        async fn send_input(&self, _session: &SessionHandle, _input: &[u8]) -> RuntimeResult<()> {
            Ok(())
        }

        async fn resize(
            &self,
            _session: &SessionHandle,
            _cols: u16,
            _rows: u16,
        ) -> RuntimeResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl WorkerBackend for ManualTerminalBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::OpenCode
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            Ok(())
        }

        async fn subscribe(&self, _session: &SessionHandle) -> RuntimeResult<WorkerEventStream> {
            Ok(Box::new(EmptyEventStream))
        }
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

    fn inspector_projection() -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-inspector");
        let session_id = WorkerSessionId::new("sess-inspector");
        let inbox_item_id = InboxItemId::new("inbox-inspector");
        let diff_artifact_id = ArtifactId::new("artifact-diff");
        let test_artifact_id = ArtifactId::new("artifact-test");
        let pr_artifact_id = ArtifactId::new("artifact-pr");
        let chat_artifact_id = ArtifactId::new("artifact-chat");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Testing),
                session_id: Some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![
                    diff_artifact_id.clone(),
                    test_artifact_id.clone(),
                    pr_artifact_id.clone(),
                    chat_artifact_id.clone(),
                ],
            },
        );
        projection.sessions.insert(
            session_id.clone(),
            SessionProjection {
                id: session_id.clone(),
                work_item_id: Some(work_item_id.clone()),
                status: Some(WorkerSessionStatus::WaitingForUser),
                latest_checkpoint: Some(test_artifact_id.clone()),
            },
        );
        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id: work_item_id.clone(),
                kind: InboxItemKind::NeedsApproval,
                title: "Inspect generated artifacts".to_owned(),
                resolved: false,
            },
        );
        projection.artifacts.insert(
            diff_artifact_id.clone(),
            ArtifactProjection {
                id: diff_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::Diff,
                label: "Feature branch delta".to_owned(),
                uri: "artifact://diff/wi-inspector?files=3&insertions=42&deletions=9".to_owned(),
            },
        );
        projection.artifacts.insert(
            test_artifact_id.clone(),
            ArtifactProjection {
                id: test_artifact_id.clone(),
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::TestRun,
                label: "cargo test -p orchestrator-ui".to_owned(),
                uri: "artifact://tests/wi-inspector?tail=thread_main_panicked%3A+line+42"
                    .to_owned(),
            },
        );
        projection.artifacts.insert(
            pr_artifact_id.clone(),
            ArtifactProjection {
                id: pr_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::PR,
                label: "Draft PR #47".to_owned(),
                uri: "https://github.com/acme/orchestrator/pull/47?draft=true".to_owned(),
            },
        );
        projection.artifacts.insert(
            chat_artifact_id.clone(),
            ArtifactProjection {
                id: chat_artifact_id,
                work_item_id: work_item_id.clone(),
                kind: ArtifactKind::Export,
                label: "Supervisor output".to_owned(),
                uri: "artifact://chat/wi-inspector".to_owned(),
            },
        );
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-checkpoint".to_owned(),
            sequence: 1,
            occurred_at: "2026-02-16T09:00:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionCheckpoint,
            payload: OrchestrationEventPayload::SessionCheckpoint(SessionCheckpointPayload {
                session_id: session_id.clone(),
                artifact_id: test_artifact_id,
                summary: "Ran 112 tests and captured the failing tail".to_owned(),
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-blocked".to_owned(),
            sequence: 2,
            occurred_at: "2026-02-16T09:01:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::SessionBlocked,
            payload: OrchestrationEventPayload::SessionBlocked(SessionBlockedPayload {
                session_id: session_id.clone(),
                reason: "cargo test fails in inspector pane tests".to_owned(),
                hint: None,
                log_ref: None,
            }),
            schema_version: 1,
        });
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-response".to_owned(),
            sequence: 3,
            occurred_at: "2026-02-16T09:02:00Z".to_owned(),
            work_item_id: Some(work_item_id.clone()),
            session_id: Some(session_id.clone()),
            event_type: OrchestrationEventType::UserResponded,
            payload: OrchestrationEventPayload::UserResponded(UserRespondedPayload {
                session_id: Some(session_id),
                work_item_id: Some(work_item_id),
                message: "Please summarize the supervisor output.".to_owned(),
            }),
            schema_version: 1,
        });

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
                prompt_id: Some("q1".to_owned()),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("A".to_owned()),
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
                prompt_id: Some("q-active".to_owned()),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("A".to_owned()),
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
                prompt_id: Some("q-prior".to_owned()),
                options: vec!["A".to_owned(), "B".to_owned()],
                default_option: Some("B".to_owned()),
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

    fn attach_supervisor_stream(
        shell_state: &mut UiShellState,
        work_item_id: &str,
    ) -> mpsc::Sender<SupervisorStreamEvent> {
        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        shell_state.supervisor_chat_stream = Some(ActiveSupervisorChatStream::new(
            SupervisorStreamTarget::Inspector {
                work_item_id: WorkItemId::new(work_item_id),
            },
            receiver,
        ));
        sender
    }

    fn attach_global_supervisor_stream(
        shell_state: &mut UiShellState,
    ) -> mpsc::Sender<SupervisorStreamEvent> {
        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        shell_state.supervisor_chat_stream = Some(ActiveSupervisorChatStream::new(
            SupervisorStreamTarget::GlobalChatPanel,
            receiver,
        ));
        sender
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

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
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

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
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

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
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

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
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
    fn open_terminal_with_active_session_focuses_terminal() {
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
    fn open_terminal_without_session_opens_manual_terminal_when_backend_available() {
        let backend = Arc::new(ManualTerminalBackend::default());
        let mut without_session =
            UiShellState::new_with_integrations(
                "ready".to_owned(),
                sample_projection(false),
                None,
                None,
                None,
                Some(backend.clone()),
            );

        without_session.open_terminal_for_selected();
        assert_eq!(without_session.view_stack.center_views().len(), 2);
        assert!(matches!(
            without_session.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));
        assert_eq!(backend.spawned_session_ids().len(), 1);
        assert!(matches!(
            without_session
                .view_stack
                .center_views()
                .first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-1"
        ));
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
    fn open_inspector_pushes_focus_and_inspector_for_selected_item() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Diff);
        assert_eq!(shell_state.view_stack.center_views().len(), 2);
        assert!(matches!(
            shell_state.view_stack.center_views().first(),
            Some(CenterView::FocusCardView { inbox_item_id }) if inbox_item_id.as_str() == "inbox-inspector"
        ));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InspectorView {
                work_item_id,
                inspector: ArtifactInspectorKind::Diff
            }) if work_item_id.as_str() == "wi-inspector"
        ));

        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Diff);
        assert_eq!(shell_state.view_stack.center_views().len(), 2);
    }

    #[test]
    fn artifact_inspector_projects_diff_test_pr_and_chat_context() {
        let projection = inspector_projection();
        let work_item_id = WorkItemId::new("wi-inspector");
        let stack = |inspector| {
            let mut stack = ViewStack::default();
            stack.replace_center(CenterView::InspectorView {
                work_item_id: work_item_id.clone(),
                inspector,
            });
            stack
        };

        let diff_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::Diff),
            None,
            None,
            None,
        );
        let diff_rendered = diff_state.center_pane.lines.join("\n");
        assert!(diff_rendered.contains("Diff artifacts:"));
        assert!(diff_rendered.contains("Diffstat: 3 files changed, +42/-9"));

        let test_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::Test),
            None,
            None,
            None,
        );
        let test_rendered = test_state.center_pane.lines.join("\n");
        assert!(test_rendered.contains("Test artifacts:"));
        assert!(test_rendered.contains("Latest test tail: thread_main_panicked: line 42"));
        assert!(test_rendered.contains("Latest blocker reason:"));

        let pr_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::PullRequest),
            None,
            None,
            None,
        );
        let pr_rendered = pr_state.center_pane.lines.join("\n");
        assert!(pr_rendered.contains("PR artifacts:"));
        assert!(pr_rendered.contains("PR metadata: #47 (draft)"));
        assert!(pr_rendered.contains("github.com/acme/orchestrator/pull/47"));

        let chat_state = project_ui_state(
            "ready",
            &projection,
            &stack(ArtifactInspectorKind::Chat),
            None,
            None,
            None,
        );
        let chat_rendered = chat_state.center_pane.lines.join("\n");
        assert!(chat_rendered.contains("Supervisor output:"));
        assert!(chat_rendered.contains("you: Please summarize the supervisor output."));
        assert!(chat_rendered.contains("artifact://chat/wi-inspector"));
    }

    #[test]
    fn chat_inspector_surfaces_latest_supervisor_query_metrics() {
        let mut projection = inspector_projection();
        projection.events.push(StoredEventEnvelope {
            event_id: "evt-inspector-supervisor-finished".to_owned(),
            sequence: 4,
            occurred_at: "2026-02-16T09:03:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-inspector")),
            session_id: Some(WorkerSessionId::new("sess-inspector")),
            event_type: OrchestrationEventType::SupervisorQueryFinished,
            payload: OrchestrationEventPayload::SupervisorQueryFinished(
                SupervisorQueryFinishedPayload {
                    query_id: "supq-1".to_owned(),
                    stream_id: "stream-1".to_owned(),
                    started_at: "2026-02-16T09:02:58Z".to_owned(),
                    finished_at: "2026-02-16T09:03:00Z".to_owned(),
                    duration_ms: 2010,
                    finish_reason: LlmFinishReason::Stop,
                    chunk_count: 6,
                    output_chars: 412,
                    usage: Some(LlmTokenUsage {
                        input_tokens: 144,
                        output_tokens: 41,
                        total_tokens: 185,
                    }),
                    error: None,
                    cancellation_source: None,
                },
            ),
            schema_version: 1,
        });

        let work_item_id = WorkItemId::new("wi-inspector");
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::InspectorView {
            work_item_id,
            inspector: ArtifactInspectorKind::Chat,
        });
        let chat_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = chat_state.center_pane.lines.join("\n");

        assert!(rendered.contains("Latest query metrics: id=supq-1"));
        assert!(rendered.contains("duration=2010ms"));
        assert!(rendered.contains("usage(input=144 output=41 total=185)"));
    }

    #[test]
    fn chat_stream_coalesces_chunks_and_renders_incrementally() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Started {
                stream_id: "stream-1".to_owned(),
            })
            .expect("send stream started");
        sender
            .try_send(SupervisorStreamEvent::Delta {
                text: "Recommended response:\n".to_owned(),
            })
            .expect("send chunk one");
        sender
            .try_send(SupervisorStreamEvent::Delta {
                text: "Please rerun tests with --nocapture and post the failing assertion."
                    .to_owned(),
            })
            .expect("send chunk two");

        shell_state.poll_supervisor_stream_events();
        let buffered_view = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(buffered_view.contains("Buffering:"));
        assert!(buffered_view.contains("State: streaming"));

        shell_state.tick_supervisor_stream();
        let flushed_view = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(flushed_view.contains("Live supervisor stream:"));
        assert!(flushed_view.contains("Backpressure: coalesced 2 chunks"));
        assert!(flushed_view.contains("Recommended response:"));
        assert!(flushed_view.contains("Please rerun tests with --nocapture"));
    }

    #[test]
    fn esc_cancels_active_chat_stream() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
        }

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.lifecycle, SupervisorStreamLifecycle::Cancelling);
        assert!(stream.pending_cancel);
    }

    #[test]
    fn ctrl_c_cancels_active_chat_stream_without_mode_change() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        shell_state.enter_insert_mode();
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
        }

        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('c')));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Insert);

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.lifecycle, SupervisorStreamLifecycle::Cancelling);
    }

    #[test]
    fn esc_in_global_chat_returns_to_normal_without_cancelling_stream() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        let _sender = attach_global_supervisor_stream(&mut shell_state);
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
        }

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert!(!should_quit);
        assert_eq!(shell_state.mode, UiMode::Normal);
        assert!(shell_state.is_global_supervisor_chat_active());

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.lifecycle, SupervisorStreamLifecycle::Streaming);
        assert!(!stream.pending_cancel);
    }

    #[test]
    fn chat_stream_rate_limit_errors_surface_in_status_warning() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed: HTTP 429 rate limit exceeded".to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let ui_state = shell_state.ui_state();
        assert!(ui_state.status.contains("rate-limit"));
        assert!(ui_state.status.contains("warning"));
        let rendered = ui_state.center_pane.lines.join("\n");
        assert!(rendered.contains("Error: OpenRouter request failed"));
        assert!(rendered.contains("Supervisor state: rate-limited"));
        assert!(rendered.contains("Retry guidance:"));
    }

    #[test]
    fn global_chat_empty_query_sets_no_context_state_with_fallback_prompts() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("empty global query should set terminal state");
        assert_eq!(stream.response_state, SupervisorResponseState::NoContext);
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: no-context"));
        assert!(rendered.contains("Safe fallback prompts:"));
    }

    #[test]
    fn rate_limit_error_with_retry_after_surfaces_cooldown_hint() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message:
                    "OpenRouter request failed with status 429: quota exhausted. Retry after 17s."
                        .to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: rate-limited"));
        assert!(rendered.contains("Cooldown: retry after 17s"));
    }

    #[test]
    fn rate_limit_error_with_reset_at_surfaces_reset_cooldown_hint() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed with status 429: quota exhausted. Retry after rate limit reset at 2026-02-17T12:00:00Z. This is recoverable; retry with a smaller context or wait for cooldown.".to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Cooldown: rate limit reset at 2026-02-17T12:00:00Z"));
        assert!(!rendered.contains("Cooldown: retry after rate"));
    }

    #[test]
    fn auth_failure_clears_prior_rate_limit_cooldown_hint() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message:
                    "OpenRouter request failed with status 429: quota exhausted. Retry after 17s."
                        .to_owned(),
            })
            .expect("send rate limit failure");
        shell_state.poll_supervisor_stream_events();

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed with status 401: unauthorized".to_owned(),
            })
            .expect("send auth failure");
        shell_state.poll_supervisor_stream_events();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: auth-unavailable"));
        assert!(!rendered.contains("Cooldown:"));
    }

    #[test]
    fn auth_failures_surface_auth_unavailable_state() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Failed {
                message: "OpenRouter request failed with status 401: unauthorized".to_owned(),
            })
            .expect("send failure");
        shell_state.poll_supervisor_stream_events();

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(
            stream.response_state,
            SupervisorResponseState::AuthUnavailable
        );
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: auth-unavailable"));
    }

    #[test]
    fn high_cost_usage_sets_high_cost_state_with_guidance() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Usage {
                usage: LlmTokenUsage {
                    input_tokens: 700,
                    output_tokens: 260,
                    total_tokens: 960,
                },
            })
            .expect("send usage");
        sender
            .try_send(SupervisorStreamEvent::Finished {
                reason: LlmFinishReason::Stop,
                usage: None,
            })
            .expect("send finished");
        shell_state.tick_supervisor_stream();

        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("stream state present");
        assert_eq!(stream.response_state, SupervisorResponseState::HighCost);
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: high-cost"));
        assert!(rendered.contains("Safe fallback prompts:"));
    }

    #[test]
    fn chat_stream_usage_updates_surface_in_chat_inspector() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");

        sender
            .try_send(SupervisorStreamEvent::Usage {
                usage: LlmTokenUsage {
                    input_tokens: 120,
                    output_tokens: 45,
                    total_tokens: 165,
                },
            })
            .expect("send usage");
        sender
            .try_send(SupervisorStreamEvent::Finished {
                reason: LlmFinishReason::Stop,
                usage: None,
            })
            .expect("send finished");
        shell_state.tick_supervisor_stream();

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Token usage: input=120 output=45 total=165"));
    }

    #[test]
    fn global_supervisor_chat_toggle_opens_from_normal_navigation_mode() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InboxView)
        ));
        assert_eq!(shell_state.mode, UiMode::Normal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::SupervisorChatView)
        ));
        assert_eq!(shell_state.mode, UiMode::Insert);
    }

    #[tokio::test]
    async fn global_supervisor_chat_accepts_freeform_query_and_streams_response() {
        let provider = Arc::new(TestLlmProvider::new(vec![
            Ok(LlmStreamChunk {
                delta: "System summary:\n".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "No blockers right now.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: Some(LlmTokenUsage {
                    input_tokens: 21,
                    output_tokens: 8,
                    total_tokens: 29,
                }),
                rate_limit: None,
            }),
        ]));
        let provider_dyn: Arc<dyn LlmProvider> = provider;
        let mut shell_state = UiShellState::new_with_supervisor(
            "ready".to_owned(),
            triage_projection(),
            Some(provider_dyn),
        );

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        for ch in "what needs me next?".chars() {
            handle_key_press(&mut shell_state, key(KeyCode::Char(ch)));
        }
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                shell_state.tick_supervisor_stream();
                let rendered = shell_state.ui_state().center_pane.lines.join("\n");
                if rendered.contains("No blockers right now.") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("global chat stream should render response");

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Last query: what needs me next?"));
        assert!(rendered.contains("Live supervisor stream:"));
        assert!(rendered.contains("Token usage: input=21 output=8 total=29"));
    }

    #[tokio::test]
    async fn selected_chat_entry_uses_dispatcher_freeform_invocation_and_renders_stream() {
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(vec![
            Ok(LlmStreamChunk {
                delta: "Current activity: running focused tests.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "No blockers detected.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let dispatcher_dyn: Arc<dyn SupervisorCommandDispatcher> = dispatcher.clone();
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            inspector_projection(),
            None,
            Some(dispatcher_dyn),
            None,
            None,
        );

        shell_state.open_chat_inspector_for_selected();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                shell_state.tick_supervisor_stream();
                let rendered = shell_state.ui_state().center_pane.lines.join("\n");
                if rendered.contains("No blockers detected.") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("selected chat stream should render dispatcher response");

        let requests = dispatcher.requests();
        assert_eq!(requests.len(), 1);
        let (invocation, context) = &requests[0];
        let command = CommandRegistry::default()
            .parse_invocation(invocation)
            .expect("dispatcher invocation should parse");
        assert!(matches!(
            command,
            Command::SupervisorQuery(SupervisorQueryArgs::Freeform { query, .. })
                if query == "What is the current status of this ticket?"
        ));
        assert_eq!(
            context.selected_work_item_id.as_deref(),
            Some("wi-inspector")
        );
        assert_eq!(
            context.selected_session_id.as_deref(),
            Some("sess-inspector")
        );
        assert_eq!(context.scope.as_deref(), Some("session:sess-inspector"));

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Live supervisor stream:"));
        assert!(rendered.contains("Current activity: running focused tests."));
        assert!(rendered.contains("No blockers detected."));
    }

    #[tokio::test]
    async fn global_chat_panel_dispatcher_open_close_and_message_rendering() {
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(vec![
            Ok(LlmStreamChunk {
                delta: "Global status: ".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "two approvals need review.".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let dispatcher_dyn: Arc<dyn SupervisorCommandDispatcher> = dispatcher.clone();
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            triage_projection(),
            None,
            Some(dispatcher_dyn),
            None,
            None,
        );

        assert!(!shell_state.is_global_supervisor_chat_active());
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(shell_state.is_global_supervisor_chat_active());
        assert_eq!(shell_state.mode, UiMode::Insert);

        for ch in "what needs me next globally?".chars() {
            handle_key_press(&mut shell_state, key(KeyCode::Char(ch)));
        }
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                shell_state.tick_supervisor_stream();
                let rendered = shell_state.ui_state().center_pane.lines.join("\n");
                if rendered.contains("two approvals need review.") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("global chat stream should render dispatcher response");

        let requests = dispatcher.requests();
        assert_eq!(requests.len(), 1);
        let (invocation, context) = &requests[0];
        let command = CommandRegistry::default()
            .parse_invocation(invocation)
            .expect("dispatcher invocation should parse");
        assert!(matches!(
            command,
            Command::SupervisorQuery(SupervisorQueryArgs::Freeform { query, .. })
                if query == "what needs me next globally?"
        ));
        assert_eq!(context.scope.as_deref(), Some("global"));
        assert!(context.selected_work_item_id.is_none());
        assert!(context.selected_session_id.is_none());

        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Last query: what needs me next globally?"));
        assert!(rendered.contains("Global status: two approvals need review."));

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        assert_eq!(shell_state.mode, UiMode::Normal);
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(!shell_state.is_global_supervisor_chat_active());
    }

    #[test]
    fn closing_global_supervisor_chat_restores_prior_context_state() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        shell_state.move_selection(2);
        shell_state.open_focus_card_for_selected();
        let before_selection = shell_state.selected_inbox_item_id.clone();
        let before_stack = shell_state.view_stack.center_views().to_vec();

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::SupervisorChatView)
        ));

        handle_key_press(&mut shell_state, key(KeyCode::Esc));
        handle_key_press(&mut shell_state, key(KeyCode::Char('j')));
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));

        assert_eq!(shell_state.selected_inbox_item_id, before_selection);
        assert_eq!(
            shell_state.view_stack.center_views(),
            before_stack.as_slice()
        );
        assert_eq!(shell_state.mode, UiMode::Normal);
    }

    #[test]
    fn failed_global_supervisor_query_preserves_draft_for_retry() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());

        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        for ch in "what changed?".chars() {
            handle_key_press(&mut shell_state, key(KeyCode::Char(ch)));
        }
        handle_key_press(&mut shell_state, key(KeyCode::Enter));

        assert_eq!(shell_state.global_supervisor_chat_draft, "what changed?");
        assert_eq!(shell_state.global_supervisor_chat_last_query, None);
        let stream = shell_state
            .supervisor_chat_stream
            .as_ref()
            .expect("failed query should surface terminal state");
        assert_eq!(
            stream.response_state,
            SupervisorResponseState::BackendUnavailable
        );
        let rendered = shell_state.ui_state().center_pane.lines.join("\n");
        assert!(rendered.contains("Supervisor state: backend-unavailable"));
        assert!(rendered.contains("Retry guidance:"));
        assert!(shell_state
            .status_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("supervisor stream unavailable")));
    }

    #[tokio::test]
    async fn opening_new_chat_stream_cancels_active_stream_with_known_id() {
        let provider = Arc::new(TestLlmProvider::new(Vec::new()));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let mut shell_state = UiShellState::new_with_supervisor(
            "ready".to_owned(),
            inspector_projection(),
            Some(provider_dyn),
        );
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
            stream.stream_id = Some("stream-old".to_owned());
        }

        shell_state.start_supervisor_stream_for_selected();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if provider
                    .cancelled_streams()
                    .iter()
                    .any(|id| id == "stream-old")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stream cancellation should be forwarded");
    }

    #[tokio::test]
    async fn opening_new_dispatcher_chat_stream_cancels_active_stream_with_known_id() {
        let dispatcher = Arc::new(TestSupervisorDispatcher::new(vec![Ok(LlmStreamChunk {
            delta: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        })]));
        let dispatcher_dyn: Arc<dyn SupervisorCommandDispatcher> = dispatcher.clone();
        let mut shell_state = UiShellState::new_with_integrations(
            "ready".to_owned(),
            inspector_projection(),
            None,
            Some(dispatcher_dyn),
            None,
            None,
        );
        shell_state.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        let _sender = attach_supervisor_stream(&mut shell_state, "wi-inspector");
        if let Some(stream) = shell_state.supervisor_chat_stream.as_mut() {
            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
            stream.stream_id = Some("dispatcher-stream-old".to_owned());
        }

        shell_state.start_supervisor_stream_for_selected();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if dispatcher
                    .cancelled_streams()
                    .iter()
                    .any(|id| id == "dispatcher-stream-old")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatcher stream cancellation should be forwarded");
    }

    #[tokio::test]
    async fn run_supervisor_stream_task_emits_usage_events_for_usage_only_chunks() {
        let provider = Arc::new(TestLlmProvider::new(vec![
            Ok(LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: Some(LlmTokenUsage {
                    input_tokens: 20,
                    output_tokens: 5,
                    total_tokens: 25,
                }),
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "done".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let provider_dyn: Arc<dyn LlmProvider> = provider;
        let request = LlmChatRequest {
            model: "test-model".to_owned(),
            tools: Vec::new(),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "status".to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
            temperature: None,
            tool_choice: None,
            max_output_tokens: None,
        };

        let (sender, mut receiver) = mpsc::channel(8);
        run_supervisor_stream_task(provider_dyn, request, sender).await;

        let mut saw_usage = false;
        let mut saw_finished = false;
        while let Some(event) = receiver.recv().await {
            match event {
                SupervisorStreamEvent::Usage { usage } => {
                    saw_usage |= usage.total_tokens == 25;
                }
                SupervisorStreamEvent::Finished { reason, usage } => {
                    saw_finished |= reason == LlmFinishReason::Stop && usage.is_none();
                }
                _ => {}
            }
        }

        assert!(saw_usage);
        assert!(saw_finished);
    }

    #[test]
    fn inspector_ignores_mismatched_artifact_work_item_links() {
        let mut projection = inspector_projection();
        let selected_work_item = WorkItemId::new("wi-inspector");
        let foreign_work_item = WorkItemId::new("wi-foreign");
        let foreign_artifact_id = ArtifactId::new("artifact-foreign-diff");

        projection
            .work_items
            .get_mut(&selected_work_item)
            .expect("selected work item")
            .artifacts
            .push(foreign_artifact_id.clone());
        projection.artifacts.insert(
            foreign_artifact_id.clone(),
            ArtifactProjection {
                id: foreign_artifact_id,
                work_item_id: foreign_work_item,
                kind: ArtifactKind::Diff,
                label: "Foreign diff artifact".to_owned(),
                uri: "artifact://diff/wi-foreign?files=99&insertions=1&deletions=1".to_owned(),
            },
        );

        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::InspectorView {
            work_item_id: selected_work_item,
            inspector: ArtifactInspectorKind::Diff,
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None, None);
        let rendered = ui_state.center_pane.lines.join("\n");
        assert!(!rendered.contains("Foreign diff artifact"));
        assert!(!rendered.contains("artifact://diff/wi-foreign"));
    }

    #[test]
    fn keymap_prefix_binding_opens_artifact_inspectors() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('v')));
        let overlay = shell_state
            .which_key_overlay
            .as_ref()
            .expect("overlay is shown for inspector prefix");
        let rendered = render_which_key_overlay_text(overlay);
        assert!(rendered.contains("v  (Artifact inspectors)"));
        assert!(rendered.contains("d  Open diff inspector for selected item"));
        assert!(rendered.contains("t  Open test inspector for selected item"));
        assert!(rendered.contains("p  Open PR inspector for selected item"));
        assert!(rendered.contains("c  Open chat inspector for selected item"));

        handle_key_press(&mut shell_state, key(KeyCode::Char('d')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InspectorView {
                inspector: ArtifactInspectorKind::Diff,
                ..
            })
        ));

        shell_state.minimize_center_view();
        handle_key_press(&mut shell_state, key(KeyCode::Char('v')));
        handle_key_press(&mut shell_state, key(KeyCode::Char('c')));
        assert!(matches!(
            shell_state.view_stack.active_center(),
            Some(CenterView::InspectorView {
                inspector: ArtifactInspectorKind::Chat,
                ..
            })
        ));
    }

    #[test]
    fn unsupported_keymap_command_id_surfaces_warning_instead_of_no_op() {
        let mut shell_state = UiShellState::new("ready".to_owned(), inspector_projection());
        let custom_keymap = KeymapTrie::compile(
            &KeymapConfig {
                modes: vec![ModeKeymapConfig {
                    mode: UiMode::Normal,
                    bindings: vec![KeyBindingConfig {
                        keys: vec!["x".to_owned()],
                        command_id: command_ids::SUPERVISOR_QUERY.to_owned(),
                    }],
                    prefixes: Vec::new(),
                }],
            },
            |_| true,
        )
        .expect("custom keymap should compile");
        shell_state.keymap = Box::leak(Box::new(custom_keymap));

        let should_quit = handle_key_press(&mut shell_state, key(KeyCode::Char('x')));
        assert!(!should_quit);
        let status = shell_state.ui_state().status;
        assert!(status.contains("unsupported command mapping"));
        assert!(status.contains(command_ids::SUPERVISOR_QUERY));
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
            None,
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
            command_id(UiCommand::ToggleGlobalSupervisorChat),
            "ui.supervisor_chat.toggle"
        );
        assert_eq!(
            command_id(UiCommand::OpenTerminalForSelected),
            command_ids::UI_OPEN_TERMINAL_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenDiffInspectorForSelected),
            command_ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenTestInspectorForSelected),
            command_ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenPrInspectorForSelected),
            command_ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::OpenChatInspectorForSelected),
            command_ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED
        );
        assert_eq!(
            command_id(UiCommand::FocusNextInbox),
            command_ids::UI_FOCUS_NEXT_INBOX
        );
        assert_eq!(
            command_id(UiCommand::AdvanceTerminalWorkflowStage),
            "ui.terminal.workflow.advance"
        );
        assert_eq!(
            command_id(UiCommand::KillSelectedSession),
            "ui.terminal.kill_selected_session"
        );
    }

    #[test]
    fn command_registry_round_trips_ids() {
        let all_commands = [
            UiCommand::EnterNormalMode,
            UiCommand::EnterInsertMode,
            UiCommand::ToggleGlobalSupervisorChat,
            UiCommand::OpenTerminalForSelected,
            UiCommand::OpenDiffInspectorForSelected,
            UiCommand::OpenTestInspectorForSelected,
            UiCommand::OpenPrInspectorForSelected,
            UiCommand::OpenChatInspectorForSelected,
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
            UiCommand::AdvanceTerminalWorkflowStage,
            UiCommand::KillSelectedSession,
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
    fn terminal_mode_supports_escape_chord_with_compose_buffer() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('t')));
        assert_eq!(shell_state.mode, UiMode::Terminal);
        assert!(shell_state.is_terminal_view_active());

        let routed = route_key_press(&mut shell_state, key(KeyCode::Char('j')));
        assert!(matches!(routed, RoutedInput::Ignore));
        assert_eq!(shell_state.terminal_compose_draft, "j");

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
        assert!(matches!(routed, RoutedInput::Ignore));
        assert!(!shell_state.terminal_escape_pending);
        assert!(shell_state.terminal_compose_draft.is_empty());
    }

    #[test]
    fn terminal_compose_supports_multiline_input() {
        let mut shell_state = UiShellState::new("ready".to_owned(), sample_projection(true));
        handle_key_press(&mut shell_state, key(KeyCode::Char('t')));
        assert_eq!(shell_state.mode, UiMode::Terminal);

        handle_key_press(&mut shell_state, key(KeyCode::Char('h')));
        handle_key_press(&mut shell_state, key(KeyCode::Char('i')));
        handle_key_press(&mut shell_state, shift_key(KeyCode::Enter));
        handle_key_press(&mut shell_state, key(KeyCode::Char('!')));

        assert_eq!(shell_state.terminal_compose_draft, "hi\n!");
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
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('c')))),
            Some(UiCommand::ToggleGlobalSupervisorChat)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('v')))),
            None
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Char('d')))),
            Some(UiCommand::OpenDiffInspectorForSelected)
        );
        assert_eq!(
            routed_command(route_key_press(&mut shell_state, key(KeyCode::Esc))),
            Some(UiCommand::EnterNormalMode)
        );
    }
}
