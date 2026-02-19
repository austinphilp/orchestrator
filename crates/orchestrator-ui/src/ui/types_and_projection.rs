use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use orchestrator_core::{
    attention_inbox_snapshot, command_ids, ArtifactKind, ArtifactProjection, AttentionBatchKind,
    AttentionEngineConfig, AttentionPriorityBand, Command, CommandRegistry, CoreError, InboxItemId,
    InboxItemKind, LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider, LlmRateLimitState,
    LlmResponseStream, LlmRole, LlmTokenUsage, OrchestrationEventPayload, ProjectId,
    ProjectionState, SelectedTicketFlowResult, SessionProjection, SupervisorQueryArgs,
    SupervisorQueryContextArgs, TicketId, TicketSummary, UntypedCommandInvocation, WorkItemId,
    WorkerSessionId, WorkerSessionStatus, WorkflowState,
};
use orchestrator_runtime::{
    BackendEvent, BackendKind, BackendNeedsInputAnswer, BackendNeedsInputEvent,
    BackendNeedsInputQuestion, BackendOutputEvent, BackendTurnStateEvent, RuntimeError,
    RuntimeResult, RuntimeSessionId, SessionHandle, SpawnSpec, WorkerBackend,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Terminal;
use ratatui_interact::components::{
    Input, InputState, Select, SelectState, SelectStyle, TabConfig, TextArea, TextAreaState,
    WrapMode,
};
use ratskin::RatSkin;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

#[path = "../keymap.rs"]
mod keymap;

pub use keymap::{
    key_stroke_from_event, KeyBindingConfig, KeyPrefixConfig, KeyStroke, KeymapCompileError,
    KeymapConfig, KeymapLookupResult, KeymapTrie, ModeKeymapConfig,
};

const TICKET_PICKER_EVENT_CHANNEL_CAPACITY: usize = 32;
const TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY: usize = 32;
const TICKET_PICKER_PRIORITY_STATES_ENV: &str = "ORCHESTRATOR_TICKET_PICKER_PRIORITY_STATES";
const UI_THEME_ENV: &str = "ORCHESTRATOR_UI_THEME";
const TICKET_PICKER_PRIORITY_STATES_DEFAULT: &[&str] =
    &["In Progress", "Final Approval", "Todo", "Backlog"];
const MERGE_POLL_INTERVAL: Duration = Duration::from_secs(60);
const MERGE_REQUEST_RATE_LIMIT: Duration = Duration::from_secs(1);

#[async_trait]
pub trait TicketPickerProvider: Send + Sync {
    async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError>;
    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
    }
    async fn start_or_resume_ticket(
        &self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) -> Result<SelectedTicketFlowResult, CoreError>;
    async fn create_and_start_ticket_from_brief(
        &self,
        brief: String,
    ) -> Result<TicketSummary, CoreError>;
    async fn archive_ticket(&self, _ticket: TicketSummary) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "ticket archiving is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn reload_projection(&self) -> Result<ProjectionState, CoreError>;
    async fn mark_session_crashed(
        &self,
        _session_id: WorkerSessionId,
        _reason: String,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn session_worktree_diff(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<SessionWorktreeDiff, CoreError> {
        Err(CoreError::Configuration(
            "session worktree diff is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn set_session_workflow_stage(
        &self,
        _session_id: WorkerSessionId,
        _workflow_stage: String,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_session_workflow_stages(
        &self,
    ) -> Result<Vec<(WorkerSessionId, String)>, CoreError> {
        Ok(Vec::new())
    }
    async fn complete_session_after_merge(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<(), CoreError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorktreeDiff {
    pub session_id: WorkerSessionId,
    pub base_branch: String,
    pub diff: String,
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
    let center_pane = project_center_pane(
        &active_center,
        &inbox_rows,
        &inbox_batch_surfaces,
        domain,
        terminal_view_state,
    );

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
            lines.push("i: open terminal for selected item".to_owned());
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
                        lines.push(format!("[stream error] {}", compact_focus_card_text(error)));
                        lines.push(String::new());
                    }
                    if terminal_state.entries.is_empty()
                        && terminal_state.output_fragment.is_empty()
                    {
                        lines.push("No terminal output available yet.".to_owned());
                    } else {
                        lines.extend(render_terminal_transcript_lines(terminal_state));
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalViewState {
    entries: Vec<TerminalTranscriptEntry>,
    error: Option<String>,
    output_fragment: String,
    workflow_stage: TerminalWorkflowStage,
    output_rendered_line_count: usize,
    output_scroll_line: usize,
    output_viewport_rows: usize,
    output_follow_tail: bool,
    turn_active: bool,
    last_merge_conflict_signature: Option<String>,
}

impl Default for TerminalViewState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            error: None,
            output_fragment: String::new(),
            workflow_stage: TerminalWorkflowStage::Planning,
            output_rendered_line_count: 0,
            output_scroll_line: 0,
            output_viewport_rows: 1,
            output_follow_tail: true,
            turn_active: false,
            last_merge_conflict_signature: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalTranscriptEntry {
    Message(String),
    Foldable(TerminalFoldSection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalFoldSection {
    kind: TerminalFoldKind,
    content: String,
    folded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedTerminalLine {
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalFoldKind {
    Reasoning,
    FileChange,
    ToolCall,
    CommandExecution,
    Other,
}

impl TerminalFoldKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::FileChange => "file-change",
            Self::ToolCall => "tool-call",
            Self::CommandExecution => "command",
            Self::Other => "meta",
        }
    }
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
    const fn label(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Implementation => "implementation",
            Self::Review => "review",
            Self::Complete => "complete",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "planning" => Some(Self::Planning),
            "implementation" => Some(Self::Implementation),
            "review" => Some(Self::Review),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }

    fn advance_instruction(self) -> Option<(Self, &'static str)> {
        match self {
            Self::Planning => Some((
                Self::Implementation,
                "Workflow transition approved: Planning -> Implementation. End planning mode and begin implementation in this worktree now. Before moving out of implementation, run the full test suite for this repository and verify it passes.",
            )),
            Self::Implementation => Some((
                Self::Review,
                "Workflow transition approved: Implementation -> Review. Pause implementation, run the build and fix all errors and warnings, run the full test suite and verify it passes, then open a GitHub PR using the gh CLI and provide a review-ready summary with PR link, evidence, tests, and open risks. While in review, keep the worktree synced with remote/base as merge-status polling runs.",
            )),
            Self::Review => Some((
                Self::Complete,
                "Workflow transition approved: Review -> Complete. Finalize the session with a completion summary, verification status, and remaining follow-ups.",
            )),
            Self::Complete => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NeedsInputAnswerDraft {
    selected_option_index: Option<usize>,
    note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NeedsInputPromptState {
    prompt_id: String,
    questions: Vec<BackendNeedsInputQuestion>,
}

#[derive(Debug, Clone)]
struct NeedsInputModalState {
    session_id: WorkerSessionId,
    prompt_id: String,
    questions: Vec<BackendNeedsInputQuestion>,
    answer_drafts: Vec<NeedsInputAnswerDraft>,
    current_question_index: usize,
    select_state: SelectState,
    note_input_state: InputState,
    note_insert_mode: bool,
    error: Option<String>,
}

impl NeedsInputModalState {
    fn new(
        session_id: WorkerSessionId,
        prompt_id: String,
        questions: Vec<BackendNeedsInputQuestion>,
    ) -> Self {
        let answer_drafts = vec![NeedsInputAnswerDraft::default(); questions.len()];
        let mut state = Self {
            session_id,
            prompt_id,
            questions,
            answer_drafts,
            current_question_index: 0,
            select_state: SelectState::new(0),
            note_input_state: InputState::empty(),
            note_insert_mode: false,
            error: None,
        };
        state.refresh_controls_from_current_question();
        state
    }

    fn current_question(&self) -> Option<&BackendNeedsInputQuestion> {
        self.questions.get(self.current_question_index)
    }

    fn persist_current_draft(&mut self) {
        let Some(draft) = self.answer_drafts.get_mut(self.current_question_index) else {
            return;
        };
        draft.selected_option_index = self.select_state.selected_index;
        draft.note = self.note_input_state.text.clone();
    }

    fn refresh_controls_from_current_question(&mut self) {
        let Some(question) = self.current_question() else {
            self.select_state = SelectState::new(0);
            self.note_input_state.clear();
            self.note_insert_mode = false;
            return;
        };

        let options_len = question.options.as_ref().map(Vec::len).unwrap_or(0);
        let draft = self
            .answer_drafts
            .get(self.current_question_index)
            .cloned()
            .unwrap_or_default();
        self.select_state = SelectState::new(options_len);
        self.select_state.focused = options_len > 0;
        if let Some(index) = draft.selected_option_index.filter(|index| *index < options_len) {
            self.select_state.selected_index = Some(index);
            self.select_state.highlighted_index = index;
        }
        self.note_input_state.set_text(draft.note);
        self.note_input_state.focused = self.note_insert_mode;
    }

    fn move_to_question(&mut self, next_index: usize) {
        if next_index >= self.questions.len() {
            return;
        }
        self.persist_current_draft();
        self.current_question_index = next_index;
        self.error = None;
        self.refresh_controls_from_current_question();
    }

    fn has_next_question(&self) -> bool {
        self.current_question_index + 1 < self.questions.len()
    }

    fn current_question_requires_option_selection(&self) -> bool {
        self.current_question()
            .and_then(|question| question.options.as_ref())
            .map(|options| !options.is_empty())
            .unwrap_or(false)
    }

    fn build_runtime_answers(&mut self) -> RuntimeResult<Vec<BackendNeedsInputAnswer>> {
        self.persist_current_draft();
        let mut answers = Vec::with_capacity(self.questions.len());
        for (index, question) in self.questions.iter().enumerate() {
            let draft = self.answer_drafts.get(index).cloned().unwrap_or_default();
            let mut entries = Vec::new();
            if let Some(options) = question.options.as_ref().filter(|options| !options.is_empty()) {
                let Some(selected_index) = draft.selected_option_index else {
                    return Err(RuntimeError::Protocol(format!(
                        "select an option for '{}'",
                        question.header
                    )));
                };
                let Some(option) = options.get(selected_index) else {
                    return Err(RuntimeError::Protocol(format!(
                        "invalid option selection for '{}'",
                        question.header
                    )));
                };
                entries.push(option.label.clone());
            }

            let note = draft.note.trim();
            if !note.is_empty() {
                entries.push(note.to_owned());
            }
            if entries.is_empty() {
                return Err(RuntimeError::Protocol(format!(
                    "provide a response for '{}'",
                    question.header
                )));
            }

            answers.push(BackendNeedsInputAnswer {
                question_id: question.id.clone(),
                answers: entries,
            });
        }
        Ok(answers)
    }
}

#[derive(Debug)]
enum TerminalSessionEvent {
    Output {
        session_id: WorkerSessionId,
        output: BackendOutputEvent,
    },
    TurnState {
        session_id: WorkerSessionId,
        turn_state: BackendTurnStateEvent,
    },
    NeedsInput {
        session_id: WorkerSessionId,
        needs_input: BackendNeedsInputEvent,
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
