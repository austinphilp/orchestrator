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
use edtui::{EditorEventHandler, EditorMode, EditorState, EditorTheme, EditorView, Lines};
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
use ratatui_interact::components::{Input, InputState, SelectState};
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
const TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY: usize = 128;
const TICKET_PICKER_PRIORITY_STATES_DEFAULT: &[&str] =
    &["In Progress", "Final Approval", "Todo", "Backlog"];
const DEFAULT_UI_THEME: &str = "nord";
const DEFAULT_SUPERVISOR_MODEL: &str = "openai/gpt-4o-mini";
const DEFAULT_BACKGROUND_SESSION_REFRESH_SECS: u64 = 15;
const MIN_BACKGROUND_SESSION_REFRESH_SECS: u64 = 2;
const MAX_BACKGROUND_SESSION_REFRESH_SECS: u64 = 15;
const MERGE_POLL_INTERVAL: Duration = Duration::from_secs(60);
const MERGE_REQUEST_RATE_LIMIT: Duration = Duration::from_secs(1);
const TICKET_PICKER_CREATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const BACKGROUND_SESSION_DEFERRED_OUTPUT_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiRuntimeConfig {
    theme: String,
    ticket_picker_priority_states: Vec<String>,
    supervisor_model: String,
    background_session_refresh_secs: u64,
}

impl Default for UiRuntimeConfig {
    fn default() -> Self {
        Self {
            theme: DEFAULT_UI_THEME.to_owned(),
            ticket_picker_priority_states: TICKET_PICKER_PRIORITY_STATES_DEFAULT
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            supervisor_model: DEFAULT_SUPERVISOR_MODEL.to_owned(),
            background_session_refresh_secs: DEFAULT_BACKGROUND_SESSION_REFRESH_SECS,
        }
    }
}

static UI_RUNTIME_CONFIG: OnceLock<std::sync::RwLock<UiRuntimeConfig>> = OnceLock::new();

fn ui_runtime_config_store() -> &'static std::sync::RwLock<UiRuntimeConfig> {
    UI_RUNTIME_CONFIG.get_or_init(|| std::sync::RwLock::new(UiRuntimeConfig::default()))
}

pub fn set_ui_runtime_config(
    theme: String,
    ticket_picker_priority_states: Vec<String>,
    supervisor_model: String,
    background_session_refresh_secs: u64,
) {
    let mut parsed_states = ticket_picker_priority_states
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if parsed_states.is_empty() {
        parsed_states = TICKET_PICKER_PRIORITY_STATES_DEFAULT
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
    }

    let config = UiRuntimeConfig {
        theme: {
            let trimmed = theme.trim();
            if trimmed.is_empty() {
                DEFAULT_UI_THEME.to_owned()
            } else {
                trimmed.to_owned()
            }
        },
        ticket_picker_priority_states: parsed_states,
        supervisor_model: {
            let trimmed = supervisor_model.trim();
            if trimmed.is_empty() {
                DEFAULT_SUPERVISOR_MODEL.to_owned()
            } else {
                trimmed.to_owned()
            }
        },
        background_session_refresh_secs: background_session_refresh_secs
            .clamp(
                MIN_BACKGROUND_SESSION_REFRESH_SECS,
                MAX_BACKGROUND_SESSION_REFRESH_SECS,
            ),
    };

    if let Ok(mut guard) = ui_runtime_config_store().write() {
        *guard = config;
    }
}

fn ui_theme_config_value() -> String {
    ui_runtime_config_store()
        .read()
        .map(|guard| guard.theme.clone())
        .unwrap_or_else(|_| DEFAULT_UI_THEME.to_owned())
}

fn ticket_picker_priority_states_config_value() -> Vec<String> {
    ui_runtime_config_store()
        .read()
        .map(|guard| guard.ticket_picker_priority_states.clone())
        .unwrap_or_else(|_| {
            TICKET_PICKER_PRIORITY_STATES_DEFAULT
                .iter()
                .map(|value| (*value).to_owned())
                .collect()
        })
}

fn supervisor_model_config_value() -> String {
    ui_runtime_config_store()
        .read()
        .map(|guard| guard.supervisor_model.clone())
        .unwrap_or_else(|_| DEFAULT_SUPERVISOR_MODEL.to_owned())
}

fn background_session_refresh_interval_config_value() -> Duration {
    let secs = ui_runtime_config_store()
        .read()
        .map(|guard| guard.background_session_refresh_secs)
        .unwrap_or(DEFAULT_BACKGROUND_SESSION_REFRESH_SECS)
        .clamp(
            MIN_BACKGROUND_SESSION_REFRESH_SECS,
            MAX_BACKGROUND_SESSION_REFRESH_SECS,
        );
    Duration::from_secs(secs)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketCreateSubmitMode {
    CreateOnly,
    CreateAndStart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTicketFromPickerRequest {
    pub brief: String,
    pub selected_project: Option<String>,
    pub submit_mode: TicketCreateSubmitMode,
}

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
    async fn create_ticket_from_brief(
        &self,
        request: CreateTicketFromPickerRequest,
    ) -> Result<TicketSummary, CoreError>;
    async fn archive_ticket(&self, _ticket: TicketSummary) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "ticket archiving is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn archive_session(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<Option<String>, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "session archiving is not supported by this ticket provider".to_owned(),
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
    async fn set_session_working_state(
        &self,
        _session_id: WorkerSessionId,
        _is_working: bool,
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
    async fn advance_session_workflow(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<SessionWorkflowAdvanceOutcome, CoreError> {
        Err(CoreError::Configuration(
            "session workflow advance is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn complete_session_after_merge(
        &self,
        _session_id: WorkerSessionId,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn publish_inbox_item(
        &self,
        _request: InboxPublishRequest,
    ) -> Result<ProjectionState, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "inbox publishing is not supported by this ticket provider".to_owned(),
        ))
    }
    async fn resolve_inbox_item(
        &self,
        _request: InboxResolveRequest,
    ) -> Result<ProjectionState, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "inbox resolution is not supported by this ticket provider".to_owned(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxPublishRequest {
    pub work_item_id: WorkItemId,
    pub session_id: Option<WorkerSessionId>,
    pub kind: InboxItemKind,
    pub title: String,
    pub coalesce_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxResolveRequest {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: WorkItemId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorktreeDiff {
    pub session_id: WorkerSessionId,
    pub base_branch: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorkflowAdvanceOutcome {
    pub session_id: WorkerSessionId,
    pub work_item_id: WorkItemId,
    pub from: WorkflowState,
    pub to: WorkflowState,
    pub instruction: Option<String>,
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
            Self::TerminalView { .. } => "Terminal".to_owned(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionDisplayLabels {
    full_label: String,
    compact_label: String,
}

fn session_display_labels(domain: &ProjectionState, session_id: &WorkerSessionId) -> SessionDisplayLabels {
    let fallback = format!("session {}", session_id.as_str());
    let Some(work_item_id) = domain
        .sessions
        .get(session_id)
        .and_then(|session| session.work_item_id.as_ref())
    else {
        return SessionDisplayLabels {
            full_label: fallback.clone(),
            compact_label: fallback,
        };
    };
    let Some(work_item) = domain.work_items.get(work_item_id) else {
        return SessionDisplayLabels {
            full_label: fallback.clone(),
            compact_label: fallback,
        };
    };
    let Some(ticket_id) = work_item.ticket_id.as_ref() else {
        return SessionDisplayLabels {
            full_label: fallback.clone(),
            compact_label: fallback,
        };
    };
    let Some(metadata) = latest_ticket_metadata(domain, ticket_id) else {
        return SessionDisplayLabels {
            full_label: fallback.clone(),
            compact_label: fallback,
        };
    };

    let title = metadata.title;
    let identifier = metadata.identifier;
    let full_label = if title.trim().is_empty() {
        if identifier.trim().is_empty() {
            fallback.clone()
        } else {
            identifier.clone()
        }
    } else {
        title
    };
    let compact_label = if identifier.trim().is_empty() {
        fallback
    } else {
        identifier
    };

    SessionDisplayLabels {
        full_label,
        compact_label,
    }
}

fn session_display_with_status(
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    status: Option<&WorkerSessionStatus>,
) -> String {
    let labels = session_display_labels(domain, session_id);
    let status_text = status
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| "Unknown".to_owned());
    format!("{} ({status_text})", labels.full_label)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TicketDisplayMetadata {
    identifier: String,
    title: String,
}

fn latest_ticket_metadata(domain: &ProjectionState, ticket_id: &TicketId) -> Option<TicketDisplayMetadata> {
    for event in domain.events.iter().rev() {
        let OrchestrationEventPayload::TicketSynced(payload) = &event.payload else {
            continue;
        };
        if payload.ticket_id != *ticket_id {
            continue;
        }
        return Some(TicketDisplayMetadata {
            identifier: payload.identifier.trim().to_owned(),
            title: payload.title.trim().to_owned(),
        });
    }
    None
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
            lines.push("Tab: toggle left/right pane focus".to_owned());
            lines.push("Shift+Tab: cycle inbox/sessions focus in left pane".to_owned());
            lines.push("[ / ]: cycle batch lanes".to_owned());
            lines.push("g/G: jump first/last item".to_owned());
            lines.push("Enter: open focus card".to_owned());
            lines.push("c: toggle global supervisor chat".to_owned());
            lines.push("i: enter insert mode (or notes/compose on right pane)".to_owned());
            lines.push("I: open terminal for selected item".to_owned());
            lines.push("o: open session output and acknowledge selected inbox item".to_owned());
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
                title: format!(
                    "Terminal {}",
                    session_display_labels(domain, session_id).compact_label
                ),
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

#[derive(Debug, Clone)]
struct TerminalViewState {
    entries: Vec<TerminalTranscriptEntry>,
    error: Option<String>,
    output_fragment: String,
    deferred_output: Vec<u8>,
    last_background_flush_at: Option<Instant>,
    output_rendered_line_count: usize,
    output_scroll_line: usize,
    output_viewport_rows: usize,
    output_follow_tail: bool,
    turn_active: bool,
    pending_needs_input_prompts: VecDeque<NeedsInputPromptState>,
    active_needs_input: Option<NeedsInputComposerState>,
    last_merge_conflict_signature: Option<String>,
}

impl Default for TerminalViewState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            error: None,
            output_fragment: String::new(),
            deferred_output: Vec::new(),
            last_background_flush_at: None,
            output_rendered_line_count: 0,
            output_scroll_line: 0,
            output_viewport_rows: 1,
            output_follow_tail: true,
            turn_active: false,
            pending_needs_input_prompts: VecDeque::new(),
            active_needs_input: None,
            last_merge_conflict_signature: None,
        }
    }
}

impl TerminalViewState {
    fn enqueue_needs_input_prompt(&mut self, prompt: NeedsInputPromptState) {
        let duplicate_active = self
            .active_needs_input
            .as_ref()
            .map(|active| active.prompt_id == prompt.prompt_id)
            .unwrap_or(false);
        if duplicate_active
            || self
                .pending_needs_input_prompts
                .iter()
                .any(|entry| entry.prompt_id == prompt.prompt_id)
        {
            return;
        }
        self.pending_needs_input_prompts.push_back(prompt);
        self.activate_next_needs_input_prompt_if_idle();
    }

    fn activate_next_needs_input_prompt_if_idle(&mut self) {
        if self.active_needs_input.is_some() {
            return;
        }
        while let Some(prompt) = self.pending_needs_input_prompts.pop_front() {
            if prompt.questions.is_empty() {
                continue;
            }
            self.active_needs_input = Some(NeedsInputComposerState::new(
                prompt.prompt_id,
                prompt.questions,
                !prompt.requires_manual_activation,
                prompt.is_structured_plan_request,
            ));
            return;
        }
    }

    fn complete_active_needs_input_prompt(&mut self) {
        self.active_needs_input = None;
        self.activate_next_needs_input_prompt_if_idle();
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NeedsInputAnswerDraft {
    selected_option_index: Option<usize>,
    note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NeedsInputPromptState {
    prompt_id: String,
    questions: Vec<BackendNeedsInputQuestion>,
    requires_manual_activation: bool,
    is_structured_plan_request: bool,
}

#[derive(Clone)]
struct NeedsInputComposerState {
    prompt_id: String,
    questions: Vec<BackendNeedsInputQuestion>,
    answer_drafts: Vec<NeedsInputAnswerDraft>,
    current_question_index: usize,
    select_state: SelectState,
    note_editor_state: EditorState,
    interaction_active: bool,
    note_insert_mode: bool,
    error: Option<String>,
    is_structured_plan_request: bool,
}

impl std::fmt::Debug for NeedsInputComposerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeedsInputComposerState")
            .field("prompt_id", &self.prompt_id)
            .field("questions", &self.questions)
            .field("answer_drafts", &self.answer_drafts)
            .field("current_question_index", &self.current_question_index)
            .field("select_state", &self.select_state)
            .field("note_editor_mode", &self.note_editor_state.mode)
            .field("note_editor_text", &editor_state_text(&self.note_editor_state))
            .field("interaction_active", &self.interaction_active)
            .field("note_insert_mode", &self.note_insert_mode)
            .field("error", &self.error)
            .field("is_structured_plan_request", &self.is_structured_plan_request)
            .finish()
    }
}

impl NeedsInputComposerState {
    fn new(
        prompt_id: String,
        questions: Vec<BackendNeedsInputQuestion>,
        interaction_active: bool,
        is_structured_plan_request: bool,
    ) -> Self {
        let answer_drafts = vec![NeedsInputAnswerDraft::default(); questions.len()];
        let mut state = Self {
            prompt_id,
            questions,
            answer_drafts,
            current_question_index: 0,
            select_state: SelectState::new(0),
            note_editor_state: insert_mode_editor_state(),
            interaction_active,
            note_insert_mode: false,
            error: None,
            is_structured_plan_request,
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
        draft.note = editor_state_text(&self.note_editor_state);
    }

    fn refresh_controls_from_current_question(&mut self) {
        let Some(question) = self.current_question() else {
            self.select_state = SelectState::new(0);
            clear_editor_state(&mut self.note_editor_state);
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
        self.select_state.focused = self.interaction_active && options_len > 0;
        if let Some(index) = draft.selected_option_index.filter(|index| *index < options_len) {
            self.select_state.selected_index = Some(index);
            self.select_state.highlighted_index = index;
        }
        set_editor_state_text(&mut self.note_editor_state, draft.note.as_str());
        self.note_editor_state.mode =
            if self.interaction_active && self.note_insert_mode {
                EditorMode::Insert
            } else {
                EditorMode::Normal
            };
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

    fn set_interaction_active(&mut self, active: bool) {
        self.interaction_active = active;
        if !active {
            self.note_insert_mode = false;
        }
        self.refresh_controls_from_current_question();
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

    fn flush_pending_delta(&mut self) -> bool {
        if self.pending_delta.is_empty() {
            self.last_flush_coalesced_chunks = 0;
            return false;
        }

        self.last_flush_coalesced_chunks = self.pending_chunk_count.max(1);
        self.transcript.push_str(&self.pending_delta);
        self.pending_delta.clear();
        self.pending_chunk_count = 0;
        trim_to_trailing_chars(&mut self.transcript, SUPERVISOR_STREAM_MAX_TRANSCRIPT_CHARS);
        true
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
