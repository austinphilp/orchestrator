use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use orchestrator_app::frontend::ui_boundary::{
    run_publish_inbox_item_task, run_resolve_inbox_item_task, run_session_merge_finalize_task,
    run_session_workflow_advance_task, run_start_pr_pipeline_polling_task,
    run_stop_pr_pipeline_polling_task, run_supervisor_command_task, run_supervisor_stream_task,
    BackendKind, BackendNeedsInputAnswer, BackendNeedsInputEvent, BackendNeedsInputOption,
    BackendNeedsInputQuestion, BackendOutputEvent, BackendTurnStateEvent, CiCheckStatus, Command,
    CommandRegistry, ControllerInboxPublishRunnerEvent, ControllerInboxResolveRunnerEvent,
    ControllerSupervisorStreamRunnerEvent, ControllerWorkflowAdvanceRunnerEvent, CoreError,
    CreateTicketFromPickerRequest, FrontendApplicationMode, FrontendCommandIntent,
    FrontendController, FrontendEvent, FrontendIntent, FrontendNotification,
    FrontendNotificationLevel, FrontendSnapshot, FrontendTerminalEvent, InboxItemId, InboxItemKind,
    InboxPublishRequest, InboxResolveRequest, LlmChatRequest, LlmFinishReason, LlmMessage,
    LlmProvider, LlmRateLimitState, LlmRole, LlmTokenUsage, MergeQueueCommandKind, MergeQueueEvent,
    OrchestrationEventPayload, ProjectionState, ProjectId, RuntimeError, RuntimeResult,
    SelectedTicketFlowResult, SessionArchiveOutcome, SessionMergeFinalizeOutcome,
    SessionProjection, SessionRuntimeProjection, SessionWorkflowAdvanceOutcome, SessionWorktreeDiff, StoredEventEnvelope,
    SupervisorCommandContext, SupervisorCommandDispatcher, SupervisorQueryArgs,
    SupervisorQueryContextArgs, TicketCreateSubmitMode, TicketId, TicketPickerProvider,
    TicketSummary, TicketSyncedPayload, UntypedCommandInvocation, WorkItemId, WorkerBackend, WorkerSessionId,
    WorkerSessionStatus, WorkflowState, apply_event, attention_inbox_snapshot, command_ids,
    ArtifactKind, ArtifactProjection, AttentionBatchKind, AttentionEngineConfig,
    AttentionInboxSnapshot, AttentionPriorityBand,
};
use orchestrator_config::{
    normalize_supervisor_model, normalize_ui_config, SupervisorConfig, UiConfigToml, UiViewConfig,
};
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use edtui::{EditorEventHandler, EditorMode, EditorState, EditorTheme, EditorView, Lines};
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
#[allow(dead_code)]
const MERGE_REQUEST_RATE_LIMIT: Duration = Duration::from_secs(1);
#[allow(dead_code)]
const TICKET_PICKER_CREATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const BACKGROUND_SESSION_DEFERRED_OUTPUT_MAX_BYTES: usize = 64 * 1024;
const RESOLVED_ANIMATION_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const PLANNING_WORKING_STATE_PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);
const PROJECTION_PERF_LOG_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiRuntimeConfig {
    theme: String,
    ticket_picker_priority_states: Vec<String>,
    supervisor_model: String,
    transcript_line_limit: usize,
    background_session_refresh_secs: u64,
    session_info_background_refresh_secs: u64,
    merge_poll_base_interval_secs: u64,
    merge_poll_max_backoff_secs: u64,
    merge_poll_backoff_multiplier: u64,
}

fn default_ui_runtime_config() -> UiRuntimeConfig {
    let ui = UiConfigToml::default();
    let supervisor = SupervisorConfig::default();

    UiRuntimeConfig {
        theme: ui.theme,
        ticket_picker_priority_states: ui.ticket_picker_priority_states,
        supervisor_model: supervisor.model,
        transcript_line_limit: ui.transcript_line_limit,
        background_session_refresh_secs: ui.background_session_refresh_secs,
        session_info_background_refresh_secs: ui.session_info_background_refresh_secs,
        merge_poll_base_interval_secs: ui.merge_poll_base_interval_secs,
        merge_poll_max_backoff_secs: ui.merge_poll_max_backoff_secs,
        merge_poll_backoff_multiplier: ui.merge_poll_backoff_multiplier,
    }
}

impl Default for UiRuntimeConfig {
    fn default() -> Self {
        default_ui_runtime_config()
    }
}

impl UiRuntimeConfig {
    fn from_inputs(ui_config: UiConfigToml, supervisor_model: String) -> Self {
        let mut normalized_ui = ui_config;
        let mut normalized_supervisor_model = supervisor_model;
        let _ = normalize_ui_config(&mut normalized_ui);
        let _ = normalize_supervisor_model(&mut normalized_supervisor_model);

        Self {
            theme: normalized_ui.theme,
            ticket_picker_priority_states: normalized_ui.ticket_picker_priority_states,
            supervisor_model: normalized_supervisor_model,
            transcript_line_limit: normalized_ui.transcript_line_limit,
            background_session_refresh_secs: normalized_ui.background_session_refresh_secs,
            session_info_background_refresh_secs: normalized_ui
                .session_info_background_refresh_secs,
            merge_poll_base_interval_secs: normalized_ui.merge_poll_base_interval_secs,
            merge_poll_max_backoff_secs: normalized_ui.merge_poll_max_backoff_secs,
            merge_poll_backoff_multiplier: normalized_ui.merge_poll_backoff_multiplier,
        }
    }

    fn from_view_config(ui_config: UiViewConfig, supervisor_model: String) -> Self {
        Self::from_inputs(
            UiConfigToml {
                theme: ui_config.theme,
                ticket_picker_priority_states: ui_config.ticket_picker_priority_states,
                transcript_line_limit: ui_config.transcript_line_limit,
                background_session_refresh_secs: ui_config.background_session_refresh_secs,
                session_info_background_refresh_secs: ui_config
                    .session_info_background_refresh_secs,
                merge_poll_base_interval_secs: ui_config.merge_poll_base_interval_secs,
                merge_poll_max_backoff_secs: ui_config.merge_poll_max_backoff_secs,
                merge_poll_backoff_multiplier: ui_config.merge_poll_backoff_multiplier,
            },
            supervisor_model,
        )
    }

    fn background_session_refresh_interval(&self) -> Duration {
        Duration::from_secs(self.background_session_refresh_secs)
    }

    fn session_info_background_refresh_interval(&self) -> Duration {
        Duration::from_secs(self.session_info_background_refresh_secs)
    }

    fn merge_poll_base_interval(&self) -> Duration {
        Duration::from_secs(self.merge_poll_base_interval_secs)
    }

    fn merge_poll_max_backoff(&self) -> Duration {
        Duration::from_secs(self.merge_poll_max_backoff_secs)
    }

    fn merge_poll_backoff_multiplier(&self) -> u64 {
        self.merge_poll_backoff_multiplier
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CenterView {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ApplicationMode {
    #[default]
    Manual,
    Autopilot,
}

impl ApplicationMode {
    fn label(self) -> &'static str {
        match self {
            Self::Manual => "Manual",
            Self::Autopilot => "Autopilot",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStack {
    center: Vec<CenterView>,
}

impl Default for ViewStack {
    fn default() -> Self {
        Self { center: Vec::new() }
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

    pub fn clear_center(&mut self) {
        self.center.clear();
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
        let label = self
            .center_view_stack
            .iter()
            .map(CenterView::label)
            .collect::<Vec<_>>()
            .join(" > ");
        if label.is_empty() {
            "Terminal".to_owned()
        } else {
            label
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiAttentionProjection {
    inbox_rows: Vec<UiInboxRow>,
    inbox_batch_surfaces: Vec<UiBatchSurface>,
    has_resolved_items: bool,
}

#[derive(Debug, Clone)]
struct AttentionProjectionCache {
    projection: Arc<UiAttentionProjection>,
    refreshed_at: Instant,
    epoch: u64,
}

#[derive(Debug, Clone)]
struct SessionPanelRowsCache {
    rows: Vec<SessionPanelRow>,
    epoch: u64,
}

#[derive(Debug, Clone, Default)]
struct EventDerivedLabelCache {
    processed_event_count: usize,
    work_item_repo: HashMap<WorkItemId, String>,
    ticket_labels: HashMap<String, String>,
}

impl EventDerivedLabelCache {
    fn refresh(&mut self, domain: &ProjectionState) {
        if self.processed_event_count > domain.events.len() {
            self.processed_event_count = 0;
            self.work_item_repo.clear();
            self.ticket_labels.clear();
        }

        for event in domain.events.iter().skip(self.processed_event_count) {
            match &event.payload {
                OrchestrationEventPayload::WorktreeCreated(payload) => {
                    self.work_item_repo.insert(
                        payload.work_item_id.clone(),
                        repository_name_from_path(payload.path.as_str()),
                    );
                }
                OrchestrationEventPayload::TicketSynced(payload) => {
                    self.ticket_labels.insert(
                        payload.ticket_id.as_str().to_owned(),
                        ticket_label_from_synced_event(payload),
                    );
                }
                _ => {}
            }
        }
        self.processed_event_count = domain.events.len();
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

fn session_info_sidebar_title(domain: &ProjectionState, session_id: &WorkerSessionId) -> String {
    let Some(work_item_id) = domain
        .sessions
        .get(session_id)
        .and_then(|session| session.work_item_id.as_ref())
    else {
        return "session info".to_owned();
    };
    let Some(work_item) = domain.work_items.get(work_item_id) else {
        return "session info".to_owned();
    };
    let Some(ticket_id) = work_item.ticket_id.as_ref() else {
        return "session info".to_owned();
    };
    let Some(metadata) = latest_ticket_metadata(domain, ticket_id) else {
        return "session info".to_owned();
    };
    let title = metadata.title.trim();
    if title.is_empty() {
        "session info".to_owned()
    } else {
        compact_focus_card_text(title)
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
    description: Option<String>,
}

fn latest_ticket_metadata(domain: &ProjectionState, ticket_id: &TicketId) -> Option<TicketDisplayMetadata> {
    let mut latest_synced: Option<TicketDisplayMetadata> = None;
    let mut latest_description: Option<Option<String>> = None;
    for event in domain.events.iter().rev() {
        match &event.payload {
            OrchestrationEventPayload::TicketSynced(payload) => {
                if payload.ticket_id != *ticket_id {
                    continue;
                }
                latest_synced = Some(TicketDisplayMetadata {
                    identifier: payload.identifier.trim().to_owned(),
                    title: payload.title.trim().to_owned(),
                    description: None,
                });
                break;
            }
            OrchestrationEventPayload::TicketDetailsSynced(payload) => {
                if payload.ticket_id != *ticket_id {
                    continue;
                }
                if latest_description.is_none() {
                    latest_description = Some(
                        payload
                            .description
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(ToOwned::to_owned),
                    );
                }
            }
            _ => {}
        }
    }
    latest_synced.map(|mut metadata| {
        if let Some(description) = latest_description {
            metadata.description = description;
        }
        metadata
    })
}

fn latest_ticket_description(domain: &ProjectionState, ticket_id: &TicketId) -> Option<String> {
    latest_ticket_metadata(domain, ticket_id).and_then(|metadata| metadata.description)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionInfoDiffCache {
    content: String,
    loading: bool,
    error: Option<String>,
}

impl Default for SessionInfoDiffCache {
    fn default() -> Self {
        Self {
            content: String::new(),
            loading: false,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SessionInfoSummaryCache {
    text: Option<String>,
    loading: bool,
    error: Option<String>,
    context_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SessionCiStatusCache {
    checks: Vec<CiCheckStatus>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
enum SessionInfoSummaryEvent {
    Completed {
        session_id: WorkerSessionId,
        summary: String,
        context_fingerprint: String,
    },
    Failed {
        session_id: WorkerSessionId,
        message: String,
        context_fingerprint: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
struct SessionInfoContext {
    session_id: WorkerSessionId,
    context_fingerprint: String,
    prompt: String,
}

#[allow(dead_code)]
fn clamp_summary_text(input: &str, max_chars: usize) -> String {
    let compact = compact_focus_card_text(input);
    let clamped = compact.chars().take(max_chars).collect::<String>();
    clamped.trim().to_owned()
}

#[allow(dead_code)]
fn summarize_text_output_request(prompt: &str, supervisor_model: &str) -> LlmChatRequest {
    LlmChatRequest {
        model: supervisor_model.to_owned(),
        messages: vec![
            LlmMessage {
                role: LlmRole::System,
                content: "Provide a concise status summary under 200 characters.".to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: prompt.to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
        tools: Vec::new(),
        temperature: Some(0.1),
        tool_choice: None,
        max_output_tokens: Some(120),
    }
}

#[allow(dead_code)]
fn session_info_summary_prompt(
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    diff: Option<&SessionInfoDiffCache>,
) -> Option<SessionInfoContext> {
    let session = domain.sessions.get(session_id)?;
    let work_item_id = session.work_item_id.as_ref()?;
    let work_item = domain.work_items.get(work_item_id)?;
    let ticket_details = work_item
        .ticket_id
        .as_ref()
        .and_then(|ticket_id| latest_ticket_metadata(domain, ticket_id));
    let pr_artifact = collect_work_item_artifacts(work_item_id, domain, 1, is_pr_artifact)
        .into_iter()
        .next();
    let open_inbox_count = work_item
        .inbox_items
        .iter()
        .filter_map(|inbox_item_id| domain.inbox_items.get(inbox_item_id))
        .filter(|item| !item.resolved)
        .count();
    let diffstats = diff
        .map(|entry| parse_diff_file_summaries(entry.content.as_str()))
        .unwrap_or_default();
    let files_changed = diffstats.len();
    let additions = diffstats.iter().map(|file| file.added).sum::<usize>();
    let deletions = diffstats.iter().map(|file| file.removed).sum::<usize>();
    let pr_text = pr_artifact
        .map(|artifact| compact_focus_card_text(artifact.uri.as_str()))
        .unwrap_or_else(|| "none".to_owned());
    let ticket_id = ticket_details
        .as_ref()
        .map(|ticket| ticket.identifier.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let ticket_title = ticket_details
        .as_ref()
        .map(|ticket| ticket.title.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let ticket_description = work_item
        .ticket_id
        .as_ref()
        .and_then(|ticket_id| latest_ticket_description(domain, ticket_id))
        .unwrap_or_default();
    let workflow = work_item
        .workflow_state
        .as_ref()
        .map(|state| format!("{state:?}"))
        .unwrap_or_else(|| "Unknown".to_owned());
    let session_status = session
        .status
        .as_ref()
        .map(|status| format!("{status:?}"))
        .unwrap_or_else(|| "Unknown".to_owned());

    let context_fingerprint = format!(
        "{}|{workflow}|{session_status}|{ticket_id}|{ticket_title}|{ticket_description}|{pr_text}|{files_changed}|{additions}|{deletions}|{open_inbox_count}",
        session_id.as_str()
    );
    let prompt = format!(
        "Session: {}\nWorkflow: {workflow}\nSession status: {session_status}\nTicket: {ticket_id} - {ticket_title}\nTicket description: {ticket_description}\nPR: {pr_text}\nDiff summary: {files_changed} files, +{additions}/-{deletions}\nOpen inbox items: {open_inbox_count}\nRespond with a single-line status update under 200 characters.",
        session_id.as_str()
    );

    Some(SessionInfoContext {
        session_id: session_id.clone(),
        context_fingerprint,
        prompt,
    })
}

pub(crate) fn project_ui_state(
    status: &str,
    domain: &ProjectionState,
    view_stack: &ViewStack,
    preferred_selected_inbox: Option<usize>,
    preferred_selected_inbox_item_id: Option<&InboxItemId>,
    terminal_view_state: Option<&TerminalViewState>,
) -> UiState {
    let attention_projection = build_ui_attention_projection(domain);
    project_ui_state_with_attention(
        status,
        domain,
        view_stack,
        preferred_selected_inbox,
        preferred_selected_inbox_item_id,
        terminal_view_state,
        &attention_projection,
    )
}

fn project_ui_state_with_attention(
    status: &str,
    domain: &ProjectionState,
    view_stack: &ViewStack,
    preferred_selected_inbox: Option<usize>,
    preferred_selected_inbox_item_id: Option<&InboxItemId>,
    terminal_view_state: Option<&TerminalViewState>,
    attention_projection: &UiAttentionProjection,
) -> UiState {
    let inbox_rows = attention_projection.inbox_rows.clone();
    let inbox_batch_surfaces = attention_projection.inbox_batch_surfaces.clone();

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
    let active_center = view_stack.active_center().cloned().or_else(|| {
        selected_session_id
            .as_ref()
            .cloned()
            .map(|session_id| CenterView::TerminalView { session_id })
    });
    let center_pane = active_center
        .as_ref()
        .map(|center| {
            project_center_pane(
                center,
                &inbox_rows,
                domain,
                terminal_view_state,
            )
        })
        .unwrap_or_else(|| CenterPaneState {
            title: "Terminal".to_owned(),
            lines: vec!["No terminal output available yet.".to_owned()],
        });

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

fn build_ui_attention_projection(domain: &ProjectionState) -> UiAttentionProjection {
    let snapshot = attention_inbox_snapshot(domain, &AttentionEngineConfig::default(), &[]);
    build_ui_attention_projection_from_snapshot(snapshot)
}

fn build_ui_attention_projection_from_snapshot(
    snapshot: AttentionInboxSnapshot,
) -> UiAttentionProjection {
    let has_resolved_items = snapshot.items.iter().any(|item| item.resolved);
    let inbox_rows = snapshot
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
    let inbox_batch_surfaces = snapshot
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
    UiAttentionProjection {
        inbox_rows,
        inbox_batch_surfaces,
        has_resolved_items,
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
    domain: &ProjectionState,
    terminal_view_state: Option<&TerminalViewState>,
) -> CenterPaneState {
    match active_center {
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
pub(crate) struct TerminalViewState {
    entries: Vec<TerminalTranscriptEntry>,
    transcript_truncated: bool,
    transcript_truncated_line_count: usize,
    error: Option<String>,
    output_fragment: String,
    render_cache: TerminalRenderCache,
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
            transcript_truncated: false,
            transcript_truncated_line_count: 0,
            error: None,
            output_fragment: String::new(),
            render_cache: TerminalRenderCache::default(),
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

#[derive(Debug, Clone)]
struct TerminalRenderCache {
    transcript_lines: Vec<String>,
    rendered_row_counts: Vec<usize>,
    rendered_prefix_sums: Vec<usize>,
    width: u16,
    transcript_stale: bool,
    metrics_stale: bool,
}

impl Default for TerminalRenderCache {
    fn default() -> Self {
        Self {
            transcript_lines: Vec::new(),
            rendered_row_counts: Vec::new(),
            rendered_prefix_sums: Vec::new(),
            width: 0,
            transcript_stale: true,
            metrics_stale: true,
        }
    }
}

impl TerminalRenderCache {
    fn invalidate_all(&mut self) {
        self.transcript_stale = true;
        self.metrics_stale = true;
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
                prompt.default_option_labels,
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
    default_option_labels: Vec<Option<String>>,
    requires_manual_activation: bool,
    is_structured_plan_request: bool,
}

#[derive(Clone)]
struct NeedsInputComposerState {
    prompt_id: String,
    questions: Vec<BackendNeedsInputQuestion>,
    default_option_labels: Vec<Option<String>>,
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
            .field("default_option_labels", &self.default_option_labels)
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
        default_option_labels: Vec<Option<String>>,
        interaction_active: bool,
        is_structured_plan_request: bool,
    ) -> Self {
        let answer_drafts = vec![NeedsInputAnswerDraft::default(); questions.len()];
        let mut normalized_default_option_labels = default_option_labels;
        if normalized_default_option_labels.len() != questions.len() {
            normalized_default_option_labels.resize(questions.len(), None);
        }
        let mut state = Self {
            prompt_id,
            questions,
            default_option_labels: normalized_default_option_labels,
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
        let Some((options_len, options)) = self
            .current_question()
            .map(|question| {
                (
                    question.options.as_ref().map(Vec::len).unwrap_or(0),
                    question.options.clone(),
                )
            })
        else {
            self.select_state = SelectState::new(0);
            clear_editor_state(&mut self.note_editor_state);
            self.note_insert_mode = false;
            return;
        };

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
        } else if let Some(default_label) = self
            .default_option_labels
            .get(self.current_question_index)
            .and_then(|value| value.as_deref())
        {
            if let Some(default_index) = options
                .as_ref()
                .and_then(|entries| entries.iter().position(|option| option.label == default_label))
            {
                self.select_state.selected_index = Some(default_index);
                self.select_state.highlighted_index = default_index;
            }
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
#[allow(dead_code)]
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

impl From<ControllerSupervisorStreamRunnerEvent> for SupervisorStreamEvent {
    fn from(value: ControllerSupervisorStreamRunnerEvent) -> Self {
        match value {
            ControllerSupervisorStreamRunnerEvent::Started { stream_id } => {
                Self::Started { stream_id }
            }
            ControllerSupervisorStreamRunnerEvent::Delta { text } => Self::Delta { text },
            ControllerSupervisorStreamRunnerEvent::RateLimit { state } => {
                Self::RateLimit { state }
            }
            ControllerSupervisorStreamRunnerEvent::Usage { usage } => Self::Usage { usage },
            ControllerSupervisorStreamRunnerEvent::Finished { reason, usage } => {
                Self::Finished { reason, usage }
            }
            ControllerSupervisorStreamRunnerEvent::Failed { message } => {
                Self::Failed { message }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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

#[allow(dead_code)]
fn build_supervisor_chat_request(
    selected_row: &UiInboxRow,
    domain: &ProjectionState,
    supervisor_model: &str,
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
        model: supervisor_model.to_owned(),
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

#[allow(dead_code)]
fn build_global_supervisor_chat_request(query: &str, supervisor_model: &str) -> LlmChatRequest {
    LlmChatRequest {
        model: supervisor_model.to_owned(),
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
            (Some(session_id), status) => {
                session_display_with_status(domain, session_id, status.as_ref())
            }
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
        lines.push("Shortcuts: v d diff | v t tests | v p PR | v c chat".to_owned());
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

#[derive(Clone)]
struct TicketPickerOverlayState {
    visible: bool,
    loading: bool,
    starting_ticket_id: Option<TicketId>,
    archiving_ticket_id: Option<TicketId>,
    creating: bool,
    new_ticket_mode: bool,
    new_ticket_brief_editor: EditorState,
    new_ticket_brief_event_handler: EditorEventHandler,
    error: Option<String>,
    project_groups: Vec<TicketProjectGroup>,
    ticket_rows: Vec<TicketPickerRowRef>,
    selected_row_index: Option<usize>,
    repository_prompt_ticket: Option<TicketSummary>,
    repository_prompt_project_id: Option<String>,
    repository_prompt_input: InputState,
    repository_prompt_missing_mapping: bool,
    archive_confirm_ticket: Option<TicketSummary>,
}

impl Default for TicketPickerOverlayState {
    fn default() -> Self {
        Self {
            visible: false,
            loading: false,
            starting_ticket_id: None,
            archiving_ticket_id: None,
            creating: false,
            new_ticket_mode: false,
            new_ticket_brief_editor: insert_mode_editor_state(),
            new_ticket_brief_event_handler: EditorEventHandler::default(),
            error: None,
            project_groups: Vec::new(),
            ticket_rows: Vec::new(),
            selected_row_index: None,
            repository_prompt_ticket: None,
            repository_prompt_project_id: None,
            repository_prompt_input: InputState::empty(),
            repository_prompt_missing_mapping: false,
            archive_confirm_ticket: None,
        }
    }
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

    fn selected_project_name(&self) -> Option<String> {
        let project_index = self.selected_project_index()?;
        self.project_groups
            .get(project_index)
            .map(|project_group| project_group.project.clone())
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

    fn project_names(&self) -> Vec<String> {
        self.project_groups
            .iter()
            .map(|project_group| project_group.project.clone())
            .collect()
    }

    fn tickets_snapshot(&self) -> Vec<TicketSummary> {
        self.project_groups
            .iter()
            .flat_map(|project_group| {
                project_group
                    .status_groups
                    .iter()
                    .flat_map(|status_group| status_group.tickets.iter().cloned())
            })
            .collect()
    }

    fn open(&mut self) {
        self.visible = true;
        self.loading = true;
        self.archiving_ticket_id = None;
        self.creating = false;
        self.new_ticket_mode = false;
        clear_editor_state(&mut self.new_ticket_brief_editor);
        self.error = None;
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
        self.archive_confirm_ticket = None;
    }

    fn close(&mut self) {
        self.visible = false;
        self.loading = false;
        self.starting_ticket_id = None;
        self.archiving_ticket_id = None;
        self.creating = false;
        self.new_ticket_mode = false;
        clear_editor_state(&mut self.new_ticket_brief_editor);
        self.error = None;
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
        self.archive_confirm_ticket = None;
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
            self.repository_prompt_input.set_text(repository_path_hint);
            self.repository_prompt_missing_mapping = false;
        } else {
            self.repository_prompt_input.clear();
            self.repository_prompt_missing_mapping = true;
        }
    }

    fn cancel_repository_prompt(&mut self) {
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
    }

    fn begin_new_ticket_mode(&mut self) {
        if self.creating {
            return;
        }
        self.new_ticket_mode = true;
        self.error = None;
    }

    fn cancel_new_ticket_mode(&mut self) {
        self.new_ticket_mode = false;
        clear_editor_state(&mut self.new_ticket_brief_editor);
        self.creating = false;
    }

    fn can_submit_new_ticket(&self) -> bool {
        !self.creating && !editor_state_text(&self.new_ticket_brief_editor).trim().is_empty()
    }

    fn apply_tickets(
        &mut self,
        tickets: Vec<TicketSummary>,
        project_names: Vec<String>,
        priority_states: &[String],
    ) {
        self.apply_tickets_preferring(tickets, project_names, priority_states, None);
    }

    fn apply_tickets_preferring(
        &mut self,
        tickets: Vec<TicketSummary>,
        project_names: Vec<String>,
        priority_states: &[String],
        preferred_ticket_id: Option<&TicketId>,
    ) {
        let selected_project_index = self.selected_project_index();
        let selected_ticket_id = preferred_ticket_id
            .cloned()
            .or_else(|| self.selected_ticket().map(|ticket| ticket.ticket_id.clone()));
        let collapsed_projects = self
            .project_groups
            .iter()
            .filter(|project_group| project_group.collapsed)
            .map(|project_group| normalize_ticket_project(project_group.project.as_str()))
            .collect::<HashSet<_>>();
        self.project_groups =
            group_tickets_by_project(
                tickets,
                project_names,
                priority_states,
                &collapsed_projects,
            );
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
#[allow(dead_code)]
enum TicketPickerEvent {
    TicketsLoaded {
        tickets: Vec<TicketSummary>,
        projects: Vec<String>,
    },
    TicketsLoadFailed {
        message: String,
    },
    SessionWorkflowAdvanced {
        outcome: SessionWorkflowAdvanceOutcome,
    },
    SessionWorkflowAdvanceFailed {
        session_id: WorkerSessionId,
        message: String,
    },
    TicketStarted {
        started_session_id: WorkerSessionId,
        projection: Option<ProjectionState>,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketStartFailed {
        message: String,
    },
    TicketArchived {
        archived_ticket: TicketSummary,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketArchiveFailed {
        ticket: TicketSummary,
        message: String,
        tickets: Option<Vec<TicketSummary>>,
    },
    TicketCreated {
        created_ticket: TicketSummary,
        submit_mode: TicketCreateSubmitMode,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketCreateFailed {
        message: String,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketStartRequiresRepository {
        ticket: TicketSummary,
        project_id: String,
        repository_path_hint: Option<String>,
        message: String,
    },
    SessionDiffLoaded {
        diff: SessionWorktreeDiff,
    },
    SessionDiffFailed {
        session_id: WorkerSessionId,
        message: String,
    },
    SessionArchived {
        session_id: WorkerSessionId,
        warning: Option<String>,
        event: StoredEventEnvelope,
    },
    SessionArchiveFailed {
        session_id: WorkerSessionId,
        message: String,
    },
    InboxItemPublished {
        event: StoredEventEnvelope,
    },
    InboxItemPublishFailed {
        message: String,
    },
    InboxItemResolved {
        event: Option<StoredEventEnvelope>,
    },
    InboxItemResolveFailed {
        message: String,
    },
}

impl From<ControllerWorkflowAdvanceRunnerEvent> for TicketPickerEvent {
    fn from(value: ControllerWorkflowAdvanceRunnerEvent) -> Self {
        match value {
            ControllerWorkflowAdvanceRunnerEvent::Advanced { outcome } => {
                Self::SessionWorkflowAdvanced { outcome }
            }
            ControllerWorkflowAdvanceRunnerEvent::Failed {
                session_id,
                message,
            } => Self::SessionWorkflowAdvanceFailed {
                session_id,
                message,
            },
        }
    }
}

impl From<ControllerInboxPublishRunnerEvent> for TicketPickerEvent {
    fn from(value: ControllerInboxPublishRunnerEvent) -> Self {
        match value {
            ControllerInboxPublishRunnerEvent::Published { event } => {
                Self::InboxItemPublished { event }
            }
            ControllerInboxPublishRunnerEvent::Failed { message } => {
                Self::InboxItemPublishFailed { message }
            }
        }
    }
}

impl From<ControllerInboxResolveRunnerEvent> for TicketPickerEvent {
    fn from(value: ControllerInboxResolveRunnerEvent) -> Self {
        match value {
            ControllerInboxResolveRunnerEvent::Resolved { event } => {
                Self::InboxItemResolved { event }
            }
            ControllerInboxResolveRunnerEvent::Failed { message } => {
                Self::InboxItemResolveFailed { message }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeDiffModalState {
    session_id: WorkerSessionId,
    base_branch: String,
    content: String,
    loading: bool,
    error: Option<String>,
    scroll: u16,
    cursor_line: usize,
    selected_file_index: usize,
    selected_hunk_index: usize,
    focus: DiffPaneFocus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffPaneFocus {
    Files,
    Diff,
}

#[derive(Debug, Clone)]
struct MergeQueueRequest {
    session_id: WorkerSessionId,
    _context: SupervisorCommandContext,
    kind: MergeQueueCommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationState {
    None,
    ActiveTurn,
    ResolvedOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingWorkingStatePersist {
    is_working: bool,
    deadline: Instant,
}

#[allow(dead_code)]
struct UiShellState {
    runtime_config: UiRuntimeConfig,
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
    application_mode: ApplicationMode,
    terminal_escape_pending: bool,
    supervisor_provider: Option<Arc<dyn LlmProvider>>,
    supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
    supervisor_chat_stream: Option<ActiveSupervisorChatStream>,
    global_supervisor_chat_input: InputState,
    global_supervisor_chat_last_query: Option<String>,
    global_supervisor_chat_return_context: Option<GlobalSupervisorChatReturnContext>,
    ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
    ticket_picker_sender: Option<mpsc::Sender<TicketPickerEvent>>,
    ticket_picker_receiver: Option<mpsc::Receiver<TicketPickerEvent>>,
    ticket_picker_overlay: TicketPickerOverlayState,
    ticket_picker_create_refresh_deadline: Option<Instant>,
    ticket_picker_priority_states: Vec<String>,
    startup_session_feed_opened: bool,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
    frontend_controller: Option<Arc<dyn FrontendController>>,
    frontend_terminal_streaming_enabled: bool,
    selected_session_index: Option<usize>,
    selected_session_id: Option<WorkerSessionId>,
    #[cfg_attr(not(test), allow(dead_code))]
    terminal_session_sender: Option<mpsc::Sender<TerminalSessionEvent>>,
    terminal_session_receiver: Option<mpsc::Receiver<TerminalSessionEvent>>,
    session_info_summary_sender: Option<mpsc::Sender<SessionInfoSummaryEvent>>,
    session_info_summary_receiver: Option<mpsc::Receiver<SessionInfoSummaryEvent>>,
    terminal_session_states: HashMap<WorkerSessionId, TerminalViewState>,
    session_info_diff_cache: HashMap<WorkerSessionId, SessionInfoDiffCache>,
    session_info_summary_cache: HashMap<WorkerSessionId, SessionInfoSummaryCache>,
    session_info_summary_deadline: Option<Instant>,
    pending_session_working_state_persists: HashMap<WorkerSessionId, PendingWorkingStatePersist>,
    session_info_summary_last_refresh_at: HashMap<WorkerSessionId, Instant>,
    session_info_diff_last_refresh_at: HashMap<WorkerSessionId, Instant>,
    background_terminal_flush_deadline: Option<Instant>,
    terminal_session_streamed: HashSet<WorkerSessionId>,
    terminal_compose_editor: EditorState,
    terminal_compose_event_handler: EditorEventHandler,
    session_panel_scroll_line: usize,
    session_panel_viewport_rows: usize,
    session_panel_rendered_line_count: usize,
    archive_session_confirm_session: Option<WorkerSessionId>,
    archiving_session_id: Option<WorkerSessionId>,
    review_merge_confirm_session: Option<WorkerSessionId>,
    merge_queue: VecDeque<MergeQueueRequest>,
    merge_last_dispatched_at: Option<Instant>,
    merge_event_sender: Option<mpsc::Sender<MergeQueueEvent>>,
    merge_event_receiver: Option<mpsc::Receiver<MergeQueueEvent>>,
    review_reconcile_eligible_sessions: HashSet<WorkerSessionId>,
    approval_reconcile_candidate_sessions: HashSet<WorkerSessionId>,
    merge_pending_sessions: HashSet<WorkerSessionId>,
    merge_poll_states: HashMap<WorkerSessionId, MergeReconcilePollState>,
    merge_finalizing_sessions: HashSet<WorkerSessionId>,
    review_sync_instructions_sent: HashSet<WorkerSessionId>,
    dirty_review_reconcile_sessions: HashSet<WorkerSessionId>,
    dirty_approval_reconcile_sessions: HashSet<WorkerSessionId>,
    session_ci_status_cache: HashMap<WorkerSessionId, SessionCiStatusCache>,
    ci_failure_signatures_notified: HashMap<WorkerSessionId, String>,
    autopilot_advancing_sessions: HashSet<WorkerSessionId>,
    autopilot_archiving_sessions: HashSet<WorkerSessionId>,
    worktree_diff_modal: Option<WorktreeDiffModalState>,
    draw_cache_epoch: u64,
    attention_projection_cache: Option<AttentionProjectionCache>,
    session_panel_rows_cache: Option<SessionPanelRowsCache>,
    event_derived_label_cache: EventDerivedLabelCache,
    full_projection_replacements: u64,
    incremental_domain_event_applies: u64,
    attention_projection_recomputes: u64,
    last_projection_perf_log_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct MergeReconcilePollState {
    next_poll_at: Instant,
    backoff: Duration,
    consecutive_failures: u32,
    last_poll_started_at: Option<Instant>,
    last_api_error_signature: Option<String>,
}

impl MergeReconcilePollState {
    fn new(now: Instant, base: Duration) -> Self {
        Self {
            next_poll_at: now + base,
            backoff: base,
            consecutive_failures: 0,
            last_poll_started_at: None,
            last_api_error_signature: None,
        }
    }
}

#[allow(dead_code)]
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
        Self::new_with_integrations(status, domain, supervisor_provider, None, None, None)
    }

    #[cfg(test)]
    fn new_with_integrations(
        status: String,
        domain: ProjectionState,
        supervisor_provider: Option<Arc<dyn LlmProvider>>,
        supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
        ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
        worker_backend: Option<Arc<dyn WorkerBackend>>,
    ) -> Self {
        Self::new_with_integrations_and_config(
            status,
            domain,
            supervisor_provider,
            supervisor_command_dispatcher,
            ticket_picker_provider,
            worker_backend,
            UiRuntimeConfig::default(),
        )
    }

    fn new_with_integrations_and_config(
        status: String,
        domain: ProjectionState,
        supervisor_provider: Option<Arc<dyn LlmProvider>>,
        supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
        ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
        worker_backend: Option<Arc<dyn WorkerBackend>>,
        runtime_config: UiRuntimeConfig,
    ) -> Self {
        let (ticket_picker_sender, ticket_picker_receiver) = if ticket_picker_provider.is_some() {
            let (sender, receiver) = mpsc::channel(TICKET_PICKER_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let (terminal_session_sender, terminal_session_receiver) = if worker_backend.is_some() {
            let (sender, receiver) = mpsc::channel(TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let (session_info_summary_sender, session_info_summary_receiver) =
            if supervisor_provider.is_some() {
                let (sender, receiver) = mpsc::channel(TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY);
                (Some(sender), Some(receiver))
            } else {
                (None, None)
            };
        let (merge_event_sender, merge_event_receiver) = if ticket_picker_provider.is_some() {
            let (sender, receiver) = mpsc::channel(TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };

        let mut shell = Self {
            runtime_config: runtime_config.clone(),
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
            application_mode: ApplicationMode::Manual,
            terminal_escape_pending: false,
            supervisor_provider,
            supervisor_command_dispatcher,
            supervisor_chat_stream: None,
            global_supervisor_chat_input: InputState::empty(),
            global_supervisor_chat_last_query: None,
            global_supervisor_chat_return_context: None,
            ticket_picker_provider,
            ticket_picker_sender,
            ticket_picker_receiver,
            ticket_picker_overlay: TicketPickerOverlayState::default(),
            ticket_picker_create_refresh_deadline: None,
            ticket_picker_priority_states: runtime_config.ticket_picker_priority_states.clone(),
            startup_session_feed_opened: false,
            worker_backend,
            frontend_controller: None,
            frontend_terminal_streaming_enabled: false,
            selected_session_index: None,
            selected_session_id: None,
            terminal_session_sender,
            terminal_session_receiver,
            session_info_summary_sender,
            session_info_summary_receiver,
            terminal_session_states: HashMap::new(),
            session_info_diff_cache: HashMap::new(),
            session_info_summary_cache: HashMap::new(),
            session_info_summary_deadline: None,
            pending_session_working_state_persists: HashMap::new(),
            session_info_summary_last_refresh_at: HashMap::new(),
            session_info_diff_last_refresh_at: HashMap::new(),
            background_terminal_flush_deadline: None,
            terminal_session_streamed: HashSet::new(),
            terminal_compose_editor: insert_mode_editor_state(),
            terminal_compose_event_handler: EditorEventHandler::default(),
            session_panel_scroll_line: 0,
            session_panel_viewport_rows: 1,
            session_panel_rendered_line_count: 0,
            archive_session_confirm_session: None,
            archiving_session_id: None,
            review_merge_confirm_session: None,
            merge_queue: VecDeque::new(),
            merge_last_dispatched_at: None,
            merge_event_sender: merge_event_sender.clone(),
            merge_event_receiver,
            review_reconcile_eligible_sessions: HashSet::new(),
            approval_reconcile_candidate_sessions: HashSet::new(),
            merge_pending_sessions: HashSet::new(),
            merge_poll_states: HashMap::new(),
            merge_finalizing_sessions: HashSet::new(),
            review_sync_instructions_sent: HashSet::new(),
            dirty_review_reconcile_sessions: HashSet::new(),
            dirty_approval_reconcile_sessions: HashSet::new(),
            session_ci_status_cache: HashMap::new(),
            ci_failure_signatures_notified: HashMap::new(),
            autopilot_advancing_sessions: HashSet::new(),
            autopilot_archiving_sessions: HashSet::new(),
            worktree_diff_modal: None,
            draw_cache_epoch: 0,
            attention_projection_cache: None,
            session_panel_rows_cache: None,
            event_derived_label_cache: EventDerivedLabelCache::default(),
            full_projection_replacements: 0,
            incremental_domain_event_applies: 0,
            attention_projection_recomputes: 0,
            last_projection_perf_log_at: None,
        };
        shell.refresh_reconcile_eligibility_for_all_sessions();
        shell.mark_all_eligible_reconcile_dirty();
        shell.start_pr_pipeline_polling(merge_event_sender.clone());
        shell
    }

    fn invalidate_draw_caches(&mut self) {
        self.draw_cache_epoch = self.draw_cache_epoch.wrapping_add(1);
    }

    fn set_frontend_terminal_streaming_enabled(&mut self, enabled: bool) {
        self.frontend_terminal_streaming_enabled = enabled;
    }

    fn set_frontend_controller(&mut self, controller: Option<Arc<dyn FrontendController>>) {
        self.frontend_controller = controller;
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

    fn ui_state_for_draw(&mut self, now: Instant) -> UiState {
        let status = self.status_text();
        let attention_projection = self.attention_projection_for_draw(now);
        let terminal_view_state = None;
        let mut ui_state = project_ui_state_with_attention(
            status.as_str(),
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
            terminal_view_state,
            attention_projection.as_ref(),
        );
        self.append_global_supervisor_chat_state(&mut ui_state);
        self.append_live_supervisor_chat(&mut ui_state);
        ui_state
    }

    fn attention_projection_for_draw(&mut self, now: Instant) -> Arc<UiAttentionProjection> {
        let should_refresh = self
            .attention_projection_cache
            .as_ref()
            .map(|cache| {
                if cache.epoch != self.draw_cache_epoch {
                    return true;
                }
                cache.projection.has_resolved_items
                    && now.duration_since(cache.refreshed_at) >= RESOLVED_ANIMATION_REFRESH_INTERVAL
            })
            .unwrap_or(true);
        if should_refresh {
            self.attention_projection_recomputes =
                self.attention_projection_recomputes.saturating_add(1);
            self.attention_projection_cache = Some(AttentionProjectionCache {
                projection: Arc::new(build_ui_attention_projection(&self.domain)),
                refreshed_at: now,
                epoch: self.draw_cache_epoch,
            });
        }
        self.attention_projection_cache
            .as_ref()
            .expect("attention projection cache should be present")
            .projection
            .clone()
    }

    fn status_text(&self) -> String {
        let base_status = sanitize_terminal_display_text(self.base_status.as_str());
        match self.status_warning.as_deref() {
            Some(warning) => format!(
                "{} | warning: {}",
                base_status,
                sanitize_terminal_display_text(warning)
            ),
            None => base_status,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let _ = self.move_session_selection(delta);
    }

    fn jump_to_first_item(&mut self) {
        let _ = self.move_to_first_session();
    }

    fn jump_to_last_item(&mut self) {
        let _ = self.move_to_last_session();
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

    fn find_next_inbox_session_selection_index(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<(usize, Vec<UiInboxRow>)> {
        let ui_state = self.ui_state();
        let current_index = ui_state
            .inbox_rows
            .iter()
            .position(|row| row.session_id.as_ref() == Some(session_id))?;
        let next_index = ui_state
            .inbox_rows
            .iter()
            .enumerate()
            .skip(current_index + 1)
            .find_map(|(index, row)| row.session_id.as_ref().map(|_| index))?;
        Some((next_index, ui_state.inbox_rows))
    }

    fn auto_advance_inbox_selection_after_workflow_progression(
        &mut self,
        session_id: &WorkerSessionId,
    ) {
        let Some((next_index, rows)) = self.find_next_inbox_session_selection_index(session_id)
        else {
            return;
        };
        self.set_selection(Some(next_index), &rows);
        let _ = self.open_selected_inbox_output(false);
    }

    fn open_terminal_for_selected(&mut self) {
        if self.open_selected_inbox_output(false) {
            return;
        }

        if let Some(session_id) = self.selected_session_id_for_panel() {
            self.view_stack.replace_center(CenterView::TerminalView {
                session_id: session_id.clone(),
            });
            let _ = self.flush_deferred_terminal_output_for_session(&session_id);
            self.enter_terminal_mode();
            self.schedule_session_info_summary_refresh_for_active_session();
        } else {
            self.status_warning = Some(
                "terminal unavailable: no open session is currently selected".to_owned(),
            );
        }
    }

    fn open_session_output_for_selected_inbox(&mut self) {
        let _ = self.open_selected_inbox_output(true);
    }

    fn open_selected_inbox_output(&mut self, acknowledge_selection: bool) -> bool {
        let ui_state = self.ui_state();
        let Some(selected_index) = ui_state.selected_inbox_index else {
            self.status_warning =
                Some("session output unavailable: select an inbox item first".to_owned());
            return false;
        };
        let Some(selected_row) = ui_state.inbox_rows.get(selected_index).cloned() else {
            self.status_warning =
                Some("session output unavailable: select an inbox item first".to_owned());
            return false;
        };
        let Some(session_id) = selected_row.session_id.clone() else {
            self.status_warning =
                Some("session output unavailable: selected inbox item has no active session".to_owned());
            return false;
        };

        if let Some(index) = self
            .session_ids_for_navigation()
            .iter()
            .position(|candidate| candidate == &session_id)
        {
            self.selected_session_index = Some(index);
        }
        self.selected_session_id = Some(session_id.clone());
        self.view_stack.replace_center(CenterView::TerminalView {
            session_id: session_id.clone(),
        });
        let _ = self.flush_deferred_terminal_output_for_session(&session_id);
        self.enter_terminal_mode();
        self.status_warning = None;

        if acknowledge_selection {
            self.acknowledge_inbox_item(selected_row.inbox_item_id, selected_row.work_item_id);
        }

        true
    }

    fn selected_session_id_for_terminal_action(&self) -> Option<WorkerSessionId> {
        self.active_terminal_session_id()
            .cloned()
            .or_else(|| self.selected_session_id_for_panel())
    }

    fn begin_archive_selected_session_confirmation(&mut self) {
        if self.archive_session_confirm_session.is_some() || self.archiving_session_id.is_some() {
            return;
        }
        let Some(session_id) = self.selected_session_id_for_terminal_action() else {
            self.status_warning = Some("session archive unavailable: no session selected".to_owned());
            return;
        };
        self.archive_session_confirm_session = Some(session_id);
        self.status_warning = None;
    }

    fn cancel_archive_selected_session_confirmation(&mut self) {
        self.archive_session_confirm_session = None;
    }

    fn confirm_archive_selected_session(&mut self) {
        let Some(session_id) = self.archive_session_confirm_session.take() else {
            return;
        };
        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!(
            "session archive action for {} is handled outside the TUI loop",
            labels.compact_label
        ));
    }

    fn session_panel_rows(&self) -> Vec<SessionPanelRow> {
        session_panel_rows(&self.domain, &self.terminal_session_states)
    }

    fn session_panel_rows_for_draw(&mut self) -> &[SessionPanelRow] {
        let cache_is_fresh = self
            .session_panel_rows_cache
            .as_ref()
            .map(|cache| cache.epoch == self.draw_cache_epoch)
            .unwrap_or(false);
        if !cache_is_fresh {
            self.event_derived_label_cache.refresh(&self.domain);
            self.session_panel_rows_cache = Some(SessionPanelRowsCache {
                rows: session_panel_rows_with_labels(
                    &self.domain,
                    &self.terminal_session_states,
                    &self.event_derived_label_cache.work_item_repo,
                    &self.event_derived_label_cache.ticket_labels,
                ),
                epoch: self.draw_cache_epoch,
            });
        }
        self.session_panel_rows_cache
            .as_ref()
            .expect("session panel rows cache should be present")
            .rows
            .as_slice()
    }

    fn session_ids_for_navigation(&self) -> Vec<WorkerSessionId> {
        self.session_panel_rows()
            .into_iter()
            .map(|row| row.session_id)
            .collect()
    }

    fn selected_session_id_for_panel(&self) -> Option<WorkerSessionId> {
        let session_ids = self.session_ids_for_navigation();
        self.selected_session_id_for_panel_from_ids(session_ids.as_slice())
    }

    fn selected_session_id_for_panel_from_ids(
        &self,
        session_ids: &[WorkerSessionId],
    ) -> Option<WorkerSessionId> {
        if session_ids.is_empty() {
            return None;
        }
        if let Some(selected_session_id) = self.selected_session_id.as_ref() {
            if session_ids
                .iter()
                .any(|candidate| candidate == selected_session_id)
            {
                return Some(selected_session_id.clone());
            }
        }
        let index = self
            .selected_session_index
            .unwrap_or(0)
            .min(session_ids.len() - 1);
        session_ids.get(index).cloned()
    }

    fn sync_selected_session_panel_state(&mut self) {
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            self.selected_session_index = None;
            self.selected_session_id = None;
            return;
        }

        if let Some(selected_session_id) = self.selected_session_id.as_ref() {
            if let Some(index) = session_ids
                .iter()
                .position(|candidate| candidate == selected_session_id)
            {
                self.selected_session_index = Some(index);
                return;
            }
        }

        let index = self
            .selected_session_index
            .unwrap_or(0)
            .min(session_ids.len() - 1);
        self.selected_session_index = Some(index);
        self.selected_session_id = session_ids.get(index).cloned();
    }

    fn should_show_session_info_sidebar(&self) -> bool {
        self.active_terminal_session_id().is_some()
    }

    fn sync_session_panel_viewport(
        &mut self,
        rendered_line_count: usize,
        selected_line: Option<usize>,
        viewport_rows: usize,
    ) {
        self.session_panel_rendered_line_count = rendered_line_count;
        self.session_panel_viewport_rows = viewport_rows.max(1);
        if rendered_line_count == 0 {
            self.session_panel_scroll_line = 0;
            return;
        }

        let max_scroll = rendered_line_count.saturating_sub(self.session_panel_viewport_rows);
        let mut next_scroll = self.session_panel_scroll_line.min(max_scroll);
        if let Some(selected_line) = selected_line {
            if selected_line < next_scroll {
                next_scroll = selected_line;
            } else if selected_line >= next_scroll + self.session_panel_viewport_rows {
                next_scroll = selected_line
                    .saturating_add(1)
                    .saturating_sub(self.session_panel_viewport_rows)
                    .min(max_scroll);
            }
        }
        self.session_panel_scroll_line = next_scroll;
    }

    fn session_panel_scroll_line(&self) -> usize {
        self.session_panel_scroll_line
    }

    fn session_info_is_foreground(&self) -> bool {
        self.active_terminal_session_id().is_some() && self.mode == UiMode::Terminal
    }

    fn session_info_diff_cache_for(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<&SessionInfoDiffCache> {
        self.session_info_diff_cache.get(session_id)
    }

    fn session_info_summary_cache_for(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<&SessionInfoSummaryCache> {
        self.session_info_summary_cache.get(session_id)
    }

    fn session_ci_status_cache_for(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<&SessionCiStatusCache> {
        self.session_ci_status_cache.get(session_id)
    }

    fn move_session_selection(&mut self, delta: isize) -> bool {
        self.sync_selected_session_panel_state();
        let session_ids = self.session_ids_for_navigation();
        let len = session_ids.len();
        if len == 0 {
            self.selected_session_index = None;
            self.selected_session_id = None;
            return false;
        }

        let mut index = self.selected_session_index.unwrap_or(0);
        if index >= len {
            index = len - 1;
        }
        let next = (index as isize + delta).rem_euclid(len as isize) as usize;
        self.selected_session_index = Some(next);
        self.selected_session_id = session_ids.get(next).cloned();
        self.show_selected_session_output();
        true
    }

    fn move_to_first_session(&mut self) -> bool {
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            self.selected_session_index = None;
            self.selected_session_id = None;
            false
        } else {
            self.selected_session_index = Some(0);
            self.selected_session_id = session_ids.first().cloned();
            self.show_selected_session_output();
            true
        }
    }

    fn move_to_last_session(&mut self) -> bool {
        let session_ids = self.session_ids_for_navigation();
        let len = session_ids.len();
        if len == 0 {
            self.selected_session_index = None;
            self.selected_session_id = None;
            false
        } else {
            self.selected_session_index = Some(len - 1);
            self.selected_session_id = session_ids.last().cloned();
            self.show_selected_session_output();
            true
        }
    }

    fn show_selected_session_output(&mut self) {
        self.sync_selected_session_panel_state();
        let Some(session_id) = self.selected_session_id_for_panel() else {
            return;
        };
        let should_switch_view = !matches!(
            self.view_stack.active_center(),
            Some(CenterView::TerminalView {
                session_id: active_session_id
            }) if *active_session_id == session_id
        );
        if should_switch_view {
            self.view_stack.replace_center(CenterView::TerminalView {
                session_id: session_id.clone(),
            });
        }
        let _ = self.flush_deferred_terminal_output_for_session(&session_id);
        self.schedule_session_info_summary_refresh_for_active_session();
    }

    fn ensure_startup_session_feed_opened_and_report(&mut self) -> bool {
        if self.startup_session_feed_opened {
            return false;
        }
        if self.active_terminal_session_id().is_some() {
            self.startup_session_feed_opened = true;
            return true;
        }
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            return false;
        }
        self.sync_selected_session_panel_state();
        self.show_selected_session_output();
        self.startup_session_feed_opened = true;
        true
    }

    fn focus_and_stream_session(&mut self, session_id: WorkerSessionId) {
        self.selected_session_id = Some(session_id.clone());
        let session_ids = self.session_ids_for_navigation();
        if let Some(index) = session_ids.iter().position(|candidate| candidate == &session_id) {
            self.selected_session_index = Some(index);
        }

        self.view_stack.replace_center(CenterView::TerminalView {
            session_id: session_id.clone(),
        });

        let _ = self.flush_deferred_terminal_output_for_session(&session_id);
        self.enter_terminal_mode();
        self.schedule_session_info_summary_refresh_for_active_session();
    }

    fn work_item_id_for_session(&self, session_id: &WorkerSessionId) -> Option<WorkItemId> {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.clone())
    }

    fn session_id_for_work_item(&self, work_item_id: &WorkItemId) -> Option<WorkerSessionId> {
        self.domain
            .work_items
            .get(work_item_id)
            .and_then(|work_item| work_item.session_id.clone())
    }

    fn spawn_publish_inbox_item(&mut self, request: InboxPublishRequest) {
        if request.title.trim().is_empty() || request.coalesce_key.trim().is_empty() {
            return;
        }
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_publish_inbox_item_task(provider, request, sender).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "inbox publish unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn spawn_resolve_inbox_item(&mut self, request: InboxResolveRequest) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_resolve_inbox_item_task(provider, request, sender).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "inbox resolution unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn spawn_set_session_working_state(
        &mut self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_set_session_working_state_task(provider, session_id, is_working).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "session working-state persistence unavailable: tokio runtime is not active"
                        .to_owned(),
                );
            }
        }
    }

    fn enqueue_or_persist_session_working_state(
        &mut self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) {
        self.domain
            .session_runtime
            .insert(session_id.clone(), SessionRuntimeProjection { is_working });

        if self.session_is_in_planning_stage(&session_id) {
            self.pending_session_working_state_persists.insert(
                session_id,
                PendingWorkingStatePersist {
                    is_working,
                    deadline: Instant::now() + PLANNING_WORKING_STATE_PERSIST_DEBOUNCE,
                },
            );
            return;
        }

        self.persist_session_working_state_immediately(session_id, is_working);
    }

    fn persist_session_working_state_immediately(
        &mut self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) {
        self.domain
            .session_runtime
            .insert(session_id.clone(), SessionRuntimeProjection { is_working });
        self.pending_session_working_state_persists
            .remove(&session_id);
        self.spawn_set_session_working_state(session_id, is_working);
    }

    fn flush_due_session_working_state_persists(&mut self) -> bool {
        if self.pending_session_working_state_persists.is_empty() {
            return false;
        }

        let now = Instant::now();
        let due_persists = self
            .pending_session_working_state_persists
            .iter()
            .filter_map(|(session_id, pending)| {
                if now >= pending.deadline || !self.session_is_in_planning_stage(session_id) {
                    Some((session_id.clone(), pending.is_working))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for (session_id, is_working) in due_persists.iter() {
            self.persist_session_working_state_immediately(session_id.clone(), *is_working);
        }

        !due_persists.is_empty()
    }

    fn acknowledge_inbox_item(&mut self, inbox_item_id: InboxItemId, work_item_id: WorkItemId) {
        if let Some(item) = self.domain.inbox_items.get_mut(&inbox_item_id) {
            item.resolved = true;
        }
        self.spawn_resolve_inbox_item(InboxResolveRequest {
            inbox_item_id,
            work_item_id,
        });
    }

    fn acknowledge_needs_decision_for_work_item(&mut self, work_item_id: &WorkItemId) {
        let Some(work_item) = self.domain.work_items.get(work_item_id) else {
            return;
        };
        let to_resolve = work_item
            .inbox_items
            .iter()
            .filter_map(|inbox_item_id| {
                self.domain
                    .inbox_items
                    .get(inbox_item_id)
                    .filter(|item| !item.resolved && item.kind == InboxItemKind::NeedsDecision)
                    .map(|_| inbox_item_id.clone())
            })
            .collect::<Vec<_>>();

        for inbox_item_id in to_resolve {
            if let Some(item) = self.domain.inbox_items.get_mut(&inbox_item_id) {
                item.resolved = true;
            }
            self.spawn_resolve_inbox_item(InboxResolveRequest {
                inbox_item_id,
                work_item_id: work_item_id.clone(),
            });
        }
    }

    fn publish_inbox_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        kind: InboxItemKind,
        title: String,
        coalesce_key: &str,
    ) {
        if !is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) {
            return;
        }

        let Some(work_item_id) = self.work_item_id_for_session(session_id) else {
            return;
        };
        self.spawn_publish_inbox_item(InboxPublishRequest {
            work_item_id,
            session_id: Some(session_id.clone()),
            kind,
            title,
            coalesce_key: coalesce_key.to_owned(),
        });
    }

    fn progression_approval_coalesce_key() -> &'static str {
        "workflow-awaiting-progression"
    }

    fn progression_approval_title_prefix() -> &'static str {
        "Approval needed to progress this ticket"
    }

    fn expected_progression_approval_inbox_id(
        session_id: &WorkerSessionId,
    ) -> InboxItemId {
        InboxItemId::new(format!(
            "inbox-{}-{}",
            session_id.as_str(),
            Self::progression_approval_coalesce_key()
        ))
    }

    fn session_is_actively_working(&self, session_id: &WorkerSessionId) -> bool {
        if let Some(state) = self.terminal_session_states.get(session_id) {
            if state.active_needs_input.is_some() || !state.pending_needs_input_prompts.is_empty() {
                return false;
            }
            return state.turn_active;
        }

        self.domain
            .session_runtime
            .get(session_id)
            .map(|runtime| runtime.is_working)
            .unwrap_or(true)
    }

    fn session_waiting_for_plan_input(&self, session_id: &WorkerSessionId) -> bool {
        if !matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning)
        ) {
            return false;
        }

        let has_prompt = self
            .terminal_session_states
            .get(session_id)
            .map(|state| {
                state.active_needs_input.is_some() || !state.pending_needs_input_prompts.is_empty()
            })
            .unwrap_or(false);

        if matches!(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
            Some(WorkerSessionStatus::WaitingForUser)
        ) {
            return has_prompt;
        }

        has_prompt
    }

    fn session_requires_progression_approval(&self, session_id: &WorkerSessionId) -> bool {
        if !matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning | WorkflowState::Implementing)
        ) {
            return false;
        }

        if self.session_is_actively_working(session_id) {
            return false;
        }

        !self.session_waiting_for_plan_input(session_id)
    }

    fn session_is_in_planning_stage(&self, session_id: &WorkerSessionId) -> bool {
        matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning)
        )
    }

    fn find_progression_approval_inbox_for_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<(InboxItemId, WorkItemId)> {
        let expected_id = Self::expected_progression_approval_inbox_id(session_id);
        let work_item_id = self.work_item_id_for_session(session_id)?;
        self.domain
            .inbox_items
            .get(&expected_id)
            .filter(|item| item.kind == InboxItemKind::NeedsApproval && !item.resolved)
            .map(|_| (expected_id, work_item_id))
    }

    fn reconcile_progression_approval_inbox_for_session(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> bool {
        if !is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) {
            return false;
        }

        if self.session_requires_progression_approval(session_id) {
            self.publish_inbox_for_session(
                session_id,
                InboxItemKind::NeedsApproval,
                format!(
                    "{}: {}",
                    Self::progression_approval_title_prefix(),
                    session_display_labels(&self.domain, session_id).compact_label
                ),
                Self::progression_approval_coalesce_key(),
            );
            return true;
        }

        if let Some((inbox_item_id, work_item_id)) =
            self.find_progression_approval_inbox_for_session(session_id)
        {
            self.spawn_resolve_inbox_item(InboxResolveRequest {
                inbox_item_id,
                work_item_id,
            });
            return true;
        }

        false
    }

    fn workflow_state_for_session(&self, session_id: &WorkerSessionId) -> Option<WorkflowState> {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.clone())
    }

    fn route_needs_input_inbox_for_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<(InboxItemKind, &'static str, &'static str)> {
        match self.workflow_state_for_session(session_id) {
            Some(WorkflowState::New | WorkflowState::Planning) => Some((
                InboxItemKind::NeedsDecision,
                "plan-input-request",
                "Plan input request",
            )),
            Some(
                WorkflowState::AwaitingYourReview
                | WorkflowState::ReadyForReview
                | WorkflowState::InReview
                | WorkflowState::PendingMerge,
            ) => Some((
                InboxItemKind::ReadyForReview,
                "review-input-request",
                "Review input request",
            )),
            Some(WorkflowState::Done | WorkflowState::Abandoned) => None,
            Some(WorkflowState::Implementing | WorkflowState::PRDrafted) | None => Some((
                InboxItemKind::NeedsApproval,
                Self::progression_approval_coalesce_key(),
                "Worker waiting for progression",
            )),
        }
    }

    fn workflow_transition_inbox_for_state(
        workflow_state: &WorkflowState,
    ) -> Option<(InboxItemKind, &'static str, &'static str)> {
        match workflow_state {
            WorkflowState::PRDrafted => Some((
                InboxItemKind::NeedsApproval,
                Self::progression_approval_coalesce_key(),
                "Approval needed to progress this ticket",
            )),
            WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview
            | WorkflowState::PendingMerge => Some((
                InboxItemKind::ReadyForReview,
                "review-idle",
                "Ticket is idle in review stage",
            )),
            _ => None,
        }
    }

    fn publish_error_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        source_key: &str,
        message: &str,
    ) {
        self.publish_inbox_for_session(
            session_id,
            InboxItemKind::Blocked,
            format!("Error: {}", compact_focus_card_text(message)),
            format!("error-{source_key}").as_str(),
        );
    }

    fn publish_error_for_work_item(
        &mut self,
        work_item_id: &WorkItemId,
        session_id: Option<WorkerSessionId>,
        source_key: &str,
        message: &str,
    ) {
        self.spawn_publish_inbox_item(InboxPublishRequest {
            work_item_id: work_item_id.clone(),
            session_id,
            kind: InboxItemKind::Blocked,
            title: format!("Error: {}", compact_focus_card_text(message)),
            coalesce_key: format!("error-{source_key}"),
        });
    }

    fn needs_input_summary(event: &BackendNeedsInputEvent) -> String {
        let summary = event
            .questions
            .first()
            .map(|question| question.question.as_str())
            .unwrap_or_else(|| event.question.as_str());
        compact_focus_card_text(summary)
    }

    fn active_terminal_session_id(&self) -> Option<&WorkerSessionId> {
        match self.view_stack.active_center() {
            Some(CenterView::TerminalView { session_id }) => Some(session_id),
            _ => None,
        }
    }

    fn active_terminal_view_state_mut(&mut self) -> Option<&mut TerminalViewState> {
        let session_id = self.active_terminal_session_id()?.clone();
        self.terminal_session_states.get_mut(&session_id)
    }

    fn ui_theme(&self) -> UiTheme {
        ui_theme_from_config_value(self.runtime_config.theme.as_str())
    }

    fn terminal_total_rendered_rows_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        width: u16,
        indicator: TerminalActivityIndicator,
    ) -> usize {
        let theme = self.ui_theme();
        self.terminal_session_states
            .get_mut(session_id)
            .map(|view| terminal_total_rendered_rows(view, width, indicator, theme))
            .unwrap_or(0)
    }

    fn render_terminal_output_viewport_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        request: TerminalViewportRequest,
    ) -> Option<TerminalViewportRender> {
        let theme = self.ui_theme();
        let view = self.terminal_session_states.get_mut(session_id)?;
        Some(render_terminal_output_viewport(view, request, theme))
    }

    fn snap_active_terminal_output_to_bottom(&mut self) {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return;
        };

        let rendered_line_count = terminal_output_line_count_for_scroll(view);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return;
        }

        view.output_follow_tail = true;
        view.output_scroll_line = rendered_line_count.saturating_sub(view.output_viewport_rows.max(1));
    }

    fn sync_terminal_output_viewport(&mut self, rendered_line_count: usize, viewport_rows: usize) {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return;
        };

        view.output_rendered_line_count = rendered_line_count;
        view.output_viewport_rows = viewport_rows.max(1);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return;
        }

        let max_scroll = rendered_line_count.saturating_sub(view.output_viewport_rows);
        if view.output_follow_tail {
            view.output_scroll_line = max_scroll;
            return;
        }

        view.output_scroll_line = view.output_scroll_line.min(max_scroll);
    }

    fn scroll_terminal_output_view(&mut self, delta: isize) -> bool {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return false;
        };
        let rendered_line_count = terminal_output_line_count_for_scroll(view);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return false;
        }

        let max_scroll = rendered_line_count.saturating_sub(view.output_viewport_rows.max(1));
        let current = view.output_scroll_line.min(max_scroll);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(max_scroll)
        };
        view.output_scroll_line = next;
        view.output_follow_tail = next == max_scroll;
        next != current
    }

    fn open_inspector_for_selected(&mut self, inspector: ArtifactInspectorKind) {
        let ui_state = self.ui_state();
        if let Some(work_item_id) = ui_state.selected_work_item_id {
            if matches!(self.view_stack.active_center(), Some(CenterView::InspectorView { .. })) {
                let _ = self.view_stack.pop_center();
            }
            let _ = self.view_stack.push_center(CenterView::InspectorView {
                work_item_id,
                inspector,
            });
        }
    }

    fn open_chat_inspector_for_selected(&mut self) {
        self.open_inspector_for_selected(ArtifactInspectorKind::Chat);
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

        if !self.view_stack.pop_center() {
            self.view_stack.clear_center();
        }
        self.enter_normal_mode();

        if let Some(context) = self.global_supervisor_chat_return_context.take() {
            self.selected_inbox_index = context.selected_inbox_index;
            self.selected_inbox_item_id = context.selected_inbox_item_id;
        }
    }

    fn open_ticket_picker(&mut self) {
        self.enter_normal_mode();
        self.ticket_picker_overlay.open();
        self.status_warning = Some(
            "ticket operations are handled outside the TUI loop; picker is read-only here"
                .to_owned(),
        );
    }

    fn close_ticket_picker(&mut self) {
        self.ticket_picker_overlay.close();
        self.ticket_picker_create_refresh_deadline = None;
    }

    fn begin_create_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.begin_new_ticket_mode();
    }

    fn cancel_create_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.cancel_new_ticket_mode();
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
        self.status_warning = Some(
            "ticket start is handled outside the TUI loop in frontend services".to_owned(),
        );
    }

    fn start_selected_ticket_from_picker_with_override(
        &mut self,
        _ticket: TicketSummary,
        _repository_override: Option<PathBuf>,
    ) {
        self.status_warning = Some(
            "ticket start with repository override is handled outside the TUI loop".to_owned(),
        );
    }

    fn begin_archive_selected_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        if self.ticket_picker_overlay.loading
            || self.ticket_picker_overlay.new_ticket_mode
            || self.ticket_picker_overlay.starting_ticket_id.is_some()
            || self.ticket_picker_overlay.archiving_ticket_id.is_some()
        {
            return;
        }
        let Some(ticket) = self.ticket_picker_overlay.selected_ticket().cloned() else {
            return;
        };
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.archive_confirm_ticket = Some(ticket);
    }

    fn cancel_ticket_picker_archive_confirmation(&mut self) {
        self.ticket_picker_overlay.archive_confirm_ticket = None;
    }

    fn submit_ticket_picker_archive_confirmation(&mut self) {
        if self.ticket_picker_overlay.archive_confirm_ticket.is_none() {
            return;
        }
        self.ticket_picker_overlay.archive_confirm_ticket = None;
        self.status_warning = Some(
            "ticket archive is handled outside the TUI loop in frontend services".to_owned(),
        );
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
        let repository_path = self.ticket_picker_overlay.repository_prompt_input.text().trim();
        if repository_path.is_empty() {
            self.ticket_picker_overlay.error = Some("repository path cannot be empty".to_owned());
            return;
        }
        let Some(repository_path) = expand_tilde_path(repository_path) else {
            self.ticket_picker_overlay.error =
                Some("could not expand repository path: HOME is not set".to_owned());
            return;
        };
        self.start_selected_ticket_from_picker_with_override(ticket, Some(repository_path));
    }

    fn cancel_ticket_picker_repository_prompt(&mut self) {
        self.ticket_picker_overlay.cancel_repository_prompt();
    }

    fn append_repository_prompt_char(&mut self, ch: char) {
        self.ticket_picker_overlay.repository_prompt_input.insert_char(ch);
    }

    fn pop_repository_prompt_char(&mut self) {
        self.ticket_picker_overlay
            .repository_prompt_input
            .delete_char_backward();
    }

    fn submit_created_ticket_from_picker(&mut self, submit_mode: TicketCreateSubmitMode) {
        let _ = submit_mode;
        if !self.ticket_picker_overlay.visible || !self.ticket_picker_overlay.new_ticket_mode {
            return;
        }
        if !self.ticket_picker_overlay.can_submit_new_ticket() {
            self.ticket_picker_overlay.error =
                Some("enter a brief description before creating a ticket".to_owned());
            return;
        }

        let brief = self
            .ticket_picker_overlay
            .new_ticket_brief_editor
            .lines
            .to_string()
            .trim()
            .to_owned();
        let selected_project = self.ticket_picker_overlay.selected_project_name();
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.new_ticket_mode = false;
        clear_editor_state(&mut self.ticket_picker_overlay.new_ticket_brief_editor);
        self.ticket_picker_overlay.creating = false;
        self.ticket_picker_create_refresh_deadline = None;
        let _ = CreateTicketFromPickerRequest {
            brief,
            selected_project,
            submit_mode,
        };
        self.status_warning = Some(
            "ticket creation is handled outside the TUI loop in frontend services".to_owned(),
        );
    }

    fn filtered_ticket_picker_tickets(&self, tickets: Vec<TicketSummary>) -> Vec<TicketSummary> {
        let active_ticket_ids = self.active_ticket_ids_for_picker();
        tickets
            .into_iter()
            .filter(|ticket| !active_ticket_ids.iter().any(|id| id == &ticket.ticket_id))
            .collect()
    }

    fn active_ticket_ids_for_picker(&self) -> Vec<TicketId> {
        self.domain
            .work_items
            .values()
            .filter_map(|work_item| {
                let ticket_id = work_item.ticket_id.clone()?;
                let session_id = work_item.session_id.as_ref()?;
                let session = self.domain.sessions.get(session_id)?;
                match session.status {
                    Some(
                        WorkerSessionStatus::Running
                        | WorkerSessionStatus::WaitingForUser
                        | WorkerSessionStatus::Blocked,
                    ) => Some(ticket_id),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
    }

    #[cfg(test)]
    fn tick_terminal_view_and_report(&mut self) -> bool {
        let mut changed = self.drain_async_events_and_report();
        changed |= self.run_due_periodic_tasks_and_report(Instant::now());
        changed |= self.maintain_active_terminal_view_and_report();
        changed
    }

    fn tick_autopilot_and_report(&mut self) -> bool {
        if self.application_mode != ApplicationMode::Autopilot {
            return false;
        }

        let now = Instant::now();
        let mut changed = false;
        changed |= self.poll_terminal_session_events();
        changed |= self.flush_background_terminal_output_and_report_at(now);
        changed |= self.enqueue_progression_approval_reconcile_polls_at(now);
        changed |= self.poll_merge_queue_events();
        changed |= self.poll_session_info_summary_events();
        changed |= self.tick_session_info_summary_refresh_at(now);
        changed |= self.refresh_review_stage_tracking();
        changed |= self.ensure_session_info_diff_loaded_for_active_session();
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            changed |= self.flush_deferred_terminal_output_for_session(&session_id);
            changed |= self.ensure_terminal_stream_and_report(session_id);
        }
        let pending_needs_input_sessions = self
            .terminal_session_states
            .iter()
            .filter_map(|(session_id, view)| {
                if view.active_needs_input.is_some() {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for session_id in pending_needs_input_sessions {
            changed |= self.autopilot_handle_needs_input_for_session(&session_id);
        }
        changed |= self.autopilot_reconcile_session_actions();
        changed
    }

    fn autopilot_reconcile_session_actions(&mut self) -> bool {
        if self.application_mode != ApplicationMode::Autopilot {
            return false;
        }
        self.status_warning = Some(
            "autopilot workflow actions are handled outside the TUI loop".to_owned(),
        );
        false
    }

    fn autopilot_handle_needs_input_for_session(&mut self, session_id: &WorkerSessionId) -> bool {
        let should_submit = {
            let Some(prompt) = self
                .terminal_session_states
                .get_mut(session_id)
                .and_then(|view| view.active_needs_input.as_mut())
            else {
                return false;
            };
            Self::select_autopilot_answers_for_prompt(prompt)
        };

        if !should_submit {
            return false;
        }

        self.submit_terminal_needs_input_response_for_session(session_id)
    }

    fn flush_deferred_terminal_output_for_session(&mut self, session_id: &WorkerSessionId) -> bool {
        let Some(view) = self.terminal_session_states.get_mut(session_id) else {
            return false;
        };
        if view.deferred_output.is_empty() {
            return false;
        }
        let bytes = std::mem::take(&mut view.deferred_output);
        append_terminal_assistant_output(view, bytes, self.runtime_config.transcript_line_limit);
        view.last_background_flush_at = None;
        self.recompute_background_terminal_flush_deadline();
        true
    }

    #[cfg(test)]
    fn flush_background_terminal_output_and_report(&mut self) -> bool {
        self.flush_background_terminal_output_and_report_at(Instant::now())
    }

    fn flush_background_terminal_output_and_report_at(&mut self, now: Instant) -> bool {
        if self.background_terminal_flush_deadline.is_none() {
            self.recompute_background_terminal_flush_deadline();
        }
        if self
            .background_terminal_flush_deadline
            .map(|deadline| now < deadline)
            .unwrap_or(true)
        {
            return false;
        }
        let active_session_id = self.active_terminal_session_id().cloned();
        let interval = self.runtime_config.background_session_refresh_interval();
        let mut flushed_any = false;

        let session_ids = self
            .terminal_session_states
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for session_id in session_ids {
            if active_session_id.as_ref() == Some(&session_id) {
                continue;
            }
            let should_flush = self
                .terminal_session_states
                .get(&session_id)
                .map(|view| {
                    !view.deferred_output.is_empty()
                        && view
                            .last_background_flush_at
                            .map(|previous| now.duration_since(previous) >= interval)
                            .unwrap_or(true)
                })
                .unwrap_or(false);
            if !should_flush {
                continue;
            }

            let Some(view) = self.terminal_session_states.get_mut(&session_id) else {
                continue;
            };
            let bytes = std::mem::take(&mut view.deferred_output);
            append_terminal_assistant_output(
                view,
                bytes,
                self.runtime_config.transcript_line_limit,
            );
            view.last_background_flush_at = Some(now);
            flushed_any = true;
        }

        self.recompute_background_terminal_flush_deadline();
        flushed_any
    }

    fn recompute_background_terminal_flush_deadline(&mut self) {
        let now = Instant::now();
        let interval = self.runtime_config.background_session_refresh_interval();
        let active_session_id = self.active_terminal_session_id().cloned();
        self.background_terminal_flush_deadline = self
            .terminal_session_states
            .iter()
            .filter(|(session_id, view)| {
                active_session_id.as_ref() != Some(*session_id) && !view.deferred_output.is_empty()
            })
            .map(|(_, view)| {
                view.last_background_flush_at
                    .map(|previous| previous + interval)
                    .unwrap_or(now)
            })
            .min();
    }

    fn poll_terminal_session_events(&mut self) -> bool {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.terminal_session_receiver.as_mut() else {
                return false;
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

        let had_events = !events.is_empty();
        let mut changed = false;
        for event in events {
            match event {
                TerminalSessionEvent::Output { session_id, output } => {
                    let is_active_session = self.active_terminal_session_id() == Some(&session_id);
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    if is_active_session {
                        if !view.deferred_output.is_empty() {
                            let deferred = std::mem::take(&mut view.deferred_output);
                            append_terminal_assistant_output(
                                view,
                                deferred,
                                self.runtime_config.transcript_line_limit,
                            );
                            view.last_background_flush_at = Some(Instant::now());
                        }
                        append_terminal_assistant_output(
                            view,
                            output.bytes,
                            self.runtime_config.transcript_line_limit,
                        );
                    } else {
                        view.deferred_output.extend_from_slice(output.bytes.as_slice());
                        if view.deferred_output.len() >= BACKGROUND_SESSION_DEFERRED_OUTPUT_MAX_BYTES
                        {
                            let deferred = std::mem::take(&mut view.deferred_output);
                            append_terminal_assistant_output(
                                view,
                                deferred,
                                self.runtime_config.transcript_line_limit,
                            );
                            view.last_background_flush_at = Some(Instant::now());
                        } else if view.last_background_flush_at.is_none() {
                            view.last_background_flush_at = Some(Instant::now());
                        }
                    }
                    view.error = None;
                }
                TerminalSessionEvent::TurnState {
                    session_id,
                    turn_state,
                } => {
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    let previous_turn_active = view.turn_active;
                    view.turn_active = turn_state.active;
                    if previous_turn_active != turn_state.active {
                        self.enqueue_or_persist_session_working_state(
                            session_id.clone(),
                            turn_state.active,
                        );
                        if self.active_terminal_session_id() == Some(&session_id) {
                            self.schedule_session_info_summary_refresh_for_active_session();
                        }
                        changed |= self.mark_reconcile_dirty_for_session(&session_id);
                    }
                }
                TerminalSessionEvent::NeedsInput {
                    session_id,
                    needs_input,
                } => {
                    let needs_input_summary = Self::needs_input_summary(&needs_input);
                    if let Some((kind, coalesce_key, title_prefix)) =
                        self.route_needs_input_inbox_for_session(&session_id)
                    {
                        self.publish_inbox_for_session(
                            &session_id,
                            kind,
                            format!("{title_prefix}: {needs_input_summary}"),
                            coalesce_key,
                        );
                    }
                    let prompt = self.needs_input_prompt_from_event(&session_id, needs_input);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    view.enqueue_needs_input_prompt(prompt);
                    self.schedule_session_info_summary_refresh_for_active_session();
                    changed |= self.mark_reconcile_dirty_for_session(&session_id);
                }
                TerminalSessionEvent::StreamFailed { session_id, error } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    view.error = Some(error.to_string());
                    view.deferred_output.clear();
                    view.last_background_flush_at = None;
                    view.entries.clear();
                    view.transcript_truncated = false;
                    view.transcript_truncated_line_count = 0;
                    view.output_fragment.clear();
                    view.render_cache.invalidate_all();
                    view.output_rendered_line_count = 0;
                    view.output_scroll_line = 0;
                    view.output_follow_tail = true;
                    view.turn_active = false;
                    self.persist_session_working_state_immediately(session_id.clone(), false);
                    self.publish_error_for_session(
                        &session_id,
                        "terminal-stream",
                        error.to_string().as_str(),
                    );
                    changed |= self.mark_reconcile_dirty_for_session(&session_id);
                    if let RuntimeError::SessionNotFound(_) = error {
                        self.recover_terminal_session_on_not_found(&session_id);
                    }
                    if self.active_terminal_session_id() == Some(&session_id) {
                        self.schedule_session_info_summary_refresh_for_active_session();
                    }
                }
                TerminalSessionEvent::StreamEnded { session_id } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    if !view.deferred_output.is_empty() {
                        let deferred = std::mem::take(&mut view.deferred_output);
                        append_terminal_assistant_output(
                            view,
                            deferred,
                            self.runtime_config.transcript_line_limit,
                        );
                    }
                    view.last_background_flush_at = None;
                    flush_terminal_output_fragment(
                        view,
                        self.runtime_config.transcript_line_limit,
                    );
                    view.turn_active = false;
                    self.persist_session_working_state_immediately(session_id.clone(), false);
                    if self.active_terminal_session_id() == Some(&session_id) {
                        self.schedule_session_info_summary_refresh_for_active_session();
                    }
                    changed |= self.mark_reconcile_dirty_for_session(&session_id);
                }
            }
        }
        if had_events {
            self.recompute_background_terminal_flush_deadline();
        }
        had_events || changed
    }

    fn reset_merge_poll_backoff(&mut self, session_id: &WorkerSessionId, now: Instant) {
        let base = self.runtime_config.merge_poll_base_interval();
        let state = self
            .merge_poll_states
            .entry(session_id.clone())
            .or_insert_with(|| MergeReconcilePollState::new(now, base));
        if state.consecutive_failures > 0 {
            tracing::info!(
                session_id = session_id.as_str(),
                consecutive_failures = state.consecutive_failures,
                "merge reconcile polling recovered; resetting backoff"
            );
        }
        state.consecutive_failures = 0;
        state.backoff = base;
        state.next_poll_at = now + base;
        state.last_poll_started_at = Some(now);
        state.last_api_error_signature = None;
    }

    fn register_merge_poll_failure(
        &mut self,
        session_id: &WorkerSessionId,
        signature: &str,
        now: Instant,
    ) -> bool {
        let base = self.runtime_config.merge_poll_base_interval();
        let max_backoff = self.runtime_config.merge_poll_max_backoff();
        let multiplier = self.runtime_config.merge_poll_backoff_multiplier().max(1);
        let state = self
            .merge_poll_states
            .entry(session_id.clone())
            .or_insert_with(|| MergeReconcilePollState::new(now, base));
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let mut next_secs = state.backoff.as_secs().max(base.as_secs());
        next_secs = next_secs.saturating_mul(multiplier).max(base.as_secs());
        let max_secs = max_backoff.as_secs().max(base.as_secs());
        state.backoff = Duration::from_secs(next_secs.min(max_secs));
        state.next_poll_at = now + state.backoff;
        state.last_poll_started_at = Some(now);
        let should_notify = state.last_api_error_signature.as_deref() != Some(signature);
        if should_notify {
            state.last_api_error_signature = Some(signature.to_owned());
        }
        tracing::warn!(
            session_id = session_id.as_str(),
            consecutive_failures = state.consecutive_failures,
            backoff_secs = state.backoff.as_secs(),
            "merge reconcile polling failed; scheduling retry with backoff"
        );
        should_notify
    }

    fn poll_merge_queue_events(&mut self) -> bool {
        let mut events = Vec::new();
        {
            let Some(receiver) = self.merge_event_receiver.as_mut() else {
                return false;
            };
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status_warning =
                            Some("merge queue channel closed unexpectedly".to_owned());
                        break;
                    }
                }
            }
        }

        let had_events = !events.is_empty();
        for event in events {
            match event {
                MergeQueueEvent::Completed {
                    session_id,
                    kind,
                    completed,
                    merge_conflict,
                    base_branch,
                    head_branch,
                    ci_checks,
                    ci_failures,
                    ci_has_failures,
                    ci_status_error,
                    error,
                } => {
                    self.session_ci_status_cache.insert(
                        session_id.clone(),
                        SessionCiStatusCache {
                            checks: ci_checks.clone(),
                            error: ci_status_error.clone(),
                        },
                    );

                    let mut failure_labels = if ci_failures.is_empty() {
                        ci_checks
                            .iter()
                            .filter(|check| check.bucket.eq_ignore_ascii_case("fail"))
                            .map(|check| {
                                let workflow_prefix = check
                                    .workflow
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|workflow| !workflow.is_empty())
                                    .map(|workflow| format!("{workflow} / "))
                                    .unwrap_or_default();
                                format!("{workflow_prefix}{}", check.name)
                            })
                            .collect::<Vec<_>>()
                    } else {
                        ci_failures
                            .iter()
                            .map(|entry| entry.trim())
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    };
                    failure_labels.sort();
                    failure_labels.dedup();

                    if ci_has_failures && !failure_labels.is_empty() {
                        let signature = failure_labels.join("|");
                        let should_notify = self
                            .ci_failure_signatures_notified
                            .get(&session_id)
                            .map(|existing| existing != &signature)
                            .unwrap_or(true);
                        if should_notify && self.session_is_in_review_stage(&session_id) {
                            let failed = failure_labels.join(", ");
                            let instruction = format!(
                                "CI pipeline failure detected while this ticket is in review ({failed}). Investigate failing GitHub Actions checks, implement a fix, push updates, and report what changed. Use CI as the source of truth instead of running the full local build/test suite."
                            );
                            self.send_terminal_instruction_to_session(
                                &session_id,
                                instruction.as_str(),
                            );
                            let labels = session_display_labels(&self.domain, &session_id);
                            self.publish_inbox_for_session(
                                &session_id,
                                InboxItemKind::FYI,
                                format!(
                                    "CI checks failed for {}; harness is fixing: {}",
                                    labels.compact_label, failed
                                ),
                                "ci-pipeline-failure",
                            );
                            self.status_warning = Some(format!(
                                "ci checks failing for {}; harness is fixing",
                                labels.compact_label
                            ));
                        }
                        self.ci_failure_signatures_notified
                            .insert(session_id.clone(), signature);
                    } else {
                        self.ci_failure_signatures_notified.remove(&session_id);
                    }

                    if let Some(error) = error {
                        let signature =
                            format!("merge-queue:{}", sanitize_terminal_display_text(error.as_str()));
                        let should_notify = self.register_merge_poll_failure(
                            &session_id,
                            signature.as_str(),
                            Instant::now(),
                        );
                        if should_notify {
                            self.publish_error_for_session(&session_id, "merge-queue", error.as_str());
                            self.status_warning = Some(format!(
                                "workflow merge check failed: {}",
                                sanitize_terminal_display_text(error.as_str())
                            ));
                        }
                        continue;
                    }

                    self.clear_merge_status_warning_for_session(&session_id);
                    if kind == MergeQueueCommandKind::Merge || !completed {
                        self.merge_pending_sessions.insert(session_id.clone());
                    }

                    if merge_conflict {
                        let base = base_branch.unwrap_or_else(|| "main".to_owned());
                        let head =
                            head_branch.unwrap_or_else(|| "current feature branch".to_owned());
                        let signature = format!("{head}->{base}");
                        let should_notify = {
                            let view = self
                                .terminal_session_states
                                .entry(session_id.clone())
                                .or_default();
                            if view.last_merge_conflict_signature.as_deref()
                                == Some(signature.as_str())
                            {
                                false
                            } else {
                                view.last_merge_conflict_signature = Some(signature.clone());
                                true
                            }
                        };

                        if should_notify {
                            let instruction = format!(
                                "Merge conflict detected on PR branch '{head}' into '{base}'. Resolve the conflict now: update your branch against '{base}', fix conflicts, push the branch, and report what changed."
                            );
                            self.send_terminal_instruction_to_session(
                                &session_id,
                                instruction.as_str(),
                            );
                        }
                        let labels = session_display_labels(&self.domain, &session_id);
                        self.status_warning = Some(format!(
                            "merge conflict for review {}: resolve conflicts and push updates",
                            labels.compact_label
                        ));
                    } else if let Some(view) = self.terminal_session_states.get_mut(&session_id) {
                        view.last_merge_conflict_signature = None;
                    }

                    if let Some(message) = ci_status_error
                        .as_deref()
                        .map(str::trim)
                        .filter(|entry| !entry.is_empty())
                    {
                        let signature = format!("ci-status:{message}");
                        let should_notify = self.register_merge_poll_failure(
                            &session_id,
                            signature.as_str(),
                            Instant::now(),
                        );
                        if should_notify {
                            self.publish_error_for_session(
                                &session_id,
                                "merge-reconcile-api-error",
                                message,
                            );
                            let labels = session_display_labels(&self.domain, &session_id);
                            self.status_warning = Some(format!(
                                "merge reconcile status unavailable for {}: {}",
                                labels.compact_label,
                                compact_focus_card_text(message)
                            ));
                        }
                    } else {
                        self.reset_merge_poll_backoff(&session_id, Instant::now());
                    }

                    if completed {
                        self.publish_inbox_for_session(
                            &session_id,
                            InboxItemKind::FYI,
                            format!(
                                "Ticket merge completed for {}",
                                session_display_labels(&self.domain, &session_id).compact_label
                            ),
                            "merge-completed",
                        );
                        self.merge_pending_sessions.remove(&session_id);
                        self.merge_poll_states.remove(&session_id);
                        if let Some(session) = self.domain.sessions.get_mut(&session_id) {
                            session.status = Some(WorkerSessionStatus::Done);
                        }
                        if let Some(work_item_id) = self
                            .domain
                            .sessions
                            .get(&session_id)
                            .and_then(|session| session.work_item_id.clone())
                        {
                            if let Some(work_item) = self.domain.work_items.get_mut(&work_item_id) {
                                work_item.workflow_state = Some(WorkflowState::Done);
                            }
                        }
                        self.refresh_reconcile_eligibility_for_session(&session_id);
                        let labels = session_display_labels(&self.domain, &session_id);
                        self.status_warning = Some(format!(
                            "merge completed for review {}",
                            labels.compact_label
                        ));
                        self.spawn_session_merge_finalize(session_id.clone());
                    } else if kind == MergeQueueCommandKind::Merge {
                        let labels = session_display_labels(&self.domain, &session_id);
                        self.status_warning = Some(format!(
                            "merge pending for review {} (waiting for checks or merge queue)",
                            labels.compact_label
                        ));
                    }
                    let _ = self.mark_reconcile_dirty_for_session(&session_id);
                }
                MergeQueueEvent::SessionFinalized {
                    session_id,
                    event,
                } => {
                    self.apply_domain_event(event);
                    self.merge_finalizing_sessions.remove(&session_id);
                    let _ = self.mark_reconcile_dirty_for_session(&session_id);
                }
                MergeQueueEvent::SessionFinalizeFailed {
                    session_id,
                    message,
                } => {
                    self.merge_finalizing_sessions.remove(&session_id);
                    self.publish_error_for_session(&session_id, "merge-finalize", message.as_str());
                    let labels = session_display_labels(&self.domain, &session_id);
                    self.status_warning = Some(format!(
                        "merged {} finalized with warnings: {}",
                        labels.compact_label,
                        compact_focus_card_text(message.as_str())
                    ));
                    let _ = self.mark_reconcile_dirty_for_session(&session_id);
                }
            }
        }
        had_events
    }

    fn refresh_review_stage_tracking(&mut self) -> bool {
        let mut changed = false;
        let session_ids = self
            .domain
            .sessions
            .iter()
            .filter_map(|(session_id, session)| {
                if !is_open_session_status(session.status.as_ref()) {
                    return None;
                }
                if self.session_is_in_review_stage(session_id) {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let active_review_sessions = session_ids.iter().cloned().collect::<HashSet<_>>();
        let review_len_before = self.review_sync_instructions_sent.len();
        self.review_sync_instructions_sent
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.review_sync_instructions_sent.len() != review_len_before;
        let pending_len_before = self.merge_pending_sessions.len();
        self.merge_pending_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.merge_pending_sessions.len() != pending_len_before;
        let finalizing_len_before = self.merge_finalizing_sessions.len();
        self.merge_finalizing_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.merge_finalizing_sessions.len() != finalizing_len_before;
        let ci_cache_len_before = self.session_ci_status_cache.len();
        self.session_ci_status_cache
            .retain(|session_id, _| active_review_sessions.contains(session_id));
        changed |= self.session_ci_status_cache.len() != ci_cache_len_before;
        let ci_notified_len_before = self.ci_failure_signatures_notified.len();
        self.ci_failure_signatures_notified
            .retain(|session_id, _| active_review_sessions.contains(session_id));
        changed |= self.ci_failure_signatures_notified.len() != ci_notified_len_before;

        for session_id in session_ids {
            changed |= self.ensure_review_sync_instruction(&session_id);
        }
        changed
    }

    fn mark_all_eligible_reconcile_dirty(&mut self) {
        self.dirty_approval_reconcile_sessions
            .extend(self.approval_reconcile_candidate_sessions.iter().cloned());
        self.dirty_review_reconcile_sessions
            .extend(self.review_reconcile_eligible_sessions.iter().cloned());
    }

    fn refresh_reconcile_eligibility_for_all_sessions(&mut self) -> bool {
        let open_sessions = self
            .domain
            .sessions
            .iter()
            .filter_map(|(session_id, session)| {
                if is_open_session_status(session.status.as_ref()) {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();

        let mut changed = false;
        let stale_review = self
            .review_reconcile_eligible_sessions
            .iter()
            .filter(|session_id| !open_sessions.contains(*session_id))
            .cloned()
            .collect::<Vec<_>>();
        for session_id in stale_review {
            changed |= self.set_review_eligible(session_id, false);
        }
        let stale_approval = self
            .approval_reconcile_candidate_sessions
            .iter()
            .filter(|session_id| !open_sessions.contains(*session_id))
            .cloned()
            .collect::<Vec<_>>();
        for session_id in stale_approval {
            changed |= self.set_approval_eligible(session_id, false);
        }

        for session_id in open_sessions {
            changed |= self.refresh_reconcile_eligibility_for_session(&session_id);
        }
        changed
    }

    fn enqueue_merge_queue_request(
        &mut self,
        session_id: WorkerSessionId,
        kind: MergeQueueCommandKind,
    ) -> bool {
        let Some(context) = self.supervisor_context_for_session(&session_id) else {
            return false;
        };
        if self
            .merge_queue
            .iter()
            .any(|queued| queued.session_id == session_id && queued.kind == kind)
        {
            return false;
        }
        let request = MergeQueueRequest {
            session_id,
            _context: context,
            kind,
        };
        if kind == MergeQueueCommandKind::Merge {
            self.merge_queue.push_front(request);
        } else {
            self.merge_queue.push_back(request);
        }
        true
    }

    fn dispatch_merge_queue_requests_at(&mut self, _now: Instant) -> bool {
        false
    }

    fn refresh_reconcile_eligibility_for_session(&mut self, session_id: &WorkerSessionId) -> bool {
        let session_is_open = self
            .domain
            .sessions
            .get(session_id)
            .map(|session| is_open_session_status(session.status.as_ref()))
            .unwrap_or(false);
        if !session_is_open {
            let mut changed = false;
            changed |= self.set_approval_eligible(session_id.clone(), false);
            changed |= self.set_review_eligible(session_id.clone(), false);
            return changed;
        }

        let is_approval_eligible = matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning | WorkflowState::Implementing)
        );
        let is_review_eligible = self.session_is_in_review_stage(session_id);

        let mut changed = false;
        changed |= self.set_approval_eligible(session_id.clone(), is_approval_eligible);
        changed |= self.set_review_eligible(session_id.clone(), is_review_eligible);
        changed
    }

    fn set_approval_eligible(&mut self, session_id: WorkerSessionId, eligible: bool) -> bool {
        if eligible {
            self.approval_reconcile_candidate_sessions.insert(session_id)
        } else {
            let mut changed = self.approval_reconcile_candidate_sessions.remove(&session_id);
            changed |= self.dirty_approval_reconcile_sessions.remove(&session_id);
            changed
        }
    }

    fn set_review_eligible(&mut self, session_id: WorkerSessionId, eligible: bool) -> bool {
        if eligible {
            self.review_reconcile_eligible_sessions.insert(session_id)
        } else {
            let mut changed = self.review_reconcile_eligible_sessions.remove(&session_id);
            changed |= self.dirty_review_reconcile_sessions.remove(&session_id);
            changed |= self.review_sync_instructions_sent.remove(&session_id);
            changed |= self.merge_pending_sessions.remove(&session_id);
            changed |= self.merge_poll_states.remove(&session_id).is_some();
            changed |= self.merge_finalizing_sessions.remove(&session_id);
            changed |= self.session_ci_status_cache.remove(&session_id).is_some();
            changed |= self.ci_failure_signatures_notified.remove(&session_id).is_some();
            if let Some(view) = self.terminal_session_states.get_mut(&session_id) {
                if view.last_merge_conflict_signature.take().is_some() {
                    changed = true;
                }
            }
            changed
        }
    }
    fn mark_reconcile_dirty_for_session(&mut self, session_id: &WorkerSessionId) -> bool {
        self.refresh_reconcile_eligibility_for_session(session_id);
        let mut changed = false;
        if self.approval_reconcile_candidate_sessions.contains(session_id) {
            changed |= self
                .dirty_approval_reconcile_sessions
                .insert(session_id.clone());
        }
        if self.review_reconcile_eligible_sessions.contains(session_id) {
            changed |= self.dirty_review_reconcile_sessions.insert(session_id.clone());
        }
        changed
    }

    fn enqueue_merge_reconcile_polls_at(&mut self, _now: Instant) -> bool {
        self.enqueue_event_driven_reconciles()
    }

    fn enqueue_progression_approval_reconcile_polls_at(&mut self, _now: Instant) -> bool {
        self.enqueue_event_driven_reconciles()
    }

    fn enqueue_event_driven_reconciles(&mut self) -> bool {
        let mut changed = false;

        let approval_dirty = self
            .dirty_approval_reconcile_sessions
            .drain()
            .collect::<Vec<_>>();
        for session_id in approval_dirty {
            if self.approval_reconcile_candidate_sessions.contains(&session_id) {
                changed |= self.reconcile_progression_approval_inbox_for_session(&session_id);
            }
        }

        if self.supervisor_command_dispatcher.is_none() {
            self.dirty_review_reconcile_sessions.clear();
            return changed;
        }

        let active_review_sessions = self
            .review_reconcile_eligible_sessions
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let pending_len_before = self.merge_pending_sessions.len();
        self.merge_pending_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.merge_pending_sessions.len() != pending_len_before;
        let poll_len_before = self.merge_poll_states.len();
        self.merge_poll_states
            .retain(|session_id, _| self.merge_pending_sessions.contains(session_id));
        changed |= self.merge_poll_states.len() != poll_len_before;
        let finalizing_len_before = self.merge_finalizing_sessions.len();
        self.merge_finalizing_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.merge_finalizing_sessions.len() != finalizing_len_before;
        let ci_cache_len_before = self.session_ci_status_cache.len();
        self.session_ci_status_cache
            .retain(|session_id, _| active_review_sessions.contains(session_id));
        changed |= self.session_ci_status_cache.len() != ci_cache_len_before;
        let ci_notified_len_before = self.ci_failure_signatures_notified.len();
        self.ci_failure_signatures_notified
            .retain(|session_id, _| active_review_sessions.contains(session_id));
        changed |= self.ci_failure_signatures_notified.len() != ci_notified_len_before;
        let queue_len_before = self.merge_queue.len();
        self.merge_queue.retain(|request| {
            request.kind == MergeQueueCommandKind::Merge
                && active_review_sessions.contains(&request.session_id)
        });
        changed |= self.merge_queue.len() != queue_len_before;

        let review_dirty = self.dirty_review_reconcile_sessions.drain().collect::<Vec<_>>();
        for session_id in review_dirty {
            if !self.review_reconcile_eligible_sessions.contains(&session_id) {
                continue;
            }
            changed |= self.ensure_review_sync_instruction(&session_id);
        }
        changed
    }

    fn run_sparse_reconcile_fallbacks(&mut self) -> bool {
        false
    }

    fn ensure_review_sync_instruction(&mut self, session_id: &WorkerSessionId) -> bool {
        if self.review_sync_instructions_sent.contains(session_id) {
            return false;
        }
        self.send_terminal_instruction_to_session(
            session_id,
            "Review-stage sync directive: keep this worktree synced with remote and the PR base branch. Fetch regularly, rebase/merge as needed, and resolve conflicts promptly while merge checks run automatically.",
        );
        self.review_sync_instructions_sent
            .insert(session_id.clone());
        true
    }

    fn session_is_in_review_stage(&self, session_id: &WorkerSessionId) -> bool {
        self.session_is_merge_reconcile_eligible(session_id)
    }

    fn session_is_merge_reconcile_eligible(&self, session_id: &WorkerSessionId) -> bool {
        if !is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) {
            return false;
        }
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.as_ref())
            .map(|state| {
                matches!(
                    state,
                    WorkflowState::AwaitingYourReview
                        | WorkflowState::ReadyForReview
                        | WorkflowState::InReview
                        | WorkflowState::PendingMerge
                )
            })
            .unwrap_or(false)
    }

    fn supervisor_context_for_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<SupervisorCommandContext> {
        let session = self.domain.sessions.get(session_id)?;
        let work_item_id = session
            .work_item_id
            .as_ref()
            .map(|id| id.as_str().to_owned());
        Some(SupervisorCommandContext {
            selected_work_item_id: work_item_id,
            selected_session_id: Some(session_id.as_str().to_owned()),
            scope: Some(format!("session:{}", session_id.as_str())),
        })
    }

    fn rebuild_reconcile_eligibility_indexes(&mut self) {
        self.review_reconcile_eligible_sessions.clear();
        self.approval_reconcile_candidate_sessions.clear();

        let session_ids = self.domain.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            self.refresh_reconcile_eligibility_for_session(&session_id);
        }
    }

    fn apply_domain_event(&mut self, event: StoredEventEnvelope) {
        self.incremental_domain_event_applies =
            self.incremental_domain_event_applies.saturating_add(1);
        let event_session_id = event.session_id.clone();
        let event_work_item_id = event.work_item_id.clone();
        apply_event(&mut self.domain, event);
        if let Some(session_id) = event_session_id {
            self.refresh_reconcile_eligibility_for_session(&session_id);
        } else if let Some(work_item_id) = event_work_item_id {
            if let Some(session_id) = self
                .domain
                .work_items
                .get(&work_item_id)
                .and_then(|work_item| work_item.session_id.clone())
            {
                self.refresh_reconcile_eligibility_for_session(&session_id);
            }
        }
    }

    fn replace_domain_projection(&mut self, projection: ProjectionState, reason: &'static str) {
        self.full_projection_replacements = self.full_projection_replacements.saturating_add(1);
        self.domain = projection;
        self.rebuild_reconcile_eligibility_indexes();
        tracing::debug!(
            reason,
            full_projection_replacements = self.full_projection_replacements,
            incremental_domain_event_applies = self.incremental_domain_event_applies,
            attention_projection_recomputes = self.attention_projection_recomputes,
            "ui full projection replacement applied"
        );
    }

    fn maybe_emit_projection_perf_log(&mut self, now: Instant) {
        if self
            .last_projection_perf_log_at
            .map(|previous| now.duration_since(previous) < PROJECTION_PERF_LOG_INTERVAL)
            .unwrap_or(false)
        {
            return;
        }
        self.last_projection_perf_log_at = Some(now);
        tracing::debug!(
            full_projection_replacements = self.full_projection_replacements,
            incremental_domain_event_applies = self.incremental_domain_event_applies,
            attention_projection_recomputes = self.attention_projection_recomputes,
            domain_event_count = self.domain.events.len(),
            "ui projection perf counters"
        );
    }
    fn session_requires_manual_needs_input_activation(&self, session_id: &WorkerSessionId) -> bool {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.as_ref())
            .map(|state| matches!(state, WorkflowState::New | WorkflowState::Planning))
            .unwrap_or(false)
    }

    fn send_terminal_instruction_to_session(
        &mut self,
        session_id: &WorkerSessionId,
        instruction: &str,
    ) {
        if instruction.trim().is_empty() {
            return;
        }
        if let Some(view) = self.terminal_session_states.get_mut(session_id) {
            append_terminal_user_message(
                view,
                instruction,
                self.runtime_config.transcript_line_limit,
            );
            view.error = None;
        }

        if let Some(controller) = self.frontend_controller.clone() {
            match TokioHandle::try_current() {
                Ok(runtime) => {
                    let target_session_id = session_id.clone();
                    let payload = instruction.to_owned();
                    runtime.spawn(async move {
                        let _ = controller
                            .submit_intent(FrontendIntent::SendTerminalInput {
                                session_id: target_session_id,
                                input: payload,
                            })
                            .await;
                    });
                }
                Err(_) => {
                    self.status_warning = Some(
                        "terminal instruction unavailable: tokio runtime is not active".to_owned(),
                    );
                }
            }
            return;
        }

        self.status_warning =
            Some("terminal instruction unavailable: frontend controller is not configured".to_owned());
    }

    fn clear_merge_status_warning_for_session(&mut self, session_id: &WorkerSessionId) {
        let Some(warning) = self.status_warning.as_deref() else {
            return;
        };
        let labels = session_display_labels(&self.domain, session_id);
        let fallback = format!("session {}", session_id.as_str());
        if !warning.contains(labels.compact_label.as_str()) && !warning.contains(fallback.as_str()) {
            return;
        }
        let is_merge_status = warning.starts_with("merge queued for review")
            || warning.starts_with("merge pending for review")
            || warning.starts_with("merge completed for review")
            || warning.starts_with("merge conflict for review")
            || warning.starts_with("ci checks failing for");
        if is_merge_status {
            self.status_warning = None;
        }
    }

    fn start_pr_pipeline_polling(&mut self, sender: Option<mpsc::Sender<MergeQueueEvent>>) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };
        let Some(sender) = sender else {
            return;
        };
        match TokioHandle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let _ = run_start_pr_pipeline_polling_task(provider, sender).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "workflow polling unavailable: tokio runtime unavailable".to_owned(),
                );
            }
        }
    }

    fn ensure_terminal_stream_and_report(&mut self, session_id: WorkerSessionId) -> bool {
        let _ = session_id;
        false
    }

    fn recover_terminal_session_on_not_found(&mut self, session_id: &WorkerSessionId) {
        if self.terminal_session_event_is_stale(session_id) {
            return;
        }
        let labels = session_display_labels(&self.domain, session_id);
        self.status_warning = Some(format!(
            "terminal {} was not found; session recovery is managed outside the UI loop",
            labels.compact_label
        ));
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

    fn poll_ticket_picker_events(&mut self) -> bool {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.ticket_picker_receiver.as_mut() else {
                return false;
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

        let had_events = !events.is_empty();
        for event in events {
            self.apply_ticket_picker_event(event);
        }
        had_events
    }

    fn apply_ticket_picker_event(&mut self, event: TicketPickerEvent) {
        match event {
            TicketPickerEvent::TicketsLoaded { tickets, projects } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = None;
                let tickets = self.filtered_ticket_picker_tickets(tickets);
                self.ticket_picker_overlay
                    .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
            }
            TicketPickerEvent::TicketsLoadFailed { message } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = Some(message.clone());
                self.status_warning = Some(format!(
                    "ticket picker load warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::SessionWorkflowAdvanced {
                outcome,
            } => {
                self.apply_domain_event(outcome.event.clone());
                self.autopilot_advancing_sessions.remove(&outcome.session_id);

                if let Some(instruction) = outcome.instruction.as_deref() {
                    self.send_terminal_instruction_to_session(&outcome.session_id, instruction);
                }
                if let Some((kind, coalesce_key, title_prefix)) =
                    Self::workflow_transition_inbox_for_state(&outcome.to)
                {
                    self.publish_inbox_for_session(
                        &outcome.session_id,
                        kind,
                        format!(
                            "{title_prefix}: {}",
                            session_display_labels(&self.domain, &outcome.session_id)
                                .compact_label
                        ),
                        coalesce_key,
                    );
                }
                let _ = self.mark_reconcile_dirty_for_session(&outcome.session_id);
                self.auto_advance_inbox_selection_after_workflow_progression(&outcome.session_id);

                let labels = session_display_labels(&self.domain, &outcome.session_id);
                self.status_warning = Some(format!(
                    "workflow advanced for {}: {:?} -> {:?}",
                    labels.compact_label,
                    outcome.from,
                    outcome.to
                ));
                self.session_info_diff_cache.remove(&outcome.session_id);
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::SessionWorkflowAdvanceFailed {
                session_id,
                message,
            } => {
                self.autopilot_advancing_sessions.remove(&session_id);
                self.publish_error_for_session(&session_id, "workflow-advance", message.as_str());
                let labels = session_display_labels(&self.domain, &session_id);
                self.status_warning = Some(format!(
                    "workflow advance warning for {}: {}",
                    labels.compact_label,
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::TicketStarted {
                started_session_id,
                projection,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.cancel_repository_prompt();
                self.ticket_picker_overlay.error = None;
                if let Some(projection) = projection {
                    // Full replacement remains only for ticket-start authoritative reloads.
                    self.replace_domain_projection(projection, "ticket-start");
                }
                if let Some(tickets) = tickets {
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                self.focus_and_stream_session(started_session_id);
                if let Some(message) = warning {
                    self.status_warning = Some(format!(
                        "ticket picker start warning: {}",
                        compact_focus_card_text(message.as_str())
                    ));
                }
                self.schedule_session_info_summary_refresh_for_active_session();
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
            TicketPickerEvent::TicketArchived {
                archived_ticket,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.archiving_ticket_id = None;
                self.ticket_picker_overlay.archive_confirm_ticket = None;
                self.ticket_picker_overlay.error = None;
                if let Some(tickets) = tickets {
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                let mut status = format!("archived {}", archived_ticket.identifier);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
            }
            TicketPickerEvent::TicketArchiveFailed {
                ticket,
                message,
                tickets,
            } => {
                self.ticket_picker_overlay.archiving_ticket_id = None;
                self.ticket_picker_overlay.archive_confirm_ticket = None;
                self.ticket_picker_overlay.error = Some(message.clone());
                if let Some(tickets) = tickets {
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                self.status_warning = Some(format!(
                    "ticket picker archive warning ({}): {}",
                    ticket.identifier,
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::TicketCreated {
                created_ticket,
                submit_mode,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_create_refresh_deadline = None;
                self.ticket_picker_overlay.error = None;
                let mut display_tickets = tickets
                    .unwrap_or_else(|| self.ticket_picker_overlay.tickets_snapshot());
                display_tickets = self.filtered_ticket_picker_tickets(display_tickets);
                if !display_tickets
                    .iter()
                    .any(|ticket| ticket.ticket_id == created_ticket.ticket_id)
                {
                    display_tickets.push(created_ticket.clone());
                }
                let projects = self.ticket_picker_overlay.project_names();
                self.ticket_picker_overlay.apply_tickets_preferring(
                    display_tickets,
                    projects,
                    &self.ticket_picker_priority_states,
                    Some(&created_ticket.ticket_id),
                );

                let mut status = format!("created {}", created_ticket.identifier);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
                if submit_mode == TicketCreateSubmitMode::CreateAndStart {
                    self.start_selected_ticket_from_picker_with_override(created_ticket, None);
                }
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::TicketCreateFailed {
                message,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_create_refresh_deadline = None;
                self.ticket_picker_overlay.error = Some(message.clone());
                if let Some(tickets) = tickets {
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                let mut warning_parts = vec![compact_focus_card_text(message.as_str())];
                if let Some(extra) = warning {
                    warning_parts.push(compact_focus_card_text(extra.as_str()));
                }
                self.status_warning = Some(format!(
                    "ticket picker create warning: {}",
                    warning_parts.join("; ")
                ));
            }
            TicketPickerEvent::SessionDiffLoaded { diff } => {
                let cache = self
                    .session_info_diff_cache
                    .entry(diff.session_id.clone())
                    .or_default();
                cache.loading = false;
                cache.error = None;
                cache.content = diff.diff.clone();
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    if modal.session_id == diff.session_id {
                        modal.loading = false;
                        modal.error = None;
                        modal.base_branch = diff.base_branch;
                        modal.content = diff.diff;
                        modal.selected_file_index = 0;
                        modal.selected_hunk_index = 0;
                        let files = parse_diff_file_summaries(modal.content.as_str());
                        modal.cursor_line =
                            files.first().map(|file| file.start_index).unwrap_or(0);
                            modal.scroll =
                            modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
                    }
                }
                if self.active_terminal_session_id() == Some(&diff.session_id) {
                    self.schedule_session_info_summary_refresh_for_active_session();
                }
            }
            TicketPickerEvent::SessionDiffFailed {
                session_id,
                message,
            } => {
                let cache = self
                    .session_info_diff_cache
                    .entry(session_id.clone())
                    .or_default();
                cache.loading = false;
                cache.error = Some(message.clone());
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    if modal.session_id == session_id {
                        modal.loading = false;
                        modal.error = Some(message.clone());
                    }
                }
                self.publish_error_for_session(&session_id, "diff-load", message.as_str());
                self.status_warning = Some(format!(
                    "diff load warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
                if self.active_terminal_session_id() == Some(&session_id) {
                    self.schedule_session_info_summary_refresh_for_active_session();
                }
            }
            TicketPickerEvent::SessionArchived {
                session_id,
                warning,
                event,
            } => {
                self.autopilot_archiving_sessions.remove(&session_id);
                self.archiving_session_id = None;
                self.archive_session_confirm_session = None;
                self.apply_domain_event(event);
                self.terminal_session_states.remove(&session_id);
                self.session_info_diff_cache.remove(&session_id);
                self.session_info_summary_cache.remove(&session_id);
                self.session_info_diff_last_refresh_at.remove(&session_id);
                self.session_info_summary_last_refresh_at.remove(&session_id);
                self.terminal_session_streamed.remove(&session_id);
                self.merge_pending_sessions.remove(&session_id);
                self.merge_poll_states.remove(&session_id);
                self.merge_finalizing_sessions.remove(&session_id);
                self.review_sync_instructions_sent.remove(&session_id);
                if self.active_terminal_session_id() == Some(&session_id) {
                    let _ = self.view_stack.pop_center();
                }
                let labels = session_display_labels(&self.domain, &session_id);
                let mut status = format!("archived {}", labels.compact_label);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::SessionArchiveFailed {
                session_id,
                message,
            } => {
                self.autopilot_archiving_sessions.remove(&session_id);
                self.archiving_session_id = None;
                self.archive_session_confirm_session = None;
                self.publish_error_for_session(&session_id, "session-archive", message.as_str());
                let labels = session_display_labels(&self.domain, &session_id);
                self.status_warning = Some(format!(
                    "session archive warning ({}): {}",
                    labels.compact_label,
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::InboxItemPublished { event } => {
                self.apply_domain_event(event);
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::InboxItemPublishFailed { message } => {
                self.status_warning = Some(format!(
                    "inbox publish warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::InboxItemResolved { event } => {
                if let Some(event) = event {
                    self.apply_domain_event(event);
                }
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::InboxItemResolveFailed { message } => {
                self.status_warning = Some(format!(
                    "inbox resolve warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
        }
    }

    fn is_ticket_picker_visible(&self) -> bool {
        self.ticket_picker_overlay.visible
    }

    fn submit_global_supervisor_chat_query(&mut self) {
        let query = self.global_supervisor_chat_input.text().trim().to_owned();
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
        self.global_supervisor_chat_input.clear();
        self.global_supervisor_chat_last_query = Some(query);
        self.status_warning = Some(
            "supervisor queries are handled outside the TUI loop via frontend services".to_owned(),
        );
        self.set_supervisor_terminal_state(
            SupervisorStreamTarget::GlobalChatPanel,
            SupervisorResponseState::NoContext,
            "query submission moved to frontend services",
        );
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
                self.global_supervisor_chat_input.delete_char_backward();
                true
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.global_supervisor_chat_input.insert_char(ch);
                true
            }
            _ => false,
        }
    }

    fn apply_terminal_compose_key(&mut self, key: KeyEvent) -> bool {
        if self.mode != UiMode::Terminal || !self.is_terminal_view_active() {
            return false;
        }
        if self.terminal_session_has_any_needs_input() {
            return false;
        }
        if is_ctrl_char(key, '\\') || is_ctrl_char(key, 'n') {
            return false;
        }

        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.enter_normal_mode();
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.submit_terminal_compose_input();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::CONTROL => {
                self.submit_terminal_compose_input();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                let enter = edtui_key_input(KeyCode::Enter, KeyModifiers::NONE)
                    .expect("enter key conversion");
                self.terminal_compose_event_handler
                    .on_key_event(enter, &mut self.terminal_compose_editor);
                true
            }
            _ => {
                if let Some(key_input) = map_edtui_key_input(key) {
                    self.terminal_compose_event_handler
                        .on_key_event(key_input, &mut self.terminal_compose_editor);
                    true
                } else {
                    false
                }
            }
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

        let request = build_supervisor_chat_request(
            &selected_row,
            &self.domain,
            self.runtime_config.supervisor_model.as_str(),
        );
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

    #[cfg(test)]
    fn tick_supervisor_stream(&mut self) {
        let _ = self.tick_supervisor_stream_and_report();
    }

    fn tick_supervisor_stream_and_report(&mut self) -> bool {
        let mut changed = self.poll_supervisor_stream_events();
        if let Some(stream) = self.supervisor_chat_stream.as_mut() {
            changed |= stream.flush_pending_delta();
        }
        changed
    }

    fn poll_supervisor_stream_events(&mut self) -> bool {
        let mut cancel_stream_id: Option<String> = None;
        let mut warning_message: Option<String> = None;
        let mut changed = false;

        {
            let Some(stream) = self.supervisor_chat_stream.as_mut() else {
                return false;
            };

            loop {
                match stream.receiver.try_recv() {
                    Ok(SupervisorStreamEvent::Started { stream_id }) => {
                        changed = true;
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
                            changed = true;
                            stream.pending_delta.push_str(text.as_str());
                            stream.pending_chunk_count += 1;
                        }
                    }
                    Ok(SupervisorStreamEvent::RateLimit { state }) => {
                        changed = true;
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
                        changed = true;
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
                        changed = true;
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
                        changed = true;
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
                            changed = true;
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
            let inspector_context = self.supervisor_chat_stream.as_ref().and_then(|stream| {
                if let SupervisorStreamTarget::Inspector { work_item_id } = &stream.target {
                    Some((work_item_id.clone(), self.session_id_for_work_item(work_item_id)))
                } else {
                    None
                }
            });
            if let Some((work_item_id, session_id)) = inspector_context {
                self.publish_error_for_work_item(
                    &work_item_id,
                    session_id,
                    "supervisor-stream",
                    message.as_str(),
                );
            }
            let state = classify_supervisor_stream_error(message.as_str());
            self.status_warning = Some(format!(
                "supervisor {} warning: {}",
                response_state_warning_label(state),
                compact_focus_card_text(message.as_str())
            ));
            changed = true;
        }
        changed
    }

    #[cfg(test)]
    fn has_active_animated_indicator(&mut self, now: Instant) -> bool {
        !matches!(self.animation_state(now), AnimationState::None)
    }

    fn animation_state(&mut self, now: Instant) -> AnimationState {
        let has_active_terminal_turn = self
            .terminal_session_states
            .iter()
            .any(|(session_id, state)| {
                state.turn_active
                    && matches!(
                        self.domain
                            .sessions
                            .get(session_id)
                            .and_then(|session| session.status.as_ref()),
                        Some(WorkerSessionStatus::Running)
                    )
            });
        if has_active_terminal_turn {
            return AnimationState::ActiveTurn;
        }

        if self.attention_projection_for_draw(now).has_resolved_items {
            AnimationState::ResolvedOnly
        } else {
            AnimationState::None
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
            if self.global_supervisor_chat_input.is_empty() {
                "<type in Insert mode>".to_owned()
            } else {
                self.global_supervisor_chat_input.text().to_owned()
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
        let previous = self.selected_inbox_item_id.clone();
        let valid_selected_index = selected_index.filter(|index| *index < rows.len());
        self.selected_inbox_index = valid_selected_index;
        self.selected_inbox_item_id =
            valid_selected_index.map(|index| rows[index].inbox_item_id.clone());
        if self.selected_inbox_item_id.is_some() && self.selected_inbox_item_id != previous {
            if let Some(index) = self.selected_inbox_index {
                self.apply_selection_focus_policy(&rows[index]);
            }
        }
    }

    fn apply_selection_focus_policy(&mut self, selected_row: &UiInboxRow) {
        if selected_row.kind == InboxItemKind::NeedsDecision {
            let _ = self.open_selected_inbox_output(false);
            return;
        }

        self.enter_normal_mode();

        let Some(session_id) = selected_row.session_id.clone() else {
            return;
        };
        if let Some(index) = self
            .session_ids_for_navigation()
            .iter()
            .position(|candidate| candidate == &session_id)
        {
            self.selected_session_index = Some(index);
            self.selected_session_id = Some(session_id);
        }
    }

    fn application_mode_label(&self) -> &'static str {
        self.application_mode.label()
    }

    fn submit_frontend_command_intent(&mut self, command: FrontendCommandIntent) {
        let Some(controller) = self.frontend_controller.clone() else {
            return;
        };
        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = controller.submit_intent(FrontendIntent::Command(command)).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "frontend intent unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn set_application_mode_autopilot(&mut self) {
        self.application_mode = ApplicationMode::Autopilot;
        self.status_warning = Some("application mode: autopilot".to_owned());
        self.submit_frontend_command_intent(FrontendCommandIntent::SetApplicationModeAutopilot);
    }

    fn set_application_mode_manual(&mut self) {
        self.application_mode = ApplicationMode::Manual;
        self.status_warning = Some("application mode: manual".to_owned());
        self.submit_frontend_command_intent(FrontendCommandIntent::SetApplicationModeManual);
    }

    fn enter_normal_mode(&mut self) {
        self.apply_ui_mode(UiMode::Normal);
    }

    fn enter_insert_mode(&mut self) {
        if !self.is_terminal_view_active() {
            self.apply_ui_mode(UiMode::Insert);
        }
    }

    fn enter_insert_mode_for_current_focus(&mut self) {
        if self.is_terminal_view_active() {
            if self.terminal_session_has_any_needs_input() && !self.terminal_session_has_active_needs_input()
            {
                let _ = self.activate_terminal_needs_input(true);
            } else {
                self.enter_terminal_mode();
            }
            return;
        }

        if self.is_global_supervisor_chat_active() {
            self.enter_insert_mode();
            return;
        }

        if self.selected_session_id_for_panel().is_some() {
            self.open_terminal_and_enter_mode();
            return;
        }

        self.enter_insert_mode();
    }

    fn enter_terminal_mode(&mut self) {
        if self.is_terminal_view_active() {
            self.snap_active_terminal_output_to_bottom();
            self.apply_ui_mode(UiMode::Terminal);
            self.schedule_session_info_summary_refresh_for_active_session();
        }
    }

    fn apply_ui_mode(&mut self, mode: UiMode) {
        self.mode = mode;
        self.mode_key_buffer.clear();
        self.which_key_overlay = None;
        self.terminal_escape_pending = false;
        self.terminal_compose_editor.mode = match mode {
            UiMode::Normal => EditorMode::Normal,
            UiMode::Insert | UiMode::Terminal => EditorMode::Insert,
        };
    }

    fn open_terminal_and_enter_mode(&mut self) {
        self.open_terminal_for_selected();
        self.enter_terminal_mode();
    }

    fn needs_input_prompt_from_event(
        &self,
        session_id: &WorkerSessionId,
        event: BackendNeedsInputEvent,
    ) -> NeedsInputPromptState {
        let is_structured_plan_request = !event.questions.is_empty();
        let default_option_labels = if event.questions.is_empty() {
            vec![event.default_option.clone()]
        } else {
            vec![None; event.questions.len()]
        };
        let questions = if event.questions.is_empty() {
            vec![BackendNeedsInputQuestion {
                id: event.prompt_id.clone(),
                header: "Input".to_owned(),
                question: event.question,
                is_other: false,
                is_secret: false,
                options: (!event.options.is_empty()).then(|| {
                    event
                        .options
                        .into_iter()
                        .map(|label| BackendNeedsInputOption {
                            label,
                            description: String::new(),
                        })
                        .collect::<Vec<_>>()
                }),
            }]
        } else {
            event.questions
        };
        NeedsInputPromptState {
            prompt_id: event.prompt_id,
            questions,
            default_option_labels,
            requires_manual_activation: self
                .session_requires_manual_needs_input_activation(session_id),
            is_structured_plan_request,
        }
    }

    fn active_terminal_needs_input(&self) -> Option<&NeedsInputComposerState> {
        let session_id = self.active_terminal_session_id()?;
        self.terminal_session_states
            .get(session_id)
            .and_then(|view| view.active_needs_input.as_ref())
    }

    fn active_terminal_needs_input_mut(&mut self) -> Option<&mut NeedsInputComposerState> {
        let session_id = self.active_terminal_session_id()?.clone();
        self.terminal_session_states
            .get_mut(&session_id)
            .and_then(|view| view.active_needs_input.as_mut())
    }

    fn terminal_session_has_active_needs_input(&self) -> bool {
        self.active_terminal_needs_input()
            .map(|prompt| prompt.interaction_active)
            .unwrap_or(false)
    }

    fn terminal_session_has_any_needs_input(&self) -> bool {
        self.active_terminal_needs_input().is_some()
    }

    fn active_terminal_session_requires_manual_needs_input_activation(&self) -> bool {
        self.active_terminal_session_id()
            .map(|session_id| self.session_requires_manual_needs_input_activation(session_id))
            .unwrap_or(false)
    }

    fn deactivate_terminal_needs_input_interaction(&mut self) -> bool {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return false;
        };
        if !prompt.interaction_active {
            return false;
        }
        prompt.persist_current_draft();
        prompt.set_interaction_active(false);
        true
    }

    fn activate_terminal_needs_input(&mut self, enable_note_insert_mode: bool) -> bool {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return false;
        };
        if prompt.interaction_active {
            return false;
        }
        prompt.set_interaction_active(true);
        if enable_note_insert_mode {
            prompt.note_insert_mode = true;
            prompt.note_editor_state.mode = EditorMode::Insert;
            prompt.select_state.focused = false;
        } else {
            prompt.note_insert_mode = false;
            prompt.note_editor_state.mode = EditorMode::Normal;
            prompt.select_state.focused = prompt.current_question_requires_option_selection();
        }
        true
    }

    fn terminal_needs_input_is_note_insert_mode(&self) -> bool {
        self.active_terminal_needs_input()
            .map(|prompt| prompt.note_insert_mode)
            .unwrap_or(false)
    }

    fn move_terminal_needs_input_question(&mut self, delta: isize) {
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return;
        };
        let planning_workflow = matches!(
            self.workflow_state_for_session(&session_id),
            Some(WorkflowState::Planning)
        );
        let Some(prompt) = self
            .terminal_session_states
            .get_mut(&session_id)
            .and_then(|view| view.active_needs_input.as_mut())
        else {
            return;
        };
        let current = prompt.current_question_index as isize;
        let upper = prompt.questions.len().saturating_sub(1) as isize;
        let next = (current + delta).clamp(0, upper) as usize;
        let question_changed = next != prompt.current_question_index;
        prompt.move_to_question(next);
        if planning_workflow && question_changed {
            prompt.note_insert_mode = false;
            prompt.note_editor_state.mode = EditorMode::Normal;
            prompt.select_state.focused = prompt.current_question_requires_option_selection();
        }
    }

    fn toggle_terminal_needs_input_note_insert_mode(&mut self, enabled: bool) {
        {
            let Some(prompt) = self.active_terminal_needs_input_mut() else {
                return;
            };
            if !prompt.interaction_active {
                return;
            }
            prompt.note_insert_mode = enabled;
            prompt.note_editor_state.mode = if enabled {
                EditorMode::Insert
            } else {
                EditorMode::Normal
            };
            prompt.select_state.focused =
                !enabled && prompt.current_question_requires_option_selection();
        }
        if enabled {
            self.apply_ui_mode(UiMode::Insert);
        } else {
            self.apply_ui_mode(UiMode::Normal);
        }
    }

    fn apply_terminal_needs_input_note_key(&mut self, key: KeyEvent) -> bool {
        if self.mode != UiMode::Insert {
            return false;
        }
        let mut exit_to_normal_mode = false;
        let handled = {
            let Some(prompt) = self.active_terminal_needs_input_mut() else {
                return false;
            };
            if !prompt.interaction_active || !prompt.note_insert_mode {
                return false;
            }

            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    prompt.note_insert_mode = false;
                    prompt.note_editor_state.mode = EditorMode::Normal;
                    prompt.select_state.focused =
                        prompt.current_question_requires_option_selection();
                    exit_to_normal_mode = true;
                    true
                }
                KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                    let enter = edtui_key_input(KeyCode::Enter, KeyModifiers::NONE)
                        .expect("enter key conversion");
                    EditorEventHandler::default().on_key_event(enter, &mut prompt.note_editor_state);
                    true
                }
                KeyCode::Enter
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
                {
                    false
                }
                _ => {
                    if let Some(key_input) = map_edtui_key_input(key) {
                        EditorEventHandler::default()
                            .on_key_event(key_input, &mut prompt.note_editor_state);
                    }
                    true
                }
            }
        };
        if exit_to_normal_mode {
            self.apply_ui_mode(UiMode::Normal);
        }
        handled
    }

    fn submit_terminal_needs_input_response(&mut self) {
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return;
        };
        let _ = self.submit_terminal_needs_input_response_for_session(&session_id);
    }

    fn set_needs_input_error_for_session(&mut self, session_id: &WorkerSessionId, message: &str) {
        if let Some(prompt) = self
            .terminal_session_states
            .get_mut(session_id)
            .and_then(|view| view.active_needs_input.as_mut())
        {
            prompt.error = Some(message.to_owned());
        }
    }

    fn submit_terminal_needs_input_response_for_session(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> bool {
        let (prompt_id, answers) = {
            let Some(active_prompt) = self
                .terminal_session_states
                .get_mut(session_id)
                .and_then(|view| view.active_needs_input.as_mut())
            else {
                return false;
            };
            let prompt_id = active_prompt.prompt_id.clone();
            let answers = match active_prompt.build_runtime_answers() {
                Ok(answers) => answers,
                Err(error) => {
                    active_prompt.error =
                        Some(sanitize_terminal_display_text(error.to_string().as_str()));
                    return false;
                }
            };
            (prompt_id, answers)
        };

        if let Some(controller) = self.frontend_controller.clone() {
            match TokioHandle::try_current() {
                Ok(runtime) => {
                    let target_session_id = session_id.clone();
                    runtime.spawn(async move {
                        let _ = controller
                            .submit_intent(FrontendIntent::RespondToNeedsInput {
                                session_id: target_session_id,
                                prompt_id,
                                answers,
                            })
                            .await;
                    });
                    if let Some(view) = self.terminal_session_states.get_mut(&session_id) {
                        view.complete_active_needs_input_prompt();
                    }
                    if let Some(work_item_id) = self.work_item_id_for_session(&session_id) {
                        self.acknowledge_needs_decision_for_work_item(&work_item_id);
                    }
                    let _ = self.mark_reconcile_dirty_for_session(&session_id);
                    return true;
                }
                Err(_) => {
                    self.set_needs_input_error_for_session(
                        session_id,
                        "input response unavailable: tokio runtime unavailable",
                    );
                    return false;
                }
            }
        }

        self.set_needs_input_error_for_session(
            session_id,
            "input response unavailable: frontend controller is not configured",
        );
        false
    }

    fn recommended_option_index(options: &[BackendNeedsInputOption]) -> Option<usize> {
        options.iter().position(|option| {
            option
                .label
                .to_ascii_lowercase()
                .contains("(recommended)")
        })
    }

    fn select_autopilot_answers_for_prompt(prompt: &mut NeedsInputComposerState) -> bool {
        for (index, question) in prompt.questions.iter().enumerate() {
            let Some(draft) = prompt.answer_drafts.get_mut(index) else {
                return false;
            };
            let Some(options) = question.options.as_ref() else {
                if draft.note.trim().is_empty() {
                    return false;
                }
                continue;
            };
            if options.is_empty() {
                if draft.note.trim().is_empty() {
                    return false;
                }
                continue;
            }
            if draft
                .selected_option_index
                .is_some_and(|selected| selected < options.len())
            {
                if index == prompt.current_question_index {
                    prompt.select_state.selected_index = draft.selected_option_index;
                    if let Some(selected) = draft.selected_option_index {
                        prompt.select_state.highlighted_index = selected;
                    }
                }
                continue;
            }
            let default_index = prompt
                .default_option_labels
                .get(index)
                .and_then(|label| label.as_deref())
                .and_then(|label| options.iter().position(|option| option.label == label));
            let selected = Self::recommended_option_index(options)
                .or(default_index)
                .unwrap_or(0);
            draft.selected_option_index = Some(selected);
            if index == prompt.current_question_index {
                prompt.select_state.selected_index = Some(selected);
                prompt.select_state.highlighted_index = selected;
            }
        }

        true
    }

    fn toggle_worktree_diff_modal(&mut self) {
        if self.worktree_diff_modal.is_some() {
            self.close_worktree_diff_modal();
            return;
        }
        self.open_worktree_diff_modal();
    }

    fn close_worktree_diff_modal(&mut self) {
        self.worktree_diff_modal = None;
    }

    fn open_worktree_diff_modal(&mut self) {
        let Some(session_id) = self.selected_session_id_for_terminal_action() else {
            self.status_warning =
                Some("diff unavailable: no active or selected terminal session".to_owned());
            return;
        };
        self.worktree_diff_modal = Some(WorktreeDiffModalState {
            session_id: session_id.clone(),
            base_branch: "main".to_owned(),
            content: String::new(),
            loading: true,
            error: None,
            scroll: 0,
            cursor_line: 0,
            selected_file_index: 0,
            selected_hunk_index: 0,
            focus: DiffPaneFocus::Files,
        });
        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!(
            "worktree diff fetch for {} is handled outside the TUI loop",
            labels.compact_label
        ));
    }

    fn scroll_worktree_diff_modal(&mut self, delta: isize) {
        let Some(modal) = self.worktree_diff_modal.as_mut() else {
            return;
        };
        match modal.focus {
            DiffPaneFocus::Files => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                if files.is_empty() {
                    return;
                }
                let max_index = files.len().saturating_sub(1);
                let current = modal.selected_file_index.min(max_index);
                let next = if delta < 0 {
                    current.saturating_sub(delta.unsigned_abs())
                } else {
                    current.saturating_add(delta as usize).min(max_index)
                };
                modal.selected_file_index = next;
                modal.selected_hunk_index = 0;
                modal.cursor_line = files[next].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
            DiffPaneFocus::Diff => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                let Some(file) = files.get(modal.selected_file_index.min(files.len().saturating_sub(1)))
                else {
                    return;
                };
                if file.addition_blocks.is_empty() {
                    return;
                }
                let max_index = file.addition_blocks.len().saturating_sub(1);
                let current = modal.selected_hunk_index.min(max_index);
                let next = if delta < 0 {
                    current.saturating_sub(delta.unsigned_abs())
                } else {
                    current.saturating_add(delta as usize).min(max_index)
                };
                modal.selected_hunk_index = next;
                modal.cursor_line = file.addition_blocks[next].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
        }
    }

    fn jump_worktree_diff_addition_block(&mut self, to_last: bool) {
        let Some(modal) = self.worktree_diff_modal.as_mut() else {
            return;
        };
        match modal.focus {
            DiffPaneFocus::Files => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                if files.is_empty() {
                    modal.cursor_line = if to_last {
                        worktree_diff_modal_line_count(modal).saturating_sub(1)
                    } else {
                        0
                    };
                    modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
                    return;
                }
                modal.selected_file_index = if to_last {
                    files.len().saturating_sub(1)
                } else {
                    0
                };
                modal.selected_hunk_index = 0;
                modal.cursor_line = files[modal.selected_file_index].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
            DiffPaneFocus::Diff => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                let Some(file) = files.get(modal.selected_file_index.min(files.len().saturating_sub(1)))
                else {
                    return;
                };
                if file.addition_blocks.is_empty() {
                    return;
                }
                modal.selected_hunk_index = if to_last {
                    file.addition_blocks.len().saturating_sub(1)
                } else {
                    0
                };
                modal.cursor_line = file.addition_blocks[modal.selected_hunk_index].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
        }
    }

    fn focus_worktree_diff_files_pane(&mut self) {
        if let Some(modal) = self.worktree_diff_modal.as_mut() {
            modal.focus = DiffPaneFocus::Files;
        }
    }

    fn focus_worktree_diff_detail_pane(&mut self) {
        if let Some(modal) = self.worktree_diff_modal.as_mut() {
            modal.focus = DiffPaneFocus::Diff;
        }
    }

    fn insert_selected_worktree_diff_refs_into_compose(&mut self) {
        let refs = {
            let Some(modal) = self.worktree_diff_modal.as_ref() else {
                return;
            };
            match collect_selected_worktree_diff_refs(modal) {
                Ok(entries) => entries,
                Err(message) => {
                    self.status_warning = Some(message);
                    return;
                }
            }
        };

        if refs.is_empty() {
            self.status_warning = Some("diff selection has no target-side lines".to_owned());
            return;
        }

        let insertion = refs.join(" ");
        let mut current = editor_state_text(&self.terminal_compose_editor);
        if !current.is_empty() && !current.ends_with(char::is_whitespace) {
            current.push(' ');
        }
        current.push_str(insertion.as_str());
        if !current.ends_with(char::is_whitespace) {
            current.push(' ');
        }
        set_editor_state_text(&mut self.terminal_compose_editor, current.as_str());
        self.status_warning = Some(format!(
            "added {} diff reference{} to compose input",
            refs.len(),
            if refs.len() == 1 { "" } else { "s" }
        ));
    }

    fn schedule_session_info_summary_refresh_for_active_session(&mut self) {
        if !self.should_show_session_info_sidebar() {
            self.session_info_summary_deadline = None;
            return;
        }
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            let now = Instant::now();
            let deadline = if self.session_info_is_foreground() {
                now + Duration::from_millis(500)
            } else {
                let interval = self.runtime_config.session_info_background_refresh_interval();
                self.session_info_summary_last_refresh_at
                    .get(&session_id)
                    .map(|previous| (*previous + interval).max(now))
                    .unwrap_or(now + interval)
            };
            self.session_info_summary_deadline = Some(
                self.session_info_summary_deadline
                    .map(|existing| existing.min(deadline))
                    .unwrap_or(deadline),
            );
            let cache = self.session_info_summary_cache.entry(session_id).or_default();
            cache.loading = true;
            cache.error = None;
        }
    }

    fn poll_session_info_summary_events(&mut self) -> bool {
        let mut events = Vec::new();
        {
            let Some(receiver) = self.session_info_summary_receiver.as_mut() else {
                return false;
            };
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }

        let changed = !events.is_empty();
        for event in events {
            match event {
                SessionInfoSummaryEvent::Completed {
                    session_id,
                    summary,
                    context_fingerprint,
                } => {
                    let cache = self.session_info_summary_cache.entry(session_id).or_default();
                    if cache.context_fingerprint.as_deref() != Some(context_fingerprint.as_str()) {
                        continue;
                    }
                    cache.loading = false;
                    cache.error = None;
                    cache.text = Some(summary);
                }
                SessionInfoSummaryEvent::Failed {
                    session_id,
                    message,
                    context_fingerprint,
                } => {
                    let cache = self.session_info_summary_cache.entry(session_id).or_default();
                    if cache.context_fingerprint.as_deref() != Some(context_fingerprint.as_str()) {
                        continue;
                    }
                    cache.loading = false;
                    cache.error = Some(message);
                    if cache.text.is_none() {
                        cache.text = None;
                    }
                }
            }
        }
        changed
    }

    fn tick_session_info_summary_refresh_at(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.session_info_summary_deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }
        if !self.session_info_is_foreground() {
            let Some(session_id) = self.active_terminal_session_id().cloned() else {
                self.session_info_summary_deadline = None;
                return false;
            };
            let interval = self.runtime_config.session_info_background_refresh_interval();
            if let Some(previous) = self.session_info_summary_last_refresh_at.get(&session_id) {
                if now.duration_since(*previous) < interval {
                    self.session_info_summary_deadline = Some(*previous + interval);
                    return false;
                }
            }
        }
        self.session_info_summary_deadline = None;
        self.spawn_active_session_summary_refresh()
    }

    #[cfg(test)]
    fn tick_session_info_summary_refresh(&mut self) -> bool {
        self.tick_session_info_summary_refresh_at(Instant::now())
    }

    fn drain_async_events_and_report(&mut self) -> bool {
        false
    }

    fn poll_frontend_events_and_report(
        &mut self,
        receiver: Option<&mut mpsc::Receiver<FrontendEvent>>,
    ) -> bool {
        let Some(receiver) = receiver else {
            return false;
        };

        let mut changed = false;
        let mut had_terminal_events = false;
        loop {
            match receiver.try_recv() {
                Ok(FrontendEvent::SnapshotUpdated(snapshot)) => {
                    changed |= self.apply_frontend_snapshot(snapshot);
                }
                Ok(FrontendEvent::Notification(notification)) => {
                    changed |= self.apply_frontend_notification(notification);
                }
                Ok(FrontendEvent::TerminalSession(event)) => {
                    had_terminal_events = true;
                    changed |= self.apply_frontend_terminal_session_event(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    changed |= self.apply_frontend_notification(FrontendNotification {
                        level: FrontendNotificationLevel::Warning,
                        message: "frontend event stream disconnected".to_owned(),
                        work_item_id: None,
                        session_id: None,
                    });
                    break;
                }
            }
        }
        if had_terminal_events {
            self.recompute_background_terminal_flush_deadline();
        }
        changed
    }

    fn apply_frontend_snapshot(&mut self, snapshot: FrontendSnapshot) -> bool {
        let mut changed = false;
        if self.base_status != snapshot.status {
            self.base_status = snapshot.status;
            changed = true;
        }
        if self.domain != snapshot.projection {
            self.replace_domain_projection(snapshot.projection, "frontend snapshot update");
            changed = true;
        }
        let target_mode = match snapshot.application_mode {
            FrontendApplicationMode::Manual => ApplicationMode::Manual,
            FrontendApplicationMode::Autopilot => ApplicationMode::Autopilot,
        };
        if self.application_mode != target_mode {
            self.application_mode = target_mode;
            changed = true;
        }
        if changed {
            self.sync_selected_session_panel_state();
        }
        changed
    }

    fn apply_frontend_notification(&mut self, notification: FrontendNotification) -> bool {
        let prefix = match notification.level {
            FrontendNotificationLevel::Info => "info",
            FrontendNotificationLevel::Warning => "warning",
            FrontendNotificationLevel::Error => "error",
        };
        let message = sanitize_terminal_display_text(notification.message.as_str());
        let warning = format!("{prefix}: {message}");
        if self.status_warning.as_deref() == Some(warning.as_str()) {
            return false;
        }
        self.status_warning = Some(warning);
        true
    }

    fn apply_frontend_terminal_session_event(&mut self, event: FrontendTerminalEvent) -> bool {
        match event {
            FrontendTerminalEvent::Output { session_id, output } => {
                let is_active_session = self.active_terminal_session_id() == Some(&session_id);
                let view = self.terminal_session_states.entry(session_id).or_default();
                if is_active_session {
                    if !view.deferred_output.is_empty() {
                        let deferred = std::mem::take(&mut view.deferred_output);
                        append_terminal_assistant_output(
                            view,
                            deferred,
                            self.runtime_config.transcript_line_limit,
                        );
                        view.last_background_flush_at = Some(Instant::now());
                    }
                    append_terminal_assistant_output(
                        view,
                        output.bytes,
                        self.runtime_config.transcript_line_limit,
                    );
                } else {
                    view.deferred_output.extend_from_slice(output.bytes.as_slice());
                    if view.deferred_output.len() >= BACKGROUND_SESSION_DEFERRED_OUTPUT_MAX_BYTES {
                        let deferred = std::mem::take(&mut view.deferred_output);
                        append_terminal_assistant_output(
                            view,
                            deferred,
                            self.runtime_config.transcript_line_limit,
                        );
                        view.last_background_flush_at = Some(Instant::now());
                    } else if view.last_background_flush_at.is_none() {
                        view.last_background_flush_at = Some(Instant::now());
                    }
                }
                view.error = None;
                true
            }
            FrontendTerminalEvent::TurnState {
                session_id,
                turn_state,
            } => {
                let view = self
                    .terminal_session_states
                    .entry(session_id.clone())
                    .or_default();
                let previous_turn_active = view.turn_active;
                view.turn_active = turn_state.active;
                let _ = previous_turn_active;
                true
            }
            FrontendTerminalEvent::NeedsInput {
                session_id,
                needs_input,
            } => {
                let prompt = self.needs_input_prompt_from_event(&session_id, needs_input);
                let view = self
                    .terminal_session_states
                    .entry(session_id.clone())
                    .or_default();
                view.enqueue_needs_input_prompt(prompt);
                true
            }
            FrontendTerminalEvent::StreamFailed {
                session_id,
                message,
                is_session_not_found,
            } => {
                self.terminal_session_streamed.remove(&session_id);
                let view = self
                    .terminal_session_states
                    .entry(session_id.clone())
                    .or_default();
                view.error = Some(message.clone());
                view.deferred_output.clear();
                view.last_background_flush_at = None;
                view.entries.clear();
                view.transcript_truncated = false;
                view.transcript_truncated_line_count = 0;
                view.output_fragment.clear();
                view.render_cache.invalidate_all();
                view.output_rendered_line_count = 0;
                view.output_scroll_line = 0;
                view.output_follow_tail = true;
                view.turn_active = false;
                let changed = true;
                if is_session_not_found {
                    self.recover_terminal_session_on_not_found(&session_id);
                }
                changed
            }
            FrontendTerminalEvent::StreamEnded { session_id } => {
                self.terminal_session_streamed.remove(&session_id);
                let view = self
                    .terminal_session_states
                    .entry(session_id.clone())
                    .or_default();
                if !view.deferred_output.is_empty() {
                    let deferred = std::mem::take(&mut view.deferred_output);
                    append_terminal_assistant_output(
                        view,
                        deferred,
                        self.runtime_config.transcript_line_limit,
                    );
                }
                view.last_background_flush_at = None;
                flush_terminal_output_fragment(
                    view,
                    self.runtime_config.transcript_line_limit,
                );
                view.turn_active = false;
                true
            }
        }
    }

    fn run_due_periodic_tasks_and_report(&mut self, now: Instant) -> bool {
        self.flush_background_terminal_output_and_report_at(now)
    }

    fn maintain_active_terminal_view_and_report(&mut self) -> bool {
        let mut changed = false;
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            changed |= self.flush_deferred_terminal_output_for_session(&session_id);
        }
        changed
    }

    fn has_pending_async_activity(&self) -> bool {
        let terminal_busy = !self.terminal_session_streamed.is_empty()
            || self
                .terminal_session_states
                .values()
                .any(|view| view.turn_active || !view.deferred_output.is_empty());
        terminal_busy
    }

    fn next_wake_deadline(
        &self,
        _now: Instant,
        animation_state: AnimationState,
        last_animation_frame: Instant,
    ) -> Option<Instant> {
        let animation_deadline = match animation_state {
            AnimationState::ActiveTurn => Some(last_animation_frame + Duration::from_millis(200)),
            AnimationState::ResolvedOnly => Some(last_animation_frame + Duration::from_millis(1_000)),
            AnimationState::None => None,
        };
        let background_flush_deadline = self.background_terminal_flush_deadline;
        [
            animation_deadline,
            background_flush_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn spawn_active_session_summary_refresh(&mut self) -> bool {
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return false;
        };
        let Some(provider) = self.supervisor_provider.clone() else {
            let cache = self.session_info_summary_cache.entry(session_id).or_default();
            cache.loading = false;
            cache.error = Some("no LLM provider configured".to_owned());
            return true;
        };
        let diff_cache = self.session_info_diff_cache.get(&session_id);
        let Some(context) = session_info_summary_prompt(&self.domain, &session_id, diff_cache) else {
            return false;
        };

        let cache = self
            .session_info_summary_cache
            .entry(context.session_id.clone())
            .or_default();
        if cache.context_fingerprint.as_deref() == Some(context.context_fingerprint.as_str())
            && cache.text.is_some()
            && cache.error.is_none()
        {
            cache.loading = false;
            return false;
        }
        cache.context_fingerprint = Some(context.context_fingerprint.clone());
        cache.loading = true;
        cache.error = None;

        let Some(sender) = self.session_info_summary_sender.clone() else {
            cache.loading = false;
            cache.error = Some("summary channel unavailable".to_owned());
            return true;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                self.session_info_summary_last_refresh_at
                    .insert(context.session_id.clone(), Instant::now());
                let supervisor_model = self.runtime_config.supervisor_model.clone();
                handle.spawn(async move {
                    run_session_info_summary_task(provider, context, supervisor_model, sender).await;
                });
            }
            Err(_) => {
                cache.loading = false;
                cache.error = Some("tokio runtime unavailable".to_owned());
            }
        }
        true
    }

    fn ensure_session_info_diff_loaded_for_active_session(&mut self) -> bool {
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return false;
        };
        let needs_load = self
            .session_info_diff_cache
            .get(&session_id)
            .map(|entry| entry.content.is_empty() && !entry.loading)
            .unwrap_or(true);
        if !needs_load {
            return false;
        }
        if !self.session_info_is_foreground() {
            let now = Instant::now();
            let interval = self.runtime_config.session_info_background_refresh_interval();
            match self.session_info_diff_last_refresh_at.get(&session_id).copied() {
                Some(previous) => {
                    if now.duration_since(previous) < interval {
                        return false;
                    }
                }
                None => {
                    self.session_info_diff_last_refresh_at
                        .insert(session_id.clone(), now);
                    return false;
                }
            }
        }
        self.spawn_session_info_diff_load(session_id)
    }

    fn spawn_session_info_diff_load(&mut self, session_id: WorkerSessionId) -> bool {
        let cache = self
            .session_info_diff_cache
            .entry(session_id.clone())
            .or_default();
        cache.loading = true;
        cache.error = None;
        if cache.content.is_empty() {
            cache.content = String::new();
        }
        let Some(provider) = self.ticket_picker_provider.clone() else {
            cache.loading = false;
            cache.error = Some("ticket provider unavailable".to_owned());
            return true;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            cache.loading = false;
            cache.error = Some("ticket event channel unavailable".to_owned());
            return true;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                self.session_info_diff_last_refresh_at
                    .insert(session_id.clone(), Instant::now());
                handle.spawn(async move {
                    run_session_diff_load_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                cache.loading = false;
                cache.error = Some("tokio runtime unavailable".to_owned());
            }
        }
        true
    }

    fn spawn_session_diff_load(&mut self, session_id: WorkerSessionId) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            if let Some(modal) = self.worktree_diff_modal.as_mut() {
                modal.loading = false;
                modal.error =
                    Some("diff unavailable: ticket provider is not configured".to_owned());
            }
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            if let Some(modal) = self.worktree_diff_modal.as_mut() {
                modal.loading = false;
                modal.error =
                    Some("diff unavailable: ticket event channel is not available".to_owned());
            }
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_session_diff_load_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    modal.loading = false;
                    modal.error = Some("diff unavailable: tokio runtime is not active".to_owned());
                }
            }
        }
    }

    fn spawn_session_workflow_advance(&mut self, session_id: WorkerSessionId) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.status_warning =
                Some("workflow advance unavailable: ticket provider is not configured".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.status_warning = Some(
                "workflow advance unavailable: ticket picker event channel is not available"
                    .to_owned(),
            );
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_session_workflow_advance_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                self.status_warning =
                    Some("workflow advance unavailable: tokio runtime is not active".to_owned());
            }
        }
    }

    fn spawn_session_merge_finalize(&mut self, session_id: WorkerSessionId) {
        if !self.merge_finalizing_sessions.insert(session_id.clone()) {
            return;
        }

        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.merge_finalizing_sessions.remove(&session_id);
            return;
        };
        let Some(sender) = self.merge_event_sender.clone() else {
            self.merge_finalizing_sessions.remove(&session_id);
            self.status_warning = Some(
                "merge finalization unavailable: merge event channel is not available".to_owned(),
            );
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_session_merge_finalize_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                self.merge_finalizing_sessions.remove(&session_id);
                self.status_warning = Some(
                    "merge finalization unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
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

        if editor_state_text(&self.terminal_compose_editor).trim().is_empty() {
            self.status_warning = Some(
                "terminal input unavailable: compose a non-empty message before sending".to_owned(),
            );
            return;
        }

        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            self.status_warning =
                Some("terminal input unavailable: no active terminal session selected".to_owned());
            return;
        };
        let user_message = editor_state_text(&self.terminal_compose_editor);
        let backend_kind = self.worker_backend.as_ref().map(|backend| backend.kind());

        if backend_kind != Some(BackendKind::Codex) {
            let view = self
                .terminal_session_states
                .entry(session_id.clone())
                .or_default();
            append_terminal_user_message(
                view,
                user_message.as_str(),
                self.runtime_config.transcript_line_limit,
            );
            view.error = None;
        }

        if let Some(controller) = self.frontend_controller.clone() {
            match TokioHandle::try_current() {
                Ok(runtime) => {
                    let target_session_id = session_id.clone();
                    let payload = user_message.clone();
                    runtime.spawn(async move {
                        let _ = controller
                            .submit_intent(FrontendIntent::SendTerminalInput {
                                session_id: target_session_id,
                                input: payload,
                            })
                            .await;
                    });
                    clear_editor_state(&mut self.terminal_compose_editor);
                    self.enter_normal_mode();
                }
                Err(_) => {
                    self.status_warning =
                        Some("terminal input unavailable: tokio runtime unavailable".to_owned());
                }
            }
            return;
        }

        self.status_warning =
            Some("terminal input unavailable: frontend controller is not configured".to_owned());
    }

    fn advance_terminal_workflow_stage(&mut self) {
        if !self.is_terminal_view_active() {
            self.status_warning =
                Some("workflow advance unavailable: open a terminal session first".to_owned());
            return;
        }

        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            self.status_warning = Some(
                "workflow advance unavailable: no active terminal session selected".to_owned(),
            );
            return;
        };

        let current_state = self
            .domain
            .sessions
            .get(&session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.clone());

        let Some(current_state) = current_state else {
            let labels = session_display_labels(&self.domain, &session_id);
            self.status_warning = Some(format!(
                "workflow advance unavailable: {} has no canonical workflow state",
                labels.compact_label
            ));
            return;
        };

        if matches!(
            current_state,
            WorkflowState::AwaitingYourReview
                | WorkflowState::ReadyForReview
                | WorkflowState::InReview
        ) {
            self.review_merge_confirm_session = Some(session_id);
            self.status_warning = None;
            return;
        }

        if matches!(current_state, WorkflowState::Done | WorkflowState::Abandoned) {
            self.status_warning = Some("workflow advance ignored: session is already complete".to_owned());
            return;
        }

        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!(
            "workflow advance for {} from {:?} is handled outside the TUI loop",
            labels.compact_label, current_state
        ));
    }

    fn is_terminal_view_active(&self) -> bool {
        matches!(
            self.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        )
    }

    fn cancel_review_merge_confirmation(&mut self) {
        self.review_merge_confirm_session = None;
    }

    fn confirm_review_merge(&mut self) {
        let Some(session_id) = self.review_merge_confirm_session.take() else {
            return;
        };
        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!(
            "review merge for {} is handled outside the TUI loop",
            labels.compact_label
        ));
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

impl Drop for UiShellState {
    fn drop(&mut self) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };
        if let Ok(runtime) = TokioHandle::try_current() {
            runtime.spawn(async move {
                let _ = run_stop_pr_pipeline_polling_task(provider).await;
            });
        }
    }
}

fn terminal_output_line_count_for_scroll(view: &TerminalViewState) -> usize {
    if view.output_rendered_line_count > 0 {
        return view.output_rendered_line_count;
    }
    render_terminal_transcript_line_count(view)
}

fn editor_state_text(state: &EditorState) -> String {
    state.lines.to_string()
}

fn set_editor_state_text(state: &mut EditorState, text: &str) {
    *state = insert_mode_editor_state_with_text(text);
}

fn clear_editor_state(state: &mut EditorState) {
    *state = insert_mode_editor_state();
}

fn insert_mode_editor_state() -> EditorState {
    insert_mode_editor_state_with_text("")
}

fn insert_mode_editor_state_with_text(text: &str) -> EditorState {
    let mut state = EditorState::new(Lines::from(text));
    state.mode = EditorMode::Insert;
    state
}

fn map_edtui_key_input(key: KeyEvent) -> Option<edtui::events::KeyInput> {
    edtui_key_input(key.code, key.modifiers)
}

fn edtui_key_input(code: KeyCode, modifiers: KeyModifiers) -> Option<edtui::events::KeyInput> {
    let code = match code {
        KeyCode::Char(ch) => ratatui::crossterm::event::KeyCode::Char(ch),
        KeyCode::Enter => ratatui::crossterm::event::KeyCode::Enter,
        KeyCode::Esc => ratatui::crossterm::event::KeyCode::Esc,
        KeyCode::Backspace => ratatui::crossterm::event::KeyCode::Backspace,
        KeyCode::Delete => ratatui::crossterm::event::KeyCode::Delete,
        KeyCode::Tab => ratatui::crossterm::event::KeyCode::Tab,
        KeyCode::Left => ratatui::crossterm::event::KeyCode::Left,
        KeyCode::Right => ratatui::crossterm::event::KeyCode::Right,
        KeyCode::Up => ratatui::crossterm::event::KeyCode::Up,
        KeyCode::Down => ratatui::crossterm::event::KeyCode::Down,
        KeyCode::Home => ratatui::crossterm::event::KeyCode::Home,
        KeyCode::End => ratatui::crossterm::event::KeyCode::End,
        _ => return None,
    };
    let mut mapped_modifiers = ratatui::crossterm::event::KeyModifiers::empty();
    if modifiers.contains(KeyModifiers::SHIFT) {
        mapped_modifiers |= ratatui::crossterm::event::KeyModifiers::SHIFT;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        mapped_modifiers |= ratatui::crossterm::event::KeyModifiers::CONTROL;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        mapped_modifiers |= ratatui::crossterm::event::KeyModifiers::ALT;
    }
    Some(edtui::events::KeyInput::with_modifiers(
        code,
        mapped_modifiers,
    ))
}

#[allow(dead_code)]
async fn run_session_archive_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.archive_session(session_id.clone()).await {
        Ok(outcome) => {
            let _ = sender
                .send(TicketPickerEvent::SessionArchived {
                    session_id,
                    warning: outcome.warning,
                    event: outcome.event,
                })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::SessionArchiveFailed {
                    session_id,
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                })
                .await;
        }
    }
}

async fn run_set_session_working_state_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    is_working: bool,
) {
    let _ = provider
        .set_session_working_state(session_id, is_working)
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_load_task(
    provider: Arc<dyn TicketPickerProvider>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.list_unfinished_tickets().await {
        Ok(tickets) => {
            let projects = match provider.list_projects().await {
                Ok(projects) => projects,
                Err(_) => Vec::new(),
            };
            let _ = sender
                .send(TicketPickerEvent::TicketsLoaded {
                    tickets,
                    projects,
                })
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

async fn run_session_diff_load_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.session_worktree_diff(session_id.clone()).await {
        Ok(diff) => {
            let _ = sender
                .send(TicketPickerEvent::SessionDiffLoaded { diff })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::SessionDiffFailed {
                    session_id,
                    message: error.to_string(),
                })
                .await;
        }
    }
}

#[allow(dead_code)]
async fn run_session_info_summary_task(
    provider: Arc<dyn LlmProvider>,
    context: SessionInfoContext,
    supervisor_model: String,
    sender: mpsc::Sender<SessionInfoSummaryEvent>,
) {
    let request = summarize_text_output_request(context.prompt.as_str(), supervisor_model.as_str());
    let (_, mut stream) = match provider.stream_chat(request).await {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(SessionInfoSummaryEvent::Failed {
                    session_id: context.session_id,
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                    context_fingerprint: context.context_fingerprint,
                })
                .await;
            return;
        }
    };

    let mut output = String::new();
    loop {
        match stream.next_chunk().await {
            Ok(Some(chunk)) => output.push_str(chunk.delta.as_str()),
            Ok(None) => break,
            Err(error) => {
                let _ = sender
                    .send(SessionInfoSummaryEvent::Failed {
                        session_id: context.session_id,
                        message: sanitize_terminal_display_text(error.to_string().as_str()),
                        context_fingerprint: context.context_fingerprint,
                    })
                    .await;
                return;
            }
        }
    }

    let summary = clamp_summary_text(output.as_str(), 200);
    let _ = sender
        .send(SessionInfoSummaryEvent::Completed {
            session_id: context.session_id,
            summary,
            context_fingerprint: context.context_fingerprint,
        })
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_start_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket: TicketSummary,
    repository_override: Option<PathBuf>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let started_ticket = ticket.clone();

    let result = match provider
        .start_or_resume_ticket(ticket, repository_override)
        .await
    {
        Ok(result) => result,
        Err(error) => match &error {
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
        },
    };

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
            started_session_id: result.mapping.session.session_id,
            projection,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_create_task(
    provider: Arc<dyn TicketPickerProvider>,
    request: CreateTicketFromPickerRequest,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let submit_mode = request.submit_mode.clone();
    let created_ticket = match provider.create_ticket_from_brief(request).await {
        Ok(ticket) => ticket,
        Err(error) => {
            let tickets = match provider.list_unfinished_tickets().await {
                Ok(tickets) => Some(tickets),
                Err(_) => None,
            };
            let _ = sender
                .send(TicketPickerEvent::TicketCreateFailed {
                    message: error.to_string(),
                    tickets,
                    warning: None,
                })
                .await;
            return;
        }
    };

    let mut warning = Vec::new();
    let tickets = match provider.list_unfinished_tickets().await {
        Ok(tickets) => Some(tickets),
        Err(error) => {
            warning.push(format!("failed to refresh tickets: {error}"));
            None
        }
    };
    if created_ticket.assignee.is_none() {
        warning.push(
            "ticket may be unassigned: could not confirm assignment to API-key user".to_owned(),
        );
    }

    let _ = sender
        .send(TicketPickerEvent::TicketCreated {
            created_ticket,
            submit_mode,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_archive_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket: TicketSummary,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let archived_ticket = ticket.clone();
    if let Err(error) = provider.archive_ticket(ticket).await {
        let tickets = match provider.list_unfinished_tickets().await {
            Ok(tickets) => Some(tickets),
            Err(_) => None,
        };
        let _ = sender
            .send(TicketPickerEvent::TicketArchiveFailed {
                ticket: archived_ticket,
                message: error.to_string(),
                tickets,
            })
            .await;
        return;
    }

    let mut warning = Vec::new();
    let tickets = match provider.list_unfinished_tickets().await {
        Ok(tickets) => Some(tickets),
        Err(error) => {
            warning.push(format!("failed to refresh tickets: {error}"));
            None
        }
    };

    let _ = sender
        .send(TicketPickerEvent::TicketArchived {
            archived_ticket,
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
    let mut lines = if overlay.new_ticket_mode {
        vec![
            "Describe ticket | Vim editor (i/Esc) | Enter: create | Shift+Enter: create + start | Esc: cancel"
                .to_owned(),
        ]
    } else {
        vec![
            "j/k or arrows: move | h/Left: fold | l/Right: unfold | Enter: start | x: archive | n: new ticket | Esc: close"
                .to_owned(),
        ]
    };

    if overlay.loading && !overlay.creating {
        lines.push("Loading unfinished tickets...".to_owned());
    }
    if let Some(starting_ticket_id) = overlay.starting_ticket_id.as_ref() {
        lines.push(format!("Starting {}...", starting_ticket_id.as_str()));
    }
    if let Some(archiving_ticket_id) = overlay.archiving_ticket_id.as_ref() {
        lines.push(format!("Archiving {}...", archiving_ticket_id.as_str()));
    }
    if overlay.creating {
        lines.push("Creating ticket...".to_owned());
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
        lines.push("Enter local repository path, then press Enter. Esc to cancel.".to_owned());
        lines.push(format!("Path: {}", overlay.repository_prompt_input.text()));
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
                let archive_marker = if overlay
                    .archive_confirm_ticket
                    .as_ref()
                    .map(|selected| selected.ticket_id == ticket.ticket_id)
                    .unwrap_or_default()
                {
                    "[X]"
                } else {
                    "[x]"
                };
                lines.push(format!(
                    "{selected_prefix}{starting_prefix} {archive_marker} {}: {}",
                    ticket.identifier,
                    compact_focus_card_text(ticket.title.as_str()),
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

fn route_needs_input_modal_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key)
        && shell_state.is_terminal_view_active()
        && shell_state.active_terminal_session_requires_manual_needs_input_activation()
    {
        let _ = shell_state.deactivate_terminal_needs_input_interaction();
        shell_state.enter_normal_mode();
        return RoutedInput::Ignore;
    }

    if shell_state.apply_terminal_needs_input_note_key(key) {
        return RoutedInput::Ignore;
    }

    if !shell_state.is_terminal_view_active() {
        return RoutedInput::Ignore;
    }
    if is_escape_to_normal(key) {
        return RoutedInput::Ignore;
    }
    if !key.modifiers.is_empty() {
        return RoutedInput::Ignore;
    }

    match key.code {
        KeyCode::Char('i') => {
            shell_state.toggle_terminal_needs_input_note_insert_mode(true);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            shell_state.move_terminal_needs_input_question(1);
        }
        KeyCode::Left | KeyCode::Char('h') => {
            shell_state.move_terminal_needs_input_question(-1);
        }
        KeyCode::Enter => {
            let should_advance = shell_state
                .active_terminal_needs_input_mut()
                .map(|prompt| {
                    if prompt.current_question_requires_option_selection() {
                        if prompt.select_state.total_options == 0 {
                            return false;
                        }
                        let selected = prompt
                            .select_state
                            .highlighted_index
                            .min(prompt.select_state.total_options.saturating_sub(1));
                        prompt.select_state.selected_index = Some(selected);
                    }
                    true
                })
                .unwrap_or(false);
            if !should_advance {
                return RoutedInput::Ignore;
            }

            let should_submit = shell_state
                .active_terminal_needs_input()
                .map(|prompt| !prompt.has_next_question())
                .unwrap_or(false);
            if should_submit {
                shell_state.submit_terminal_needs_input_response();
            } else {
                shell_state.move_terminal_needs_input_question(1);
            }
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Char(' ')
        | KeyCode::Char('j')
        | KeyCode::Char('k') => {
            if let Some(prompt) = shell_state.active_terminal_needs_input_mut() {
                let select_code = match key.code {
                    KeyCode::Char('j') => KeyCode::Down,
                    KeyCode::Char('k') => KeyCode::Up,
                    other => other,
                };
                let select_state = &mut prompt.select_state;
                if select_state.enabled && select_state.focused && select_state.total_options > 0 {
                    match select_code {
                        KeyCode::Char(' ') => {
                            let selected = select_state
                                .highlighted_index
                                .min(select_state.total_options.saturating_sub(1));
                            select_state.selected_index = Some(selected);
                        }
                        KeyCode::Up => {
                            select_state.highlight_prev();
                        }
                        KeyCode::Down => {
                            select_state.highlight_next();
                        }
                        KeyCode::Home => {
                            select_state.highlight_first();
                        }
                        KeyCode::End => {
                            select_state.highlight_last();
                        }
                        KeyCode::PageUp => {
                            for _ in 0..5 {
                                select_state.highlight_prev();
                            }
                        }
                        KeyCode::PageDown => {
                            for _ in 0..5 {
                                select_state.highlight_next();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    RoutedInput::Ignore
}

fn route_ticket_picker_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.ticket_picker_overlay.has_repository_prompt() {
        return route_ticket_picker_repository_prompt_key(shell_state, key);
    }

    if shell_state.ticket_picker_overlay.archive_confirm_ticket.is_some() {
        if is_escape_to_normal(key) {
            shell_state.cancel_ticket_picker_archive_confirmation();
            return RoutedInput::Ignore;
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    shell_state.submit_ticket_picker_archive_confirmation();
                    return RoutedInput::Ignore;
                }
                KeyCode::Char('n') => {
                    shell_state.cancel_ticket_picker_archive_confirmation();
                    return RoutedInput::Ignore;
                }
                _ => {}
            }
        }
        return RoutedInput::Ignore;
    }

    if shell_state.ticket_picker_overlay.new_ticket_mode {
        if is_escape_to_normal(key) {
            if shell_state.ticket_picker_overlay.new_ticket_brief_editor.mode != EditorMode::Normal
            {
                if let Some(key_input) = map_edtui_key_input(key) {
                    shell_state
                        .ticket_picker_overlay
                        .new_ticket_brief_event_handler
                        .on_key_event(
                            key_input,
                            &mut shell_state.ticket_picker_overlay.new_ticket_brief_editor,
                        );
                }
            } else {
                shell_state.cancel_create_ticket_from_picker();
            }
            return RoutedInput::Ignore;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return RoutedInput::Ignore;
        }

        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                shell_state.submit_created_ticket_from_picker(TicketCreateSubmitMode::CreateOnly);
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                shell_state.submit_created_ticket_from_picker(TicketCreateSubmitMode::CreateAndStart);
            }
            _ => {
                if let Some(key_input) = map_edtui_key_input(key) {
                    shell_state
                        .ticket_picker_overlay
                        .new_ticket_brief_event_handler
                        .on_key_event(
                            key_input,
                            &mut shell_state.ticket_picker_overlay.new_ticket_brief_editor,
                        );
                }
            }
        }
        return RoutedInput::Ignore;
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
        KeyCode::Char('x') => {
            shell_state.begin_archive_selected_ticket_from_picker();
            RoutedInput::Ignore
        }
        KeyCode::Char('n') => {
            shell_state.begin_create_ticket_from_picker();
            RoutedInput::Ignore
        }
        _ => RoutedInput::Ignore,
    }
}

fn route_review_merge_confirm_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.cancel_review_merge_confirmation();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                shell_state.confirm_review_merge();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('n') => {
                shell_state.cancel_review_merge_confirmation();
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
}

fn route_archive_session_confirm_key(
    shell_state: &mut UiShellState,
    key: KeyEvent,
) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.cancel_archive_selected_session_confirmation();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                shell_state.confirm_archive_selected_session();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('n') => {
                shell_state.cancel_archive_selected_session_confirmation();
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
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
    runtime_config: UiRuntimeConfig,
    frontend_controller: Option<Arc<dyn FrontendController>>,
    keyboard_enhancement_enabled: bool,
}

impl Ui {
    pub fn init_with_view_config(
        ui_config: UiViewConfig,
        supervisor_model: String,
    ) -> io::Result<Self> {
        Self::init_with_runtime_config(UiRuntimeConfig::from_view_config(
            ui_config,
            supervisor_model,
        ))
    }

    fn init_with_runtime_config(runtime_config: UiRuntimeConfig) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let keyboard_enhancement_enabled =
            match crossterm::terminal::supports_keyboard_enhancement() {
                Ok(true) => stdout
                    .execute(crossterm::event::PushKeyboardEnhancementFlags(
                        crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                            | crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
                    ))
                    .is_ok(),
                _ => false,
            };
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            runtime_config,
            frontend_controller: None,
            keyboard_enhancement_enabled,
        })
    }

    pub fn with_frontend_controller(
        mut self,
        controller: Arc<dyn FrontendController>,
    ) -> Self {
        self.frontend_controller = Some(controller);
        self
    }

    pub fn run(&mut self, status: &str, projection: &ProjectionState) -> io::Result<()> {
        const FULL_REDRAW_INTERVAL: Duration = Duration::from_secs(1);

        let mut shell_state = UiShellState::new_with_integrations_and_config(
            status.to_owned(),
            projection.clone(),
            None,
            None,
            None,
            None,
            self.runtime_config.clone(),
        );
        shell_state.set_frontend_controller(self.frontend_controller.clone());
        shell_state.set_frontend_terminal_streaming_enabled(self.frontend_controller.is_some());
        let mut force_draw = true;
        let mut last_animation_frame = Instant::now();
        let mut last_full_redraw_at = Instant::now();
        let mut cached_ui_state: Option<UiState> = None;
        let mut frontend_event_receiver =
            spawn_frontend_event_bridge(self.frontend_controller.clone());
        loop {
            let now = Instant::now();
            let mut changed = false;
            changed |= shell_state.poll_frontend_events_and_report(
                frontend_event_receiver.as_mut(),
            );
            changed |= shell_state.drain_async_events_and_report();
            changed |= shell_state.run_due_periodic_tasks_and_report(now);
            changed |= shell_state.maintain_active_terminal_view_and_report();
            changed |= shell_state.ensure_startup_session_feed_opened_and_report();
            if changed {
                shell_state.invalidate_draw_caches();
            }

            shell_state.maybe_emit_projection_perf_log(now);
            let animation_state = shell_state.animation_state(now);
            let animation_frame_interval = match animation_state {
                AnimationState::ActiveTurn => Some(Duration::from_millis(200)),
                AnimationState::ResolvedOnly => Some(Duration::from_millis(1_000)),
                AnimationState::None => None,
            };
            let animation_frame_ready = animation_frame_interval
                .map(|interval| now.duration_since(last_animation_frame) >= interval)
                .unwrap_or(false);
            let full_redraw_interval = FULL_REDRAW_INTERVAL;
            let full_redraw_due = now.duration_since(last_full_redraw_at) >= full_redraw_interval;
            let should_draw = force_draw
                || changed
                || full_redraw_due
                || (animation_frame_interval.is_some() && animation_frame_ready);
            let should_refresh_ui_state = changed
                || cached_ui_state.is_none()
                || full_redraw_due
                || matches!(animation_state, AnimationState::ResolvedOnly) && animation_frame_ready;

            if should_draw {
                if animation_frame_ready {
                    last_animation_frame = now;
                }
                if full_redraw_due {
                    self.terminal.clear()?;
                    shell_state.invalidate_draw_caches();
                    cached_ui_state = None;
                }
                let ui_state = if should_refresh_ui_state {
                    let state = shell_state.ui_state_for_draw(now);
                    cached_ui_state = Some(state.clone());
                    state
                } else {
                    cached_ui_state
                        .clone()
                        .expect("cached UI state should exist for redraw")
                };
                self.terminal.draw(|frame| {
                    let area = frame.area();
                    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(4)]);
                    let [main, footer] = layout.areas(area);
                    let main_layout = Layout::horizontal([
                        Constraint::Percentage(35),
                        Constraint::Percentage(65),
                    ]);
                    let [left_area, center_area] = main_layout.areas(main);
                    let left_layout =
                        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]);
                    let [sessions_area, inbox_area] = left_layout.areas(left_area);

                    let sessions_viewport_rows =
                        usize::from(sessions_area.height.saturating_sub(2).max(1));
                    let selected_session_index = shell_state.selected_session_index;
                    let selected_session_id_hint = shell_state.selected_session_id.clone();
                    let session_panel_scroll_line = shell_state.session_panel_scroll_line();
                    let (session_metrics, sessions_text) = {
                        let session_rows = shell_state.session_panel_rows_for_draw();
                        let selected_session_id = if session_rows.is_empty() {
                            None
                        } else if let Some(selected_session_id) = selected_session_id_hint.as_ref() {
                            if session_rows
                                .iter()
                                .any(|row| &row.session_id == selected_session_id)
                            {
                                Some(selected_session_id.clone())
                            } else {
                                let index = selected_session_index.unwrap_or(0).min(session_rows.len() - 1);
                                session_rows.get(index).map(|row| row.session_id.clone())
                            }
                        } else {
                            let index = selected_session_index.unwrap_or(0).min(session_rows.len() - 1);
                            session_rows.get(index).map(|row| row.session_id.clone())
                        };
                        let metrics =
                            session_panel_line_metrics_from_rows(session_rows, selected_session_id.as_ref());
                        let text = render_sessions_panel_text_virtualized_from_rows(
                            session_rows,
                            selected_session_id.as_ref(),
                            session_panel_scroll_line,
                            sessions_viewport_rows,
                        );
                        (metrics, text)
                    };
                    shell_state.sync_session_panel_viewport(
                        session_metrics.total_lines,
                        session_metrics.selected_line,
                        sessions_viewport_rows,
                    );
                    let sessions_block = Block::default().title("sessions").borders(Borders::ALL);
                    frame.render_widget(
                        Paragraph::new(sessions_text).block(sessions_block),
                        sessions_area,
                    );

                    let inbox_text = render_inbox_panel(&ui_state);
                    let inbox_block = Block::default().title("inbox").borders(Borders::ALL);
                    frame.render_widget(Paragraph::new(inbox_text).block(inbox_block), inbox_area);
                    if let Some(session_id) = shell_state.active_terminal_session_id().cloned() {
                        let (terminal_area, session_info_area) = if shell_state
                            .should_show_session_info_sidebar()
                            && center_area.width >= 80
                        {
                            let [terminal_area, sidebar_area] = Layout::horizontal([
                                Constraint::Percentage(68),
                                Constraint::Percentage(32),
                            ])
                            .areas(center_area);
                            (terminal_area, Some(sidebar_area))
                        } else {
                            (center_area, None)
                        };
                        let active_needs_input = shell_state.active_terminal_needs_input().cloned();
                        let terminal_input_height = terminal_input_pane_height(
                            terminal_area.height,
                            terminal_area.width,
                            active_needs_input.as_ref(),
                            &shell_state.terminal_compose_editor,
                        );
                        let center_layout = Layout::vertical([
                            Constraint::Length(3),
                            Constraint::Min(1),
                            Constraint::Length(terminal_input_height),
                        ]);
                        let [terminal_meta_area, terminal_output_area, terminal_input_area] =
                            center_layout.areas(terminal_area);
                        let terminal_view_state =
                            shell_state.terminal_session_states.get(&session_id);
                        let meta_text = render_terminal_top_bar(
                            &shell_state.domain,
                            &session_id,
                            terminal_view_state,
                        );
                        let terminal_meta_block =
                            Block::default().title("terminal").borders(Borders::ALL);
                        frame.render_widget(
                            Paragraph::new(meta_text).block(terminal_meta_block),
                            terminal_meta_area,
                        );

                        const TERMINAL_VIEWPORT_OVERSCAN_ROWS: usize = 8;
                        let output_width = terminal_output_area.width.saturating_sub(2).max(1);
                        let viewport_height = terminal_output_area.height.saturating_sub(2).max(1);
                        let indicator = terminal_activity_indicator(
                            &shell_state.domain,
                            &shell_state.terminal_session_states,
                            &session_id,
                        );
                        let total_rows = shell_state
                            .terminal_total_rendered_rows_for_session(
                                &session_id,
                                output_width,
                                indicator,
                            )
                            .max(1);
                        shell_state.sync_terminal_output_viewport(
                            total_rows,
                            usize::from(viewport_height),
                        );
                        let output_render = shell_state
                            .render_terminal_output_viewport_for_session(
                                &session_id,
                                TerminalViewportRequest {
                                    width: output_width,
                                    scroll_top: shell_state
                                        .terminal_session_states
                                        .get(&session_id)
                                        .map(|view| view.output_scroll_line)
                                        .unwrap_or(0),
                                    viewport_rows: usize::from(viewport_height),
                                    overscan_rows: TERMINAL_VIEWPORT_OVERSCAN_ROWS,
                                    indicator,
                                },
                            )
                            .unwrap_or_else(|| TerminalViewportRender {
                                text: Text::raw("No terminal output available yet."),
                                local_scroll_top: 0,
                            });
                        let terminal_output_block =
                            Block::default().title("output").borders(Borders::ALL);
                        frame.render_widget(
                            Paragraph::new(output_render.text)
                                .wrap(Wrap { trim: false })
                                .scroll((output_render.local_scroll_top, 0))
                                .block(terminal_output_block),
                            terminal_output_area,
                        );

                        if let Some(prompt) = active_needs_input.as_ref() {
                            render_terminal_needs_input_panel(
                                frame,
                                terminal_input_area,
                                &shell_state.domain,
                                &session_id,
                                prompt,
                                shell_state.mode == UiMode::Insert,
                            );
                        } else {
                            frame.render_widget(
                                EditorView::new(&mut shell_state.terminal_compose_editor)
                                    .theme(nord_editor_theme(
                                        Block::default()
                                            .title("input (Enter send, Shift+Enter newline)")
                                            .borders(Borders::ALL),
                                    ))
                                    .wrap(true),
                                terminal_input_area,
                            );
                        }

                        if let Some(sidebar_area) = session_info_area {
                            let sidebar_text = render_session_info_panel(
                                &shell_state.domain,
                                &session_id,
                                shell_state.session_info_diff_cache_for(&session_id),
                                shell_state.session_info_summary_cache_for(&session_id),
                                shell_state.session_ci_status_cache_for(&session_id),
                            );
                            let session_info_title =
                                session_info_sidebar_title(&shell_state.domain, &session_id);
                            frame.render_widget(
                                Paragraph::new(sidebar_text).block(
                                    Block::default()
                                        .title(session_info_title)
                                        .borders(Borders::ALL),
                                ),
                                sidebar_area,
                            );
                        }
                    } else {
                        let center_text = render_center_panel(&ui_state);
                        if shell_state.is_global_supervisor_chat_active() {
                            let [chat_output_area, chat_input_area] =
                                Layout::vertical([Constraint::Min(3), Constraint::Length(3)])
                                    .areas(center_area);
                            frame.render_widget(
                                Paragraph::new(center_text).block({
                                    Block::default()
                                        .title(ui_state.center_pane.title.as_str())
                                        .borders(Borders::ALL)
                                }),
                                chat_output_area,
                            );
                            shell_state.global_supervisor_chat_input.focused =
                                shell_state.mode == UiMode::Insert;
                            Input::new(&shell_state.global_supervisor_chat_input)
                                .label("draft (Enter send)")
                                .placeholder("Type supervisor query")
                                .render_stateful(frame, chat_input_area);
                        } else {
                            frame.render_widget(
                                Paragraph::new(center_text).block({
                                    Block::default()
                                        .title(ui_state.center_pane.title.as_str())
                                        .borders(Borders::ALL)
                                }),
                                center_area,
                            );
                        }
                    }

                    let footer_style = bottom_bar_style(shell_state.mode);
                    let footer_text = Text::from(vec![
                        Line::from(format!(
                            "status: {} | mode: {}",
                            ui_state.status,
                            shell_state.mode.label(),
                        )),
                        Line::from(mode_help(shell_state.mode)),
                    ]);
                    frame.render_widget(
                        Paragraph::new(footer_text).style(footer_style).block(
                            Block::default()
                                .title("shell")
                                .borders(Borders::ALL)
                                .border_style(footer_style)
                                .style(footer_style),
                        ),
                        footer,
                    );

                    if let Some(which_key) = shell_state.which_key_overlay.as_ref() {
                        render_which_key_overlay(frame, center_area, which_key);
                    }
                    if shell_state.ticket_picker_overlay.visible {
                        render_ticket_picker_overlay(
                            frame,
                            main,
                            &mut shell_state.ticket_picker_overlay,
                        );
                    }
                    if let Some(ticket) = shell_state
                        .ticket_picker_overlay
                        .archive_confirm_ticket
                        .as_ref()
                    {
                        render_ticket_archive_confirm_overlay(frame, main, ticket);
                    }
                    if let Some(session_id) = shell_state.archive_session_confirm_session.as_ref() {
                        render_archive_session_confirm_overlay(
                            frame,
                            main,
                            &shell_state.domain,
                            session_id,
                        );
                    }
                    if let Some(modal) = shell_state.worktree_diff_modal.as_ref() {
                        render_worktree_diff_modal(frame, main, &shell_state.domain, modal);
                    }
                    if let Some(session_id) = shell_state.review_merge_confirm_session.as_ref() {
                        render_review_merge_confirm_overlay(
                            frame,
                            main,
                            &shell_state.domain,
                            session_id,
                        );
                    }
                })?;
                if full_redraw_due {
                    last_full_redraw_at = now;
                }

                if shell_state.ticket_picker_overlay.has_repository_prompt()
                    || shell_state.terminal_needs_input_is_note_insert_mode()
                    || (shell_state.mode == UiMode::Insert
                        && shell_state.is_terminal_view_active())
                {
                    let _ = io::stdout()
                        .execute(Show)
                        .and_then(|stdout| stdout.execute(SetCursorStyle::BlinkingBlock));
                } else {
                    let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
                }
            }

            force_draw = false;
            const BUSY_EVENT_SCAN_INTERVAL: Duration = Duration::from_millis(50);
            const IDLE_EVENT_SCAN_INTERVAL: Duration = Duration::from_millis(100);
            let event_scan_interval = if matches!(animation_state, AnimationState::ActiveTurn)
                || shell_state.has_pending_async_activity()
            {
                BUSY_EVENT_SCAN_INTERVAL
            } else {
                IDLE_EVENT_SCAN_INTERVAL
            };
            let wake_deadline =
                shell_state.next_wake_deadline(
                    now,
                    animation_state,
                    last_animation_frame,
                );
            let poll_timeout = match wake_deadline {
                Some(deadline) if deadline <= now => Duration::from_millis(0),
                Some(deadline) => deadline.duration_since(now).min(event_scan_interval),
                None => event_scan_interval,
            };
            if event::poll(poll_timeout)? {
                match event::read()? {
                    Event::Key(key) => {
                        shell_state.invalidate_draw_caches();
                        if key.kind == KeyEventKind::Press && handle_key_press(&mut shell_state, key)
                        {
                            break;
                        }
                        force_draw = true;
                        cached_ui_state = None;
                    }
                    _ => {
                        force_draw = true;
                        cached_ui_state = None;
                    }
                }
            }
        }

        Ok(())
    }
}

fn spawn_frontend_event_bridge(
    controller: Option<Arc<dyn FrontendController>>,
) -> Option<mpsc::Receiver<FrontendEvent>> {
    let controller = controller?;
    let (sender, receiver) = mpsc::channel(128);
    match TokioHandle::try_current() {
        Ok(runtime) => {
            runtime.spawn(async move {
                match controller.snapshot().await {
                    Ok(snapshot) => {
                        let _ = sender.send(FrontendEvent::SnapshotUpdated(snapshot)).await;
                    }
                    Err(error) => {
                        let _ = sender
                            .send(FrontendEvent::Notification(FrontendNotification {
                                level: FrontendNotificationLevel::Warning,
                                message: sanitize_terminal_display_text(
                                    format!("frontend snapshot unavailable: {error}").as_str(),
                                ),
                                work_item_id: None,
                                session_id: None,
                            }))
                            .await;
                    }
                }

                let mut stream = match controller.subscribe().await {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = sender
                            .send(FrontendEvent::Notification(FrontendNotification {
                                level: FrontendNotificationLevel::Warning,
                                message: sanitize_terminal_display_text(
                                    format!("frontend subscription unavailable: {error}").as_str(),
                                ),
                                work_item_id: None,
                                session_id: None,
                            }))
                            .await;
                        return;
                    }
                };

                loop {
                    match stream.next_event().await {
                        Ok(Some(event)) => {
                            if sender.send(event).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender
                                .send(FrontendEvent::Notification(FrontendNotification {
                                    level: FrontendNotificationLevel::Warning,
                                    message: sanitize_terminal_display_text(
                                        format!("frontend subscription failed: {error}").as_str(),
                                    ),
                                    work_item_id: None,
                                    session_id: None,
                                }))
                                .await;
                            break;
                        }
                    }
                }
            });
            Some(receiver)
        }
        Err(_) => None,
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        if self.keyboard_enhancement_enabled {
            let _ = io::stdout().execute(crossterm::event::PopKeyboardEnhancementFlags);
        }
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn bottom_bar_style(mode: UiMode) -> Style {
    match mode {
        UiMode::Normal => Style::default()
            .fg(Color::Rgb(236, 239, 244))
            .bg(Color::Rgb(59, 66, 82)),
        UiMode::Insert => Style::default()
            .fg(Color::Rgb(236, 239, 244))
            .bg(Color::Rgb(76, 86, 106)),
        UiMode::Terminal => Style::default()
            .fg(Color::Rgb(236, 239, 244))
            .bg(Color::Rgb(46, 52, 64)),
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

    let mut wrote_lane_section = false;
    for surface in ui_state
        .inbox_batch_surfaces
        .iter()
        .filter(|surface| surface.total_count > 0)
    {
        if wrote_lane_section {
            lines.push(String::new());
        }
        lines.push(format!("{}:", surface.kind.heading_label()));
        wrote_lane_section = true;

        for (index, row) in ui_state.inbox_rows.iter().enumerate() {
            if row.batch_kind != surface.kind {
                continue;
            }
            let selected = if Some(index) == ui_state.selected_inbox_index {
                ">"
            } else {
                " "
            };
            let resolved = if row.resolved { "x" } else { " " };
            let display_title = sanitize_inbox_display_title(&row.title);
            lines.push(format!(
                "{selected} [{resolved}] {}",
                display_title
            ));
        }
    }

    lines.join("\n")
}

fn sanitize_inbox_display_title(title: &str) -> String {
    let trimmed = title.trim_start();
    let Some((raw_prefix, remainder)) = trimmed.split_once(':') else {
        return title.to_owned();
    };
    let prefix = raw_prefix.trim();
    let is_status_prefix = [
        "NeedsDecision",
        "NeedsReview",
        "NeedsApproval",
        "ReadyForReview",
        "Blocked",
        "FYI",
    ]
    .iter()
    .any(|candidate| prefix.eq_ignore_ascii_case(candidate));

    if !is_status_prefix {
        return title.to_owned();
    }

    remainder.trim_start().to_owned()
}

fn session_panel_rows(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
) -> Vec<SessionPanelRow> {
    let work_item_repo = work_item_repository_labels(domain);
    let ticket_labels = ticket_labels_by_ticket_id(domain);
    session_panel_rows_with_labels(
        domain,
        terminal_session_states,
        &work_item_repo,
        &ticket_labels,
    )
}

fn session_panel_rows_with_labels(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    work_item_repo: &HashMap<WorkItemId, String>,
    ticket_labels: &HashMap<String, String>,
) -> Vec<SessionPanelRow> {
    let mut rows = Vec::new();
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

        let project = project_label_for_session(session, domain, &work_item_repo);
        let ticket = ticket_label_for_session(session, domain, &ticket_labels);
        let badge = workflow_badge_for_session(session, domain);
        let group = session_state_group_for_session(session, domain, badge.as_str());
        let activity = if session_waiting_for_planning_needs_input(
            domain,
            terminal_session_states,
            &session.id,
        ) {
            SessionRowActivity::Idle
        } else if session_turn_is_running(terminal_session_states, session) {
            SessionRowActivity::Active
        } else {
            SessionRowActivity::Idle
        };
        rows.push(SessionPanelRow {
            session_id: session.id.clone(),
            project,
            group,
            ticket_label: ticket,
            badge,
            activity,
        });
    }

    rows.sort_unstable_by(|left, right| {
        left.project
            .cmp(&right.project)
            .then_with(|| left.group.sort_key().cmp(&right.group.sort_key()))
            .then_with(|| {
                left.ticket_label
                    .to_ascii_lowercase()
                    .cmp(&right.ticket_label.to_ascii_lowercase())
            })
            .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
    });

    rows
}

#[cfg(test)]
fn render_sessions_panel(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    selected_session_id: Option<&WorkerSessionId>,
) -> String {
    let rendered = render_sessions_panel_text(domain, terminal_session_states, selected_session_id);
    rendered
        .lines
        .iter()
        .map(render_plain_text_line)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
fn render_sessions_panel_text(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    selected_session_id: Option<&WorkerSessionId>,
) -> Text<'static> {
    let session_rows = session_panel_rows(domain, terminal_session_states);
    render_sessions_panel_text_from_rows(&session_rows, selected_session_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionPanelLineMetrics {
    total_lines: usize,
    selected_line: Option<usize>,
}

fn session_panel_line_metrics_from_rows(
    session_rows: &[SessionPanelRow],
    selected_session_id: Option<&WorkerSessionId>,
) -> SessionPanelLineMetrics {
    if session_rows.is_empty() {
        return SessionPanelLineMetrics {
            total_lines: 1,
            selected_line: None,
        };
    }

    let mut total_lines = 0usize;
    let mut selected_line = None;
    let mut previous_project: Option<String> = None;
    let mut previous_group: Option<SessionStateGroup> = None;
    for row in session_rows {
        if previous_project.as_deref() != Some(row.project.as_str()) {
            if previous_project.is_some() {
                total_lines += 1;
            }
            total_lines += 1;
            previous_project = Some(row.project.clone());
            previous_group = None;
        }
        if previous_group.as_ref() != Some(&row.group) {
            total_lines += 1;
            previous_group = Some(row.group.clone());
        }
        if selected_session_id == Some(&row.session_id) {
            selected_line = Some(total_lines);
        }
        total_lines += 1;
    }

    SessionPanelLineMetrics {
        total_lines,
        selected_line,
    }
}

#[cfg(test)]
fn render_sessions_panel_text_from_rows(
    session_rows: &[SessionPanelRow],
    selected_session_id: Option<&WorkerSessionId>,
) -> Text<'static> {
    let metrics = session_panel_line_metrics_from_rows(session_rows, selected_session_id);
    render_sessions_panel_text_virtualized_from_rows(
        session_rows,
        selected_session_id,
        0,
        metrics.total_lines.max(1),
    )
}

fn render_sessions_panel_text_virtualized_from_rows(
    session_rows: &[SessionPanelRow],
    selected_session_id: Option<&WorkerSessionId>,
    scroll_line: usize,
    viewport_rows: usize,
) -> Text<'static> {
    if session_rows.is_empty() {
        return Text::from("No open sessions.");
    }

    let viewport_start = scroll_line;
    let viewport_end = viewport_start.saturating_add(viewport_rows.max(1));
    let mut lines = Vec::with_capacity(viewport_rows.max(1));
    let mut line_index = 0usize;
    let mut previous_project: Option<&str> = None;
    let mut previous_group: Option<&SessionStateGroup> = None;
    for row in session_rows {
        let is_selected = selected_session_id == Some(&row.session_id);
        let marker = if is_selected { ">" } else { " " };
        if previous_project != Some(row.project.as_str()) {
            if previous_project.is_some() {
                if (viewport_start..viewport_end).contains(&line_index) {
                    lines.push(Line::from(String::new()));
                }
                line_index += 1;
            }
            if (viewport_start..viewport_end).contains(&line_index) {
                lines.push(Line::from(format!("{}:", row.project)));
            }
            line_index += 1;
            previous_project = Some(row.project.as_str());
            previous_group = None;
        }
        if previous_group != Some(&row.group) {
            if (viewport_start..viewport_end).contains(&line_index) {
                lines.push(Line::from(format!("  {}:", row.group.display_label())));
            }
            line_index += 1;
            previous_group = Some(&row.group);
        }
        let indicator = if row.activity == SessionRowActivity::Active {
            loading_spinner_frame()
        } else {
            "•"
        };
        let status_chip = row.activity.status_chip();
        let status_style = row.activity.style();
        let mut spans = vec![
            Span::raw(marker),
            Span::raw("    "),
            Span::styled(format!("{indicator} "), status_style),
            Span::styled(format!("{status_chip} "), status_style),
        ];
        if row.group.is_other() {
            spans.push(Span::raw(format!("[{}] ", row.badge)));
        }
        spans.push(Span::raw(row.ticket_label.clone()));
        if (viewport_start..viewport_end).contains(&line_index) {
            lines.push(Line::from(spans));
        }
        line_index += 1;
    }

    Text::from(lines)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionPanelRow {
    session_id: WorkerSessionId,
    project: String,
    group: SessionStateGroup,
    ticket_label: String,
    badge: String,
    activity: SessionRowActivity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionStateGroup {
    Planning,
    Implementation,
    Review,
    Other(String),
}

impl SessionStateGroup {
    fn sort_key(&self) -> (u8, &str) {
        match self {
            Self::Planning => (0, ""),
            Self::Implementation => (1, ""),
            Self::Review => (2, ""),
            Self::Other(label) => (3, label.as_str()),
        }
    }

    fn display_label(&self) -> &'static str {
        match self {
            Self::Planning => "Planning",
            Self::Implementation => "Implementation",
            Self::Review => "Review",
            Self::Other(_) => "Other",
        }
    }

    fn is_other(&self) -> bool {
        matches!(self, Self::Other(_))
    }
}

#[cfg(test)]
fn render_plain_text_line(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionRowActivity {
    Active,
    Idle,
}

impl SessionRowActivity {
    fn status_chip(self) -> &'static str {
        match self {
            Self::Active => "[active]",
            Self::Idle => "[idle]",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Active => Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
            Self::Idle => Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
        }
    }
}

fn render_center_panel(ui_state: &UiState) -> String {
    let mut lines = Vec::with_capacity(ui_state.center_pane.lines.len() + 2);
    lines.push(format!("Stack: {}", ui_state.center_stack_label()));
    lines.push(String::new());
    lines.extend(ui_state.center_pane.lines.iter().cloned());
    lines.join("\n")
}

fn render_session_info_panel(
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    diff_cache: Option<&SessionInfoDiffCache>,
    summary_cache: Option<&SessionInfoSummaryCache>,
    ci_cache: Option<&SessionCiStatusCache>,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("session: {}", session_id.as_str()));

    let session = domain.sessions.get(session_id);
    let work_item = session
        .and_then(|entry| entry.work_item_id.as_ref())
        .and_then(|work_item_id| domain.work_items.get(work_item_id));

    lines.push(String::new());
    lines.push("PR:".to_owned());
    if let Some(work_item_id) = work_item.map(|entry| &entry.id) {
        let pr = collect_work_item_artifacts(work_item_id, domain, 1, is_pr_artifact)
            .into_iter()
            .next();
        if let Some(pr_artifact) = pr {
            lines.push(format!(
                "- {}",
                compact_focus_card_text(pr_artifact.uri.as_str())
            ));
            if let Some(metadata) = pr_metadata_summary_line(pr_artifact) {
                lines.push(format!("  {}", compact_focus_card_text(metadata.as_str())));
            }
        } else {
            lines.push("- No PR artifact available.".to_owned());
        }
    } else {
        lines.push("- No work item context.".to_owned());
    }

    lines.push(String::new());
    lines.push("File changes:".to_owned());
    match diff_cache {
        Some(cache) if cache.loading => {
            lines.push("- Loading diff...".to_owned());
        }
        Some(cache) if cache.error.is_some() => {
            lines.push(format!(
                "- Diff unavailable: {}",
                compact_focus_card_text(cache.error.as_deref().unwrap_or_default())
            ));
        }
        Some(cache) => {
            let files = parse_diff_file_summaries(cache.content.as_str());
            if files.is_empty() {
                lines.push("- No changed files.".to_owned());
            } else {
                for file in files {
                    lines.push(format!("- {} +{} -{}", file.path, file.added, file.removed));
                }
            }
        }
        None => lines.push("- Diff not requested yet.".to_owned()),
    }

    lines.push(String::new());
    lines.push("Ticket:".to_owned());
    if let Some(work_item) = work_item {
        if let Some(ticket_id) = work_item.ticket_id.as_ref() {
            if let Some(metadata) = latest_ticket_metadata(domain, ticket_id) {
                lines.push(format!(
                    "- {} {}",
                    metadata.identifier,
                    compact_focus_card_text(metadata.title.as_str())
                ));
                let description = metadata
                    .description
                    .or_else(|| latest_ticket_description(domain, ticket_id))
                    .unwrap_or_else(|| "No description synced.".to_owned());
                lines.push(format!(
                    "- {}",
                    compact_focus_card_text(description.as_str())
                ));
            } else {
                lines.push("- Ticket details unavailable.".to_owned());
            }
        } else {
            lines.push("- No ticket mapped.".to_owned());
        }
    } else {
        lines.push("- No work item context.".to_owned());
    }

    lines.push(String::new());
    lines.push("Open inbox:".to_owned());
    if let Some(work_item) = work_item {
        let mut has_open = false;
        for inbox_item_id in &work_item.inbox_items {
            let Some(item) = domain.inbox_items.get(inbox_item_id) else {
                continue;
            };
            if item.resolved {
                continue;
            }
            has_open = true;
            let display_title = sanitize_inbox_display_title(item.title.as_str());
            lines.push(format!(
                "- {}",
                compact_focus_card_text(display_title.as_str())
            ));
        }
        if !has_open {
            lines.push("- None.".to_owned());
        }
    } else {
        lines.push("- No work item context.".to_owned());
    }

    lines.push(String::new());
    lines.push("CI status:".to_owned());
    match ci_cache {
        Some(cache) if cache.error.is_some() => lines.push(format!(
            "- CI status unavailable: {}",
            compact_focus_card_text(cache.error.as_deref().unwrap_or_default())
        )),
        Some(cache) if cache.checks.is_empty() => {
            lines.push("- No required CI checks reported.".to_owned())
        }
        Some(cache) => {
            for check in &cache.checks {
                let workflow_prefix = check
                    .workflow
                    .as_deref()
                    .map(str::trim)
                    .filter(|workflow| !workflow.is_empty())
                    .map(|workflow| format!("{workflow} / "))
                    .unwrap_or_default();
                let bucket = check.bucket.trim();
                let status_label = if bucket.is_empty() {
                    "unknown".to_owned()
                } else {
                    bucket.to_ascii_uppercase()
                };
                lines.push(format!(
                    "- [{status_label}] {workflow_prefix}{}",
                    compact_focus_card_text(check.name.as_str())
                ));
                if let Some(link) = check.link.as_deref() {
                    lines.push(format!("  {}", compact_focus_card_text(link)));
                }
            }
        }
        None => lines.push("- No CI status polled yet.".to_owned()),
    }

    lines.push(String::new());
    lines.push("Summary:".to_owned());
    match summary_cache {
        Some(cache) if cache.loading => lines.push("- Updating...".to_owned()),
        Some(cache) if cache.error.is_some() => lines.push(format!(
            "- {}",
            compact_focus_card_text(cache.error.as_deref().unwrap_or_default())
        )),
        Some(cache) => lines.push(format!(
            "- {}",
            compact_focus_card_text(cache.text.as_deref().unwrap_or("No summary yet."))
        )),
        None => lines.push("- No summary yet.".to_owned()),
    }

    lines.join("\n")
}

fn render_terminal_top_bar(
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    terminal_view_state: Option<&TerminalViewState>,
) -> String {
    let labels = session_display_labels(domain, session_id);
    let mut text = if let Some(session) = domain.sessions.get(session_id) {
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
            "ticket: {} | status: {} | checkpoint: {}",
            labels.compact_label, status, checkpoint
        )
    } else {
        format!(
            "ticket: {} | status: unavailable | checkpoint: none",
            labels.compact_label
        )
    };
    if let Some(view) = terminal_view_state {
        if view.transcript_truncated {
            text.push_str(
                format!(
                    " | history: truncated (-{} lines)",
                    view.transcript_truncated_line_count
                )
                .as_str(),
            );
        }
    }
    sanitize_terminal_display_text(text.as_str())
}

fn render_terminal_transcript_lines(state: &TerminalViewState) -> Vec<String> {
    let mut lines = Vec::with_capacity(
        state.entries.len().saturating_mul(2) + usize::from(!state.output_fragment.is_empty()),
    );
    render_terminal_transcript_lines_into(state, &mut lines);
    lines
}

fn render_terminal_transcript_line_count(state: &TerminalViewState) -> usize {
    let mut count = 0usize;
    let mut previous_is_foldable = false;
    let mut previous_rendered_non_empty = false;
    for entry in &state.entries {
        let current_is_foldable = matches!(entry, TerminalTranscriptEntry::Foldable(_));
        if count > 0 && (previous_is_foldable || current_is_foldable) {
            count += 1;
            previous_rendered_non_empty = false;
        }
        match entry {
            TerminalTranscriptEntry::Message(line) => {
                let is_outgoing = is_user_outgoing_terminal_message(line);
                if is_outgoing && previous_rendered_non_empty {
                    count += 1;
                }
                count += 1;
                if is_outgoing {
                    count += 1;
                    previous_rendered_non_empty = false;
                } else {
                    previous_rendered_non_empty = !line.trim().is_empty();
                }
            }
            TerminalTranscriptEntry::Foldable(section) => {
                count += 1;
                if !section.folded {
                    count += section
                        .content
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count();
                }
                previous_rendered_non_empty = true;
            }
        }
        previous_is_foldable = current_is_foldable;
    }
    if !state.output_fragment.is_empty() {
        count += 1;
    }
    count
}

fn render_terminal_transcript_entries(state: &TerminalViewState) -> Vec<RenderedTerminalLine> {
    render_terminal_transcript_lines(state)
        .into_iter()
        .map(|text| RenderedTerminalLine { text })
        .collect()
}

fn render_terminal_transcript_lines_into(state: &TerminalViewState, lines: &mut Vec<String>) {
    let mut previous_is_foldable = false;
    for (index, entry) in state.entries.iter().enumerate() {
        let current_is_foldable = matches!(entry, TerminalTranscriptEntry::Foldable(_));
        if index > 0 && (previous_is_foldable || current_is_foldable) {
            lines.push(String::new());
        }
        match entry {
            TerminalTranscriptEntry::Message(line) => {
                if is_user_outgoing_terminal_message(line)
                    && lines
                        .last()
                        .map(|previous| !previous.trim().is_empty())
                        .unwrap_or(false)
                {
                    lines.push(String::new());
                }
                lines.push(line.clone());
                if is_user_outgoing_terminal_message(line) {
                    lines.push(String::new());
                }
            }
            TerminalTranscriptEntry::Foldable(section) => {
                let fold_marker = if section.folded { "[+]" } else { "[-]" };
                let summary = summarize_folded_terminal_content(section.content.as_str());
                lines.push(format!("  {fold_marker} {}: {summary}", section.kind.label()));
                if !section.folded {
                    for content_line in section.content.lines() {
                        let trimmed = content_line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        lines.push(format!("  {trimmed}"));
                    }
                }
            }
        }
        previous_is_foldable = current_is_foldable;
    }
    if !state.output_fragment.is_empty() {
        lines.push(state.output_fragment.clone());
    }
}

fn is_user_outgoing_terminal_message(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("> ") && !trimmed.starts_with("> system:")
}

fn summarize_folded_terminal_content(content: &str) -> String {
    let mut compact = String::with_capacity(content.len());
    for segment in content.split_whitespace() {
        if !compact.is_empty() {
            compact.push(' ');
        }
        compact.push_str(segment);
    }
    if compact.is_empty() {
        return "(no details)".to_owned();
    }
    const MAX_CHARS: usize = 96;
    if compact.chars().count() > MAX_CHARS {
        let truncated = compact.chars().take(MAX_CHARS).collect::<String>();
        format!("{truncated}...")
    } else {
        compact
    }
}

#[derive(Debug, Clone)]
struct TerminalViewportRequest {
    width: u16,
    scroll_top: usize,
    viewport_rows: usize,
    overscan_rows: usize,
    indicator: TerminalActivityIndicator,
}

#[derive(Debug, Clone)]
struct TerminalViewportRender {
    text: Text<'static>,
    local_scroll_top: u16,
}

fn terminal_total_rendered_rows(
    state: &mut TerminalViewState,
    width: u16,
    indicator: TerminalActivityIndicator,
    theme: UiTheme,
) -> usize {
    ensure_terminal_render_metrics(state, width.max(1), theme);
    let transcript_rows = state
        .render_cache
        .rendered_prefix_sums
        .last()
        .copied()
        .unwrap_or(0);
    transcript_rows + terminal_indicator_rows(transcript_rows > 0, indicator).len()
}

fn render_terminal_output_viewport(
    state: &mut TerminalViewState,
    request: TerminalViewportRequest,
    theme: UiTheme,
) -> TerminalViewportRender {
    let width = request.width.max(1);
    ensure_terminal_render_metrics(state, width, theme);
    let transcript_rows = state
        .render_cache
        .rendered_prefix_sums
        .last()
        .copied()
        .unwrap_or(0);
    let indicator_rows = terminal_indicator_rows(transcript_rows > 0, request.indicator);
    let total_rows = transcript_rows + indicator_rows.len();

    if total_rows == 0 {
        return TerminalViewportRender {
            text: Text::raw("No terminal output available yet."),
            local_scroll_top: 0,
        };
    }

    let max_scroll = total_rows.saturating_sub(request.viewport_rows.max(1));
    let scroll_top = request.scroll_top.min(max_scroll);
    let slice_start = scroll_top.saturating_sub(request.overscan_rows);
    let slice_end = scroll_top
        .saturating_add(request.viewport_rows.max(1))
        .saturating_add(request.overscan_rows)
        .min(total_rows);
    let local_scroll_top = scroll_top.saturating_sub(slice_start).min(u16::MAX as usize) as u16;

    let mut rendered = Vec::new();
    if transcript_rows > 0 && slice_start < transcript_rows {
        let transcript_slice_end = slice_end.min(transcript_rows);
        rendered.extend(render_terminal_transcript_slice(
            state,
            width,
            slice_start,
            transcript_slice_end,
            theme,
        ));
    }

    if slice_end > transcript_rows && !indicator_rows.is_empty() {
        let indicator_start = slice_start.saturating_sub(transcript_rows);
        let indicator_end = slice_end.saturating_sub(transcript_rows).min(indicator_rows.len());
        for row in indicator_rows
            .iter()
            .skip(indicator_start)
            .take(indicator_end.saturating_sub(indicator_start))
        {
            rendered.push(Line::from(Span::styled(row.text.clone(), row.style)));
        }
    }

    TerminalViewportRender {
        text: Text::from(rendered),
        local_scroll_top,
    }
}

#[cfg(test)]
fn render_terminal_output_with_accents(lines: &[String], width: u16, theme: UiTheme) -> Text<'static> {
    let mut rendered = Vec::with_capacity(lines.len().saturating_mul(2));
    let mut active_fold_kind: Option<TerminalFoldKind> = None;

    for raw_line in lines {
        let line = sanitize_terminal_display_text(raw_line);
        if line.trim().is_empty() {
            active_fold_kind = None;
            rendered.push(Line::from(String::new()));
            continue;
        }

        if let Some((kind, folded)) = parse_fold_header_line(&line) {
            active_fold_kind = if folded { None } else { Some(kind) };
            rendered.push(Line::from(Span::styled(
                line,
                fold_accent_style(kind, true),
            )));
            continue;
        }

        if let Some(kind) = active_fold_kind {
            rendered.push(Line::from(Span::styled(
                line,
                fold_accent_style(kind, false),
            )));
            continue;
        }

        if line.starts_with("> ") {
            rendered.push(Line::from(Span::styled(
                line,
                Style::default().fg(Color::LightBlue),
            )));
            continue;
        }

        if !line_looks_like_markdown(line.as_str()) {
            rendered.push(Line::from(line));
            continue;
        }

        let markdown = render_markdown_for_terminal(line.as_str(), width, theme);
        if markdown.lines.is_empty() {
            rendered.push(Line::from(String::new()));
        } else {
            rendered.extend(markdown.lines.into_iter());
        }
    }

    Text::from(rendered)
}

fn ensure_terminal_render_metrics(state: &mut TerminalViewState, width: u16, theme: UiTheme) {
    ensure_terminal_transcript_cache(state);
    if state.render_cache.width != width {
        state.render_cache.metrics_stale = true;
        state.render_cache.width = width;
    }
    if !state.render_cache.metrics_stale {
        return;
    }

    let mut active_fold_kind: Option<TerminalFoldKind> = None;
    state.render_cache.rendered_row_counts.clear();
    state
        .render_cache
        .rendered_row_counts
        .reserve(state.render_cache.transcript_lines.len());
    for line in &state.render_cache.transcript_lines {
        let row_count = terminal_rendered_row_count_for_line(
            line,
            width,
            &mut active_fold_kind,
            theme,
        );
        state.render_cache.rendered_row_counts.push(row_count.max(1));
    }

    state.render_cache.rendered_prefix_sums.clear();
    state
        .render_cache
        .rendered_prefix_sums
        .reserve(state.render_cache.rendered_row_counts.len() + 1);
    state.render_cache.rendered_prefix_sums.push(0);
    for row_count in &state.render_cache.rendered_row_counts {
        let next = state
            .render_cache
            .rendered_prefix_sums
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(*row_count);
        state.render_cache.rendered_prefix_sums.push(next);
    }
    state.render_cache.metrics_stale = false;
}

fn ensure_terminal_transcript_cache(state: &mut TerminalViewState) {
    if !state.render_cache.transcript_stale {
        return;
    }
    state.render_cache.transcript_lines = render_terminal_transcript_entries(state)
        .into_iter()
        .map(|line| line.text)
        .collect();
    state.render_cache.transcript_stale = false;
    state.render_cache.metrics_stale = true;
}

fn terminal_rendered_row_count_for_line(
    raw_line: &str,
    width: u16,
    active_fold_kind: &mut Option<TerminalFoldKind>,
    theme: UiTheme,
) -> usize {
    let line = sanitize_terminal_display_text(raw_line);
    if line.trim().is_empty() {
        *active_fold_kind = None;
        return 1;
    }

    if let Some((kind, folded)) = parse_fold_header_line(&line) {
        *active_fold_kind = if folded { None } else { Some(kind) };
        return 1;
    }

    if active_fold_kind.is_some() || line.starts_with("> ") || !line_looks_like_markdown(&line) {
        return 1;
    }

    let markdown = render_markdown_for_terminal(line.as_str(), width, theme);
    markdown.lines.len().max(1)
}

fn render_terminal_transcript_slice(
    state: &TerminalViewState,
    width: u16,
    start_row: usize,
    end_row: usize,
    theme: UiTheme,
) -> Vec<Line<'static>> {
    if start_row >= end_row || state.render_cache.transcript_lines.is_empty() {
        return Vec::new();
    }
    let prefix = &state.render_cache.rendered_prefix_sums;
    let max_row = prefix.last().copied().unwrap_or(0);
    if start_row >= max_row {
        return Vec::new();
    }

    let clamped_end = end_row.min(max_row);
    let mut raw_start = prefix.partition_point(|&row| row <= start_row).saturating_sub(1);
    raw_start = raw_start.min(state.render_cache.transcript_lines.len().saturating_sub(1));
    let mut raw_end = prefix.partition_point(|&row| row < clamped_end);
    raw_end = raw_end.min(state.render_cache.transcript_lines.len());

    let mut active_fold_kind: Option<TerminalFoldKind> = None;
    for line in state.render_cache.transcript_lines.iter().take(raw_start) {
        let sanitized = sanitize_terminal_display_text(line);
        if sanitized.trim().is_empty() {
            active_fold_kind = None;
            continue;
        }
        if let Some((kind, folded)) = parse_fold_header_line(&sanitized) {
            active_fold_kind = if folded { None } else { Some(kind) };
        }
    }

    let mut rendered = Vec::new();
    for raw_index in raw_start..raw_end {
        let line_start = prefix.get(raw_index).copied().unwrap_or(0);
        let line_end = prefix.get(raw_index + 1).copied().unwrap_or(line_start);
        if line_end <= start_row || line_start >= clamped_end {
            continue;
        }

        let line = &state.render_cache.transcript_lines[raw_index];
        let mut expanded =
            render_terminal_line_with_context(line, width, &mut active_fold_kind, theme);
        let local_start = start_row.saturating_sub(line_start);
        let local_end = clamped_end.saturating_sub(line_start).min(expanded.len());
        if local_start < local_end {
            rendered.extend(expanded.drain(local_start..local_end));
        }
    }

    rendered
}

fn render_terminal_line_with_context(
    raw_line: &str,
    width: u16,
    active_fold_kind: &mut Option<TerminalFoldKind>,
    theme: UiTheme,
) -> Vec<Line<'static>> {
    let line = sanitize_terminal_display_text(raw_line);
    if line.trim().is_empty() {
        *active_fold_kind = None;
        return vec![Line::from(String::new())];
    }

    if let Some((kind, folded)) = parse_fold_header_line(&line) {
        *active_fold_kind = if folded { None } else { Some(kind) };
        return vec![Line::from(Span::styled(line, fold_accent_style(kind, true)))];
    }

    if let Some(kind) = *active_fold_kind {
        return vec![Line::from(Span::styled(
            line,
            fold_accent_style(kind, false),
        ))];
    }

    if line.starts_with("> ") {
        return vec![Line::from(Span::styled(
            line,
            Style::default().fg(Color::LightBlue),
        ))];
    }

    if !line_looks_like_markdown(line.as_str()) {
        return vec![Line::from(line)];
    }

    let markdown = render_markdown_for_terminal(line.as_str(), width, theme);
    if markdown.lines.is_empty() {
        vec![Line::from(String::new())]
    } else {
        markdown.lines
    }
}

#[derive(Debug, Clone)]
struct TerminalIndicatorRow {
    text: String,
    style: Style,
}

fn terminal_indicator_rows(
    transcript_has_content: bool,
    indicator: TerminalActivityIndicator,
) -> Vec<TerminalIndicatorRow> {
    if matches!(indicator, TerminalActivityIndicator::None) {
        return Vec::new();
    }

    let mut rows = Vec::with_capacity(2);
    if transcript_has_content {
        rows.push(TerminalIndicatorRow {
            text: String::new(),
            style: Style::default(),
        });
    }
    match indicator {
        TerminalActivityIndicator::None => {}
        TerminalActivityIndicator::Working => rows.push(TerminalIndicatorRow {
            text: format!("{} agent working...", loading_spinner_frame()),
            style: Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::DIM),
        }),
        TerminalActivityIndicator::AwaitingInput => rows.push(TerminalIndicatorRow {
            text: "󰥔 awaiting input".to_owned(),
            style: Style::default().fg(Color::Yellow),
        }),
    }
    rows
}

fn line_looks_like_markdown(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#')
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("> ")
        || trimmed.starts_with("```")
        || trimmed.starts_with('`')
        || trimmed.contains("**")
        || trimmed.contains("__")
        || trimmed.contains("`")
        || (trimmed.contains('[') && trimmed.contains("]("))
}

fn parse_fold_header_line(line: &str) -> Option<(TerminalFoldKind, bool)> {
    let trimmed = line.trim_start();
    let without_cursor = trimmed.strip_prefix("=> ").unwrap_or(trimmed);
    let (folded, remainder) = if let Some(rest) = without_cursor.strip_prefix("[+]") {
        (true, rest)
    } else if let Some(rest) = without_cursor.strip_prefix("[-]") {
        (false, rest)
    } else {
        return None;
    };

    let remainder = remainder.trim_start();
    for kind in [
        TerminalFoldKind::Reasoning,
        TerminalFoldKind::FileChange,
        TerminalFoldKind::ToolCall,
        TerminalFoldKind::CommandExecution,
        TerminalFoldKind::Other,
    ] {
        let prefix = format!("{}:", kind.label());
        if remainder.starts_with(prefix.as_str()) {
            return Some((kind, folded));
        }
    }
    None
}

fn fold_accent_style(kind: TerminalFoldKind, header: bool) -> Style {
    let color = match kind {
        TerminalFoldKind::Reasoning => Color::Rgb(163, 190, 140),
        TerminalFoldKind::FileChange => Color::Rgb(208, 135, 112),
        TerminalFoldKind::ToolCall => Color::Rgb(180, 142, 173),
        TerminalFoldKind::CommandExecution => Color::Rgb(129, 161, 193),
        TerminalFoldKind::Other => Color::Rgb(143, 188, 187),
    };
    let mut style = Style::default().fg(color);
    if header {
        style = style.add_modifier(Modifier::BOLD);
    } else {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalActivityIndicator {
    None,
    Working,
    AwaitingInput,
}

fn terminal_activity_indicator(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> TerminalActivityIndicator {
    if suppress_working_indicator_for_planning_prompt(domain, terminal_session_states, session_id) {
        return TerminalActivityIndicator::None;
    }
    if terminal_session_is_running(domain, terminal_session_states, session_id) {
        return TerminalActivityIndicator::Working;
    }
    if terminal_session_is_awaiting_input(domain, terminal_session_states, session_id) {
        return TerminalActivityIndicator::AwaitingInput;
    }
    TerminalActivityIndicator::None
}

fn terminal_session_is_running(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    if !matches!(
        domain
            .sessions
            .get(session_id)
            .and_then(|session| session.status.as_ref()),
        Some(WorkerSessionStatus::Running)
    ) {
        return false;
    }
    session_turn_is_active(terminal_session_states, session_id)
}

fn suppress_working_indicator_for_planning_prompt(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    session_waiting_for_planning_needs_input(domain, terminal_session_states, session_id)
}

fn session_waiting_for_planning_needs_input(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    if !session_is_in_workflow_state(domain, session_id, WorkflowState::Planning) {
        return false;
    }
    terminal_session_states
        .get(session_id)
        .map(|state| {
            state.active_needs_input.is_some() || !state.pending_needs_input_prompts.is_empty()
        })
        .unwrap_or(false)
}

fn session_is_in_workflow_state(
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    expected_state: WorkflowState,
) -> bool {
    domain
        .sessions
        .get(session_id)
        .and_then(|session| session.work_item_id.as_ref())
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.workflow_state.as_ref())
        == Some(&expected_state)
}

fn terminal_session_is_awaiting_input(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    if session_turn_is_active(terminal_session_states, session_id) {
        return false;
    }
    let waiting_for_user = matches!(
        domain
            .sessions
            .get(session_id)
            .and_then(|session| session.status.as_ref()),
        Some(WorkerSessionStatus::WaitingForUser)
    );
    waiting_for_user
        && terminal_session_states
            .get(session_id)
            .and_then(|state| state.active_needs_input.as_ref())
            .is_some()
}

fn session_turn_is_active(
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    terminal_session_states
        .get(session_id)
        .map(|state| state.turn_active)
        .unwrap_or(false)
}

fn loading_spinner_frame() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let elapsed_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as usize)
        .unwrap_or(0);
    let index = (elapsed_ms / 200) % FRAMES.len();
    FRAMES[index]
}

fn render_markdown_for_terminal(input: &str, width: u16, theme: UiTheme) -> Text<'static> {
    if input.is_empty() {
        return Text::raw(String::new());
    }

    let source = preprocess_markdown_layout(input);
    let lines = terminal_markdown_skin(theme).parse(RatSkin::parse_text(source.as_str()), width);
    if lines.is_empty() {
        Text::raw(String::new())
    } else {
        Text::from(
            lines
                .into_iter()
                .map(|line| {
                    Line::from(
                        line.spans
                            .into_iter()
                            .map(|span| Span::styled(span.content.into_owned(), span.style))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    }
}

fn preprocess_markdown_layout(input: &str) -> String {
    let mut output = Vec::new();
    for raw_line in input.lines() {
        let line = raw_line.trim_end_matches('\r');
        if is_markdown_heading(line.trim_start()) {
            if output
                .last()
                .map(|entry: &String| !entry.trim().is_empty())
                .unwrap_or(false)
            {
                output.push(String::new());
            }
            output.push(line.to_owned());
            output.push(String::new());
            continue;
        }
        output.push(line.to_owned());
    }
    output.join("\n")
}

fn is_markdown_heading(line: &str) -> bool {
    let mut level = 0usize;
    for ch in line.chars() {
        if ch == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level == 0 || level > 6 {
        return false;
    }
    line.get(level..)
        .map(str::trim_start)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

fn terminal_markdown_skin(theme: UiTheme) -> &'static RatSkin {
    static DEFAULT_SKIN: OnceLock<RatSkin> = OnceLock::new();
    static NORD_SKIN: OnceLock<RatSkin> = OnceLock::new();
    let cache = match theme {
        UiTheme::Default => &DEFAULT_SKIN,
        UiTheme::Nord => &NORD_SKIN,
    };
    cache.get_or_init(|| {
        let mut skin = RatSkin::default();
        if matches!(theme, UiTheme::Nord) {
            apply_nord_markdown_theme(&mut skin);
        }
        skin.skin.paragraph.right_margin = 0;
        skin.skin.code_block.left_margin = 0;
        for header in &mut skin.skin.headers {
            header.left_margin = 0;
            header.right_margin = 0;
        }
        skin
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiTheme {
    Default,
    Nord,
}

fn ui_theme_from_config_value(value: &str) -> UiTheme {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "default" => UiTheme::Default,
        "nord" => UiTheme::Nord,
        _ => UiTheme::Nord,
    }
}

fn apply_nord_markdown_theme(skin: &mut RatSkin) {
    // Nord-inspired palette
    // Polar Night:  #2E3440 #3B4252 #434C5E #4C566A
    // Snow Storm:   #D8DEE9 #E5E9F0 #ECEFF4
    // Frost:        #8FBCBB #88C0D0 #81A1C1 #5E81AC
    // Aurora:       #BF616A #D08770 #EBCB8B #A3BE8C #B48EAD
    skin.skin.paragraph.set_fg((216, 222, 233).into()); // nord4
    skin.skin.bold.set_fg((236, 239, 244).into()); // nord6
    skin.skin.italic.set_fg((136, 192, 208).into()); // nord8
    skin.skin.strikeout.set_fg((180, 142, 173).into()); // nord15

    skin.skin.headers[0].set_fg((235, 203, 139).into()); // nord13
    skin.skin.headers[1].set_fg((143, 188, 187).into()); // nord7
    skin.skin.headers[2].set_fg((129, 161, 193).into()); // nord9
    skin.skin.headers[3].set_fg((136, 192, 208).into()); // nord8
    skin.skin.headers[4].set_fg((163, 190, 140).into()); // nord14
    skin.skin.headers[5].set_fg((208, 135, 112).into()); // nord12

    skin.skin
        .inline_code
        .set_fgbg((235, 203, 139).into(), (46, 52, 64).into()); // nord13 on nord0
    skin.skin
        .code_block
        .set_fgbg((236, 239, 244).into(), (59, 66, 82).into()); // nord6 on nord1

    skin.skin.bullet.set_fg((136, 192, 208).into()); // nord8
    skin.skin.quote_mark.set_fg((94, 129, 172).into()); // nord10
    skin.skin.horizontal_rule.set_fg((76, 86, 106).into()); // nord3
    skin.skin
        .table
        .compound_style
        .set_fg((129, 161, 193).into()); // nord9
}

fn nord_editor_theme<'a>(block: Block<'a>) -> EditorTheme<'a> {
    EditorTheme::default()
        .base(
            Style::default()
                .bg(Color::Rgb(46, 52, 64))
                .fg(Color::Rgb(216, 222, 233)),
        )
        .cursor_style(
            Style::default()
                .bg(Color::Rgb(236, 239, 244))
                .fg(Color::Rgb(46, 52, 64)),
        )
        .selection_style(
            Style::default()
                .bg(Color::Rgb(94, 129, 172))
                .fg(Color::Rgb(236, 239, 244)),
        )
        .line_numbers_style(
            Style::default()
                .bg(Color::Rgb(46, 52, 64))
                .fg(Color::Rgb(76, 86, 106)),
        )
        .block(block)
        .hide_status_line()
}

fn sanitize_terminal_display_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\t' => output.push(ch),
            '\r' => output.push('\n'),
            '\u{2400}' => {}
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

fn work_item_repository_labels(domain: &ProjectionState) -> HashMap<WorkItemId, String> {
    let mut work_item_repo = HashMap::new();
    for event in &domain.events {
        if let OrchestrationEventPayload::WorktreeCreated(payload) = &event.payload {
            work_item_repo.insert(
                payload.work_item_id.clone(),
                repository_name_from_path(payload.path.as_str()),
            );
        }
    }
    work_item_repo
}

fn ticket_labels_by_ticket_id(domain: &ProjectionState) -> HashMap<String, String> {
    let mut ticket_labels = HashMap::new();
    for event in &domain.events {
        if let OrchestrationEventPayload::TicketSynced(payload) = &event.payload {
            let label = ticket_label_from_synced_event(payload);
            ticket_labels.insert(payload.ticket_id.as_str().to_owned(), label);
        }
    }
    ticket_labels
}

fn ticket_label_from_synced_event(payload: &TicketSyncedPayload) -> String {
    let title = payload.title.trim();
    let identifier = payload.identifier.trim();
    if title.is_empty() {
        if identifier.is_empty() {
            payload.ticket_id.as_str().to_owned()
        } else {
            identifier.to_owned()
        }
    } else {
        title.to_owned()
    }
}

fn project_label_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    work_item_repo: &HashMap<WorkItemId, String>,
) -> String {
    session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.project_id.as_ref())
        .map(|project_id: &ProjectId| project_id.as_str().trim().to_owned())
        .filter(|project_id| !project_id.is_empty())
        .or_else(|| {
            session
                .work_item_id
                .as_ref()
                .and_then(|work_item_id| work_item_repo.get(work_item_id).cloned())
        })
        .unwrap_or_else(|| "unknown-repo".to_owned())
}

fn ticket_label_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    ticket_labels: &HashMap<String, String>,
) -> String {
    let fallback = format!("session {}", session.id.as_str());
    let Some(work_item_id) = session.work_item_id.as_ref() else {
        return fallback;
    };
    let Some(work_item) = domain.work_items.get(work_item_id) else {
        return fallback;
    };
    let Some(ticket_id) = work_item.ticket_id.as_ref() else {
        return fallback;
    };
    ticket_labels
        .get(ticket_id.as_str())
        .cloned()
        .or_else(|| {
            let fallback = ticket_id.as_str().trim();
            if fallback.is_empty() {
                None
            } else {
                Some(fallback.to_owned())
            }
        })
        .unwrap_or(fallback)
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

fn workflow_badge_for_session(session: &SessionProjection, domain: &ProjectionState) -> String {
    session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.workflow_state.as_ref())
        .map(workflow_state_to_badge_label)
        .or_else(|| {
            if matches!(session.status.as_ref(), Some(WorkerSessionStatus::Running)) {
                Some("planning".to_owned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| session_status_label(session.status.as_ref()).to_owned())
}

fn session_state_group_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    fallback_badge: &str,
) -> SessionStateGroup {
    let workflow_state = session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.workflow_state.as_ref());
    match workflow_state {
        Some(WorkflowState::New | WorkflowState::Planning) => SessionStateGroup::Planning,
        Some(WorkflowState::Implementing | WorkflowState::PRDrafted) => {
            SessionStateGroup::Implementation
        }
        Some(
            WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview
            | WorkflowState::PendingMerge,
        ) => SessionStateGroup::Review,
        Some(WorkflowState::Done | WorkflowState::Abandoned) => {
            SessionStateGroup::Other("complete".to_owned())
        }
        None => SessionStateGroup::Other(fallback_badge.to_owned()),
    }
}

fn session_turn_is_running(
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session: &SessionProjection,
) -> bool {
    matches!(session.status.as_ref(), Some(WorkerSessionStatus::Running))
        && session_turn_is_active(terminal_session_states, &session.id)
}

fn workflow_state_to_badge_label(state: &WorkflowState) -> String {
    match state {
        WorkflowState::New | WorkflowState::Planning => "planning",
        WorkflowState::Implementing | WorkflowState::PRDrafted => "implementation",
        WorkflowState::AwaitingYourReview
        | WorkflowState::ReadyForReview
        | WorkflowState::InReview => "review",
        WorkflowState::PendingMerge => "pending-merge",
        WorkflowState::Done | WorkflowState::Abandoned => "complete",
    }
    .to_owned()
}

fn render_ticket_picker_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    overlay: &mut TicketPickerOverlayState,
) {
    let content = render_ticket_picker_overlay_text(overlay);
    let Some(popup) = ticket_picker_popup(anchor_area) else {
        return;
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("start ticket").borders(Borders::ALL)),
        popup,
    );
    if popup.width > 4 && popup.height > 4 {
        let input_height: u16 = if overlay.new_ticket_mode {
            let max_input_height = popup.height.saturating_sub(3);
            if max_input_height < TICKET_PICKER_BRIEF_MIN_HEIGHT {
                return;
            }
            editor_widget_height(
                &overlay.new_ticket_brief_editor,
                popup.width.saturating_sub(2),
                TICKET_PICKER_BRIEF_MIN_HEIGHT,
                TICKET_PICKER_BRIEF_MAX_HEIGHT,
            )
            .min(max_input_height)
        } else {
            3
        };
        if popup.height <= input_height.saturating_add(1) {
            return;
        }
        let input_area = Rect {
            x: popup.x.saturating_add(1),
            y: popup
                .y
                .saturating_add(popup.height.saturating_sub(input_height.saturating_add(1))),
            width: popup.width.saturating_sub(2),
            height: input_height,
        };
        if overlay.new_ticket_mode {
            let project_area = Rect {
                x: popup.x.saturating_add(1),
                y: input_area.y.saturating_sub(1),
                width: popup.width.saturating_sub(2),
                height: 1,
            };
            let selected_project = overlay
                .selected_project_name()
                .unwrap_or_else(|| "No Project".to_owned());
            frame.render_widget(
                Paragraph::new(format!("Assigned project: {selected_project}")),
                project_area,
            );
            frame.render_widget(
                EditorView::new(&mut overlay.new_ticket_brief_editor)
                    .theme(nord_editor_theme(
                        Block::default()
                            .title("describe ticket")
                            .borders(Borders::ALL),
                    ))
                    .wrap(true),
                input_area,
            );
        } else if overlay.has_repository_prompt() {
            let mut state = overlay.repository_prompt_input.clone();
            state.focused = true;
            Input::new(&state)
                .label("path")
                .placeholder("repository path")
                .render_stateful(frame, input_area);
        }
    }
}

const TERMINAL_COMPOSE_MIN_HEIGHT: u16 = 6;
const TERMINAL_COMPOSE_MAX_HEIGHT: u16 = 14;
const TICKET_PICKER_BRIEF_MIN_HEIGHT: u16 = 4;
const TICKET_PICKER_BRIEF_MAX_HEIGHT: u16 = 10;
const INPUT_PANEL_OUTER_BORDER_HEIGHT: u16 = 2;
const EDITOR_WIDGET_BORDER_HEIGHT: u16 = 2;
const NEEDS_INPUT_NOTE_MIN_HEIGHT: u16 = 3;
const NEEDS_INPUT_NOTE_MAX_HEIGHT: u16 = 8;
const NEEDS_INPUT_HELP_HEIGHT: u16 = 1;
const NEEDS_INPUT_ERROR_HEIGHT: u16 = 1;
const NEEDS_INPUT_QUESTION_MIN_HEIGHT: u16 = 3;
const NEEDS_INPUT_QUESTION_MAX_EXPANDED_HEIGHT: u16 = 7;
const NEEDS_INPUT_QUESTION_MAX_DEFAULT_HEIGHT: u16 = 4;
const NEEDS_INPUT_CHOICE_MIN_ROWS_DEFAULT: usize = 1;
const NEEDS_INPUT_CHOICE_MAX_ROWS_DEFAULT: usize = 4;
const NEEDS_INPUT_CHOICE_MAX_ROWS_EXPANDED: usize = 12;

fn terminal_input_pane_height(
    center_height: u16,
    center_width: u16,
    prompt: Option<&NeedsInputComposerState>,
    compose_editor: &EditorState,
) -> u16 {
    let max_height = center_height.saturating_sub(4).max(3);

    let Some(prompt) = prompt else {
        let compose_height = editor_widget_height(
            compose_editor,
            center_width,
            TERMINAL_COMPOSE_MIN_HEIGHT,
            TERMINAL_COMPOSE_MAX_HEIGHT,
        );
        return compose_height.clamp(3, max_height);
    };
    let Some(question) = prompt.current_question() else {
        let compose_height = editor_widget_height(
            compose_editor,
            center_width,
            TERMINAL_COMPOSE_MIN_HEIGHT,
            TERMINAL_COMPOSE_MAX_HEIGHT,
        );
        return compose_height.clamp(3, max_height);
    };
    let is_plan_prompt = expanded_needs_input_layout_active(prompt);
    let question_height = needs_input_question_height(question, center_width, is_plan_prompt);
    let choice_height = needs_input_choice_height(question, center_width, is_plan_prompt);
    let note_height = editor_widget_height(
        &prompt.note_editor_state,
        center_width.saturating_sub(INPUT_PANEL_OUTER_BORDER_HEIGHT),
        NEEDS_INPUT_NOTE_MIN_HEIGHT,
        NEEDS_INPUT_NOTE_MAX_HEIGHT,
    );
    let mut inner_required = question_height
        .saturating_add(choice_height)
        .saturating_add(note_height)
        .saturating_add(NEEDS_INPUT_HELP_HEIGHT);
    if prompt.error.is_some() {
        inner_required = inner_required.saturating_add(NEEDS_INPUT_ERROR_HEIGHT);
    }
    let mut target_height = inner_required.saturating_add(INPUT_PANEL_OUTER_BORDER_HEIGHT);
    if is_plan_prompt {
        target_height = target_height.max(TERMINAL_COMPOSE_MIN_HEIGHT.saturating_mul(2));
    }
    target_height.clamp(3, max_height)
}

fn expanded_needs_input_layout_active(prompt: &NeedsInputComposerState) -> bool {
    if prompt.questions.len() > 1 {
        return true;
    }

    if prompt.prompt_id.to_ascii_lowercase().contains("plan") {
        return true;
    }

    prompt.questions.iter().any(|question| {
        let id_has_plan = question.id.to_ascii_lowercase().contains("plan");
        let header_has_plan = question.header.to_ascii_lowercase().contains("plan");
        let question_has_plan = question.question.to_ascii_lowercase().contains("plan");
        id_has_plan || header_has_plan || question_has_plan
    })
}

fn needs_input_question_height(
    question: &BackendNeedsInputQuestion,
    panel_width: u16,
    expanded_layout: bool,
) -> u16 {
    let question_text = format!("{} | {}", question.header, question.question);
    let content_width = panel_width.saturating_sub(6);
    let wrapped_rows = wrapped_row_count(question_text.as_str(), content_width);
    let max_height = if expanded_layout {
        NEEDS_INPUT_QUESTION_MAX_EXPANDED_HEIGHT
    } else {
        NEEDS_INPUT_QUESTION_MAX_DEFAULT_HEIGHT
    };
    wrapped_rows
        .saturating_add(2)
        .clamp(NEEDS_INPUT_QUESTION_MIN_HEIGHT, max_height)
}

fn needs_input_choice_height(
    question: &BackendNeedsInputQuestion,
    panel_width: u16,
    expanded_layout: bool,
) -> u16 {
    let content_width = panel_width.saturating_sub(10);
    let Some(options) = question
        .options
        .as_ref()
        .filter(|options| !options.is_empty())
    else {
        return 3;
    };

    let mut rows = 0usize;
    for option in options {
        let label = compact_focus_card_text(option.label.as_str());
        let line = if option.description.trim().is_empty() {
            format!("> [ ] {label}")
        } else {
            let description = compact_focus_card_text(option.description.as_str());
            format!("> [ ] {label} - {description}")
        };
        rows = rows.saturating_add(usize::from(wrapped_row_count(line.as_str(), content_width)));
    }
    let max_rows = if expanded_layout {
        NEEDS_INPUT_CHOICE_MAX_ROWS_EXPANDED
    } else {
        NEEDS_INPUT_CHOICE_MAX_ROWS_DEFAULT
    };
    let clamped_rows = rows.clamp(NEEDS_INPUT_CHOICE_MIN_ROWS_DEFAULT, max_rows);
    u16::try_from(clamped_rows)
        .unwrap_or(NEEDS_INPUT_CHOICE_MAX_EXPANDED_HEIGHT_U16)
        .saturating_add(2)
}

const NEEDS_INPUT_CHOICE_MAX_EXPANDED_HEIGHT_U16: u16 = 12;

fn editor_widget_height(
    state: &EditorState,
    widget_width: u16,
    min_height: u16,
    max_height: u16,
) -> u16 {
    let max_height = max_height.max(min_height);
    let content_width = widget_width.saturating_sub(EDITOR_WIDGET_BORDER_HEIGHT);
    let max_content_rows = usize::from(
        max_height
            .saturating_sub(EDITOR_WIDGET_BORDER_HEIGHT)
            .max(1),
    );
    let content_rows = editor_wrapped_row_count(state, content_width, max_content_rows);
    content_rows
        .saturating_add(EDITOR_WIDGET_BORDER_HEIGHT)
        .clamp(min_height, max_height)
}

fn editor_wrapped_row_count(state: &EditorState, content_width: u16, max_rows: usize) -> u16 {
    let width = usize::from(content_width.max(1));
    let max_rows = max_rows.max(1);
    let mut rows = 0usize;
    for row in state.lines.iter_row() {
        let chars = row.len();
        let wrapped = if chars == 0 { 1 } else { chars.div_ceil(width) };
        rows = rows.saturating_add(wrapped.max(1));
        if rows >= max_rows {
            rows = max_rows;
            break;
        }
    }
    u16::try_from(rows.max(1)).unwrap_or(u16::MAX)
}

fn wrapped_row_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let mut rows = 0usize;
    for segment in text.split('\n') {
        let chars = segment.chars().count();
        let wrapped = if chars == 0 { 1 } else { chars.div_ceil(width) };
        rows = rows.saturating_add(wrapped.max(1));
    }
    u16::try_from(rows).unwrap_or(u16::MAX)
}

fn render_terminal_needs_input_panel(
    frame: &mut ratatui::Frame<'_>,
    input_area: Rect,
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    prompt: &NeedsInputComposerState,
    focused: bool,
) {
    if input_area.height < 3 {
        return;
    }
    let Some(question) = prompt.current_question() else {
        return;
    };

    let labels = session_display_labels(domain, session_id);
    let title = format!(
        "input required | {} | {} / {}",
        labels.compact_label,
        prompt.current_question_index + 1,
        prompt.questions.len()
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(input_area);
    frame.render_widget(block, input_area);

    let options_len = question
        .options
        .as_ref()
        .map(|options| options.len())
        .unwrap_or(0);
    let is_plan_prompt = expanded_needs_input_layout_active(prompt);
    let question_height = needs_input_question_height(question, input_area.width, is_plan_prompt);
    let choice_height = needs_input_choice_height(question, input_area.width, is_plan_prompt);
    let note_height = editor_widget_height(
        &prompt.note_editor_state,
        inner.width,
        NEEDS_INPUT_NOTE_MIN_HEIGHT,
        NEEDS_INPUT_NOTE_MAX_HEIGHT,
    );

    let mut constraints = vec![
        Constraint::Length(question_height),
        Constraint::Min(choice_height),
        Constraint::Length(note_height),
    ];
    if prompt.error.is_some() {
        constraints.push(Constraint::Length(NEEDS_INPUT_ERROR_HEIGHT));
    }
    constraints.push(Constraint::Length(NEEDS_INPUT_HELP_HEIGHT));
    let sections = Layout::vertical(constraints).split(inner);
    let question_area = sections[0];
    let choice_area = sections[1];
    let note_area = sections[2];

    let header = format!("{} | {}", question.header, question.question);
    frame.render_widget(
        Paragraph::new(compact_focus_card_text(header.as_str()))
            .block(Block::default().title("question").borders(Borders::ALL)),
        question_area,
    );

    if let Some(options) = question
        .options
        .as_ref()
        .filter(|options| !options.is_empty())
    {
        let selected_index = prompt.select_state.selected_index;
        let highlighted_index = prompt
            .select_state
            .highlighted_index
            .min(options.len().saturating_sub(1));
        let visible_rows = usize::from(choice_area.height.saturating_sub(2)).max(1);
        let mut start = highlighted_index.saturating_sub(visible_rows / 2);
        if start + visible_rows > options.len() {
            start = options.len().saturating_sub(visible_rows);
        }
        let end = (start + visible_rows).min(options.len());

        let mut lines = Vec::new();
        for (idx, option) in options[start..end].iter().enumerate() {
            let option_index = start + idx;
            let cursor = if option_index == highlighted_index {
                ">"
            } else {
                " "
            };
            let selected = if selected_index == Some(option_index) {
                "[x]"
            } else {
                "[ ]"
            };
            let label = compact_focus_card_text(option.label.as_str());
            let line = if option.description.trim().is_empty() {
                format!("{cursor} {selected} {label}")
            } else {
                let description = compact_focus_card_text(option.description.as_str());
                format!("{cursor} {selected} {label} - {description}")
            };
            let style = if option_index == highlighted_index {
                Style::default().fg(Color::LightCyan)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(line, style)));
        }

        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().title("choices").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            choice_area,
        );
    } else {
        frame.render_widget(
            Paragraph::new("No predefined options. Enter a response note.")
                .block(Block::default().title("choices").borders(Borders::ALL)),
            choice_area,
        );
    }

    let mut note_editor_state = prompt.note_editor_state.clone();
    note_editor_state.mode = if prompt.interaction_active && prompt.note_insert_mode && focused {
        EditorMode::Insert
    } else {
        EditorMode::Normal
    };
    frame.render_widget(
        EditorView::new(&mut note_editor_state)
            .theme(nord_editor_theme(
                Block::default()
                    .title("note (optional)")
                    .borders(Borders::ALL),
            ))
            .wrap(true),
        note_area,
    );

    let mut index = 3usize;
    if let Some(error) = prompt.error.as_ref() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                compact_focus_card_text(error.as_str()),
                Style::default().fg(Color::LightRed),
            )),
            sections[index],
        );
        index += 1;
    }
    let help = if prompt.interaction_active {
        format!(
            "{}Enter: {} | h/l or arrows: question | i: note insert | Esc: normal mode",
            if options_len > 0 {
                "j/k: option | "
            } else {
                ""
            },
            if options_len > 0 {
                if prompt.has_next_question() {
                    "select option + next question"
                } else {
                    "select option + submit"
                }
            } else if prompt.has_next_question() {
                "next question"
            } else {
                "submit"
            },
        )
    } else {
        "Press i or use j/k + Enter to activate input | Esc: normal mode".to_owned()
    };
    frame.render_widget(
        Paragraph::new(help).wrap(Wrap { trim: false }),
        sections[index],
    );
}

fn render_worktree_diff_modal(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    domain: &ProjectionState,
    modal: &WorktreeDiffModalState,
) {
    let Some(popup) = worktree_diff_modal_popup(anchor_area) else {
        return;
    };

    let labels = session_display_labels(domain, &modal.session_id);
    let title = format!(
        "diff | ticket: {} | base: {}",
        labels.compact_label, modal.base_branch
    );
    frame.render_widget(Clear, popup);
    if modal.loading {
        frame.render_widget(
            Paragraph::new(Text::from(Line::from(Span::styled(
                "Loading diff...",
                Style::default().fg(Color::LightBlue),
            ))))
            .block(Block::default().title(title).borders(Borders::ALL)),
            popup,
        );
        return;
    }

    if let Some(error) = modal.error.as_deref() {
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(Span::styled(
                    "Failed to load diff:",
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(error.to_owned()),
            ]))
            .block(Block::default().title(title).borders(Borders::ALL)),
            popup,
        );
        return;
    }

    if modal.content.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Text::from(Line::from(Span::styled(
                "(No diff against base branch.)",
                Style::default().fg(Color::DarkGray),
            ))))
            .block(Block::default().title(title).borders(Borders::ALL)),
            popup,
        );
        return;
    }

    let outer = Block::default().title(title).borders(Borders::ALL);
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);
    if inner.width < 20 || inner.height < 6 {
        return;
    }

    let panes = if inner.width < 90 {
        Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(inner)
    } else {
        Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(inner)
    };
    if panes.len() < 2 {
        return;
    }

    let files = parse_diff_file_summaries(modal.content.as_str());
    let left = render_diff_file_list(modal, files.as_slice());
    frame.render_widget(
        Paragraph::new(left).block(Block::default().title("files").borders(Borders::ALL)),
        panes[0],
    );

    let right = render_selected_file_diff(modal, files.as_slice());
    let selected_path = files
        .get(modal.selected_file_index.min(files.len().saturating_sub(1)))
        .map(|entry| entry.path.as_str())
        .unwrap_or("(no file)");
    let right_title = format!("diff | {selected_path}");
    let diff_viewport_rows = usize::from(panes[1].height.saturating_sub(2)).max(1);
    frame.render_widget(
        Paragraph::new(right)
            .scroll((
                worktree_diff_modal_scroll(modal, files.as_slice(), diff_viewport_rows),
                0,
            ))
            .block(Block::default().title(right_title).borders(Borders::ALL)),
        panes[1],
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffRenderedLineKind {
    FileHeader,
    HunkHeader,
    Addition,
    Deletion,
    Context,
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffRenderedLine {
    text: String,
    kind: DiffRenderedLineKind,
    file_path: Option<String>,
    new_line_no: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffAdditionBlock {
    start_index: usize,
    end_index: usize,
    file_path: String,
    start_new_line: usize,
    end_new_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffFileSummary {
    path: String,
    added: usize,
    removed: usize,
    start_index: usize,
    end_index: usize,
    addition_blocks: Vec<DiffAdditionBlock>,
}

fn parse_rendered_diff_lines(content: &str) -> Vec<DiffRenderedLine> {
    let mut lines = Vec::new();
    let mut current_file: Option<String> = None;
    let mut old_line: Option<usize> = None;
    let mut new_line: Option<usize> = None;

    for raw_line in content.lines() {
        let line = sanitize_terminal_display_text(raw_line);
        if let Some(path) = parse_diff_file_header_path(line.as_str()) {
            current_file = Some(path);
            old_line = None;
            new_line = None;
        } else if let Some(path) = parse_diff_plus_plus_plus_path(line.as_str()) {
            current_file = Some(path);
        }

        if let Some((old_start, new_start)) = parse_unified_diff_hunk_header(line.as_str()) {
            old_line = Some(old_start);
            new_line = Some(new_start);
            lines.push(DiffRenderedLine {
                text: format!("{:>6} {:>6} {}", "", "", line),
                kind: DiffRenderedLineKind::HunkHeader,
                file_path: current_file.clone(),
                new_line_no: None,
            });
            continue;
        }

        let (old_cell, new_cell, kind, new_line_no) =
            if line.starts_with('+') && !line.starts_with("+++") {
                let new_cell = format_line_no(new_line);
                let selected_new_line = new_line;
                if let Some(value) = new_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                (
                    String::new(),
                    new_cell,
                    DiffRenderedLineKind::Addition,
                    selected_new_line,
                )
            } else if line.starts_with('-') && !line.starts_with("---") {
                let old_cell = format_line_no(old_line);
                if let Some(value) = old_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                (
                    old_cell,
                    String::new(),
                    DiffRenderedLineKind::Deletion,
                    None,
                )
            } else if line.starts_with(' ') {
                let old_cell = format_line_no(old_line);
                let new_cell = format_line_no(new_line);
                let selected_new_line = new_line;
                if let Some(value) = old_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                if let Some(value) = new_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                (
                    old_cell,
                    new_cell,
                    DiffRenderedLineKind::Context,
                    selected_new_line,
                )
            } else if line.starts_with("diff --git ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
            {
                (
                    String::new(),
                    String::new(),
                    DiffRenderedLineKind::FileHeader,
                    None,
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    DiffRenderedLineKind::Metadata,
                    None,
                )
            };

        lines.push(DiffRenderedLine {
            text: format!("{:>6} {:>6} {}", old_cell, new_cell, line),
            kind,
            file_path: current_file.clone(),
            new_line_no,
        });
    }

    lines
}

fn parse_diff_addition_blocks(content: &str) -> Vec<DiffAdditionBlock> {
    let lines = parse_rendered_diff_lines(content);
    let mut blocks = Vec::new();
    let mut active: Option<DiffAdditionBlock> = None;

    for (index, line) in lines.into_iter().enumerate() {
        if line.kind == DiffRenderedLineKind::Addition {
            let Some(path) = line.file_path else {
                continue;
            };
            let Some(new_line_no) = line.new_line_no else {
                continue;
            };
            if let Some(current) = active.as_mut() {
                if current.file_path == path && current.end_index + 1 == index {
                    current.end_index = index;
                    current.end_new_line = new_line_no;
                    continue;
                }
                blocks.push(current.clone());
            }
            active = Some(DiffAdditionBlock {
                start_index: index,
                end_index: index,
                file_path: path,
                start_new_line: new_line_no,
                end_new_line: new_line_no,
            });
        } else if let Some(current) = active.take() {
            blocks.push(current);
        }
    }

    if let Some(current) = active {
        blocks.push(current);
    }
    blocks
}

fn parse_diff_file_summaries(content: &str) -> Vec<DiffFileSummary> {
    let lines = parse_rendered_diff_lines(content);
    if lines.is_empty() {
        return Vec::new();
    }

    let blocks = parse_diff_addition_blocks(content);
    let mut files = Vec::<DiffFileSummary>::new();

    for (index, line) in lines.iter().enumerate() {
        let Some(path) = line.file_path.as_deref() else {
            continue;
        };
        let needs_new = files
            .last()
            .map(|entry| entry.path.as_str() != path)
            .unwrap_or(true);
        if needs_new {
            files.push(DiffFileSummary {
                path: path.to_owned(),
                added: 0,
                removed: 0,
                start_index: index,
                end_index: index,
                addition_blocks: Vec::new(),
            });
        }
        if let Some(current) = files.last_mut() {
            current.end_index = index;
            match line.kind {
                DiffRenderedLineKind::Addition => current.added = current.added.saturating_add(1),
                DiffRenderedLineKind::Deletion => {
                    current.removed = current.removed.saturating_add(1)
                }
                _ => {}
            }
        }
    }

    for file in files.iter_mut() {
        file.addition_blocks = blocks
            .iter()
            .filter(|block| block.file_path == file.path)
            .cloned()
            .collect();
    }

    files
}

fn render_diff_file_list(
    modal: &WorktreeDiffModalState,
    files: &[DiffFileSummary],
) -> Text<'static> {
    if files.is_empty() {
        return Text::from(Line::from(Span::styled(
            "(No changed files.)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let mut rendered = Vec::with_capacity(files.len());
    let selected = modal.selected_file_index.min(files.len().saturating_sub(1));
    for (index, file) in files.iter().enumerate() {
        let mut base = Style::default().fg(Color::Gray);
        if index == selected {
            base = if modal.focus == DiffPaneFocus::Files {
                base.bg(Color::Rgb(59, 66, 82)).add_modifier(Modifier::BOLD)
            } else {
                base.bg(Color::Rgb(47, 52, 63))
            };
        }
        rendered.push(Line::from(vec![
            Span::styled(format!("{:<36}", file.path), base),
            Span::styled(format!(" +{}", file.added), base.fg(Color::LightGreen)),
            Span::styled(format!(" -{}", file.removed), base.fg(Color::LightRed)),
        ]));
    }
    Text::from(rendered)
}

fn selected_file_and_hunk_range(
    modal: &WorktreeDiffModalState,
    files: &[DiffFileSummary],
) -> Option<(usize, usize, Option<(usize, usize)>)> {
    let file = files.get(modal.selected_file_index.min(files.len().saturating_sub(1)))?;
    let hunk = file
        .addition_blocks
        .get(
            modal
                .selected_hunk_index
                .min(file.addition_blocks.len().saturating_sub(1)),
        )
        .map(|block| (block.start_index, block.end_index));
    Some((file.start_index, file.end_index, hunk))
}

fn render_selected_file_diff(
    modal: &WorktreeDiffModalState,
    files: &[DiffFileSummary],
) -> Text<'static> {
    let parsed = parse_rendered_diff_lines(modal.content.as_str());
    let Some((start, end, selected_hunk)) = selected_file_and_hunk_range(modal, files) else {
        return Text::from(Line::from(Span::styled(
            "(No diff content for selected file.)",
            Style::default().fg(Color::DarkGray),
        )));
    };

    let mut rendered = Vec::with_capacity(end.saturating_sub(start).saturating_add(1));
    for (global_index, line) in parsed.iter().enumerate().skip(start).take(end - start + 1) {
        let mut style = diff_rendered_line_style(line.kind);
        if let Some((hunk_start, hunk_end)) = selected_hunk {
            if global_index >= hunk_start && global_index <= hunk_end {
                style = if modal.focus == DiffPaneFocus::Diff {
                    style
                        .bg(Color::Rgb(59, 66, 82))
                        .add_modifier(Modifier::BOLD)
                } else {
                    style.bg(Color::Rgb(47, 52, 63))
                };
            }
        }
        rendered.push(Line::from(Span::styled(line.text.clone(), style)));
    }
    Text::from(rendered)
}

fn worktree_diff_modal_scroll(
    modal: &WorktreeDiffModalState,
    files: &[DiffFileSummary],
    viewport_rows: usize,
) -> u16 {
    let Some((file_start, file_end, selected_hunk)) = selected_file_and_hunk_range(modal, files)
    else {
        return 0;
    };
    let center = selected_hunk
        .map(|(start, end)| start + (end.saturating_sub(start) / 2))
        .unwrap_or(file_start);
    let local_line = center.saturating_sub(file_start);
    let preferred_scroll = local_line.saturating_sub(3);
    let line_count = file_end.saturating_sub(file_start).saturating_add(1);
    let max_scroll = line_count.saturating_sub(viewport_rows.max(1));
    preferred_scroll.min(max_scroll).min(u16::MAX as usize) as u16
}

fn diff_rendered_line_style(kind: DiffRenderedLineKind) -> Style {
    match kind {
        DiffRenderedLineKind::FileHeader => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::HunkHeader => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::Addition => Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::Deletion => Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::Context => Style::default().fg(Color::Gray),
        DiffRenderedLineKind::Metadata => Style::default().fg(Color::DarkGray),
    }
}

fn format_line_no(value: Option<usize>) -> String {
    value.map(|entry| entry.to_string()).unwrap_or_default()
}

fn parse_diff_file_header_path(line: &str) -> Option<String> {
    if !line.starts_with("diff --git ") {
        return None;
    }
    let mut parts = line.split_whitespace();
    let _diff = parts.next()?;
    let _git = parts.next()?;
    let _a_path = parts.next()?;
    let b_path = parts.next()?;
    normalize_diff_path(b_path)
}

fn parse_diff_plus_plus_plus_path(line: &str) -> Option<String> {
    let raw = line.strip_prefix("+++ ")?;
    normalize_diff_path(raw.trim())
}

fn normalize_diff_path(raw: &str) -> Option<String> {
    let path = raw.trim();
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    let normalized = path.strip_prefix("b/").unwrap_or(path).trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_owned())
    }
}

fn worktree_diff_modal_line_count(modal: &WorktreeDiffModalState) -> usize {
    if modal.loading || modal.error.is_some() || modal.content.trim().is_empty() {
        return 1;
    }
    let files = parse_diff_file_summaries(modal.content.as_str());
    if let Some((start, end, _)) = selected_file_and_hunk_range(modal, files.as_slice()) {
        return end.saturating_sub(start).saturating_add(1).max(1);
    }
    parse_rendered_diff_lines(modal.content.as_str())
        .len()
        .max(1)
}

fn collect_selected_worktree_diff_refs(
    modal: &WorktreeDiffModalState,
) -> Result<Vec<String>, String> {
    if modal.loading {
        return Err("diff selection unavailable while loading".to_owned());
    }
    if modal.error.is_some() {
        return Err("diff selection unavailable: fix diff load error first".to_owned());
    }
    if modal.content.trim().is_empty() {
        return Err("diff selection unavailable: no diff content loaded".to_owned());
    }
    if parse_rendered_diff_lines(modal.content.as_str()).is_empty() {
        return Err("diff selection unavailable: no diff content loaded".to_owned());
    }
    let files = parse_diff_file_summaries(modal.content.as_str());
    let Some(file) = files.get(modal.selected_file_index.min(files.len().saturating_sub(1))) else {
        return Err("diff selection unavailable: no changed files in diff".to_owned());
    };
    let Some(block) = file.addition_blocks.get(
        modal
            .selected_hunk_index
            .min(file.addition_blocks.len().saturating_sub(1)),
    ) else {
        return Err("diff selection unavailable: no addition blocks in diff".to_owned());
    };

    Ok(vec![format!(
        "[{} {}:{}]",
        block.file_path, block.start_new_line, block.end_new_line
    )])
}

fn parse_unified_diff_hunk_header(line: &str) -> Option<(usize, usize)> {
    if !line.starts_with("@@") {
        return None;
    }
    let rest = line.strip_prefix("@@")?;
    let (header, _) = rest.split_once("@@")?;
    let mut parts = header.split_whitespace();
    let old_part = parts.next()?;
    let new_part = parts.next()?;
    let old_start = parse_unified_diff_hunk_coord(old_part, '-')?;
    let new_start = parse_unified_diff_hunk_coord(new_part, '+')?;
    Some((old_start, new_start))
}

fn parse_unified_diff_hunk_coord(part: &str, expected_prefix: char) -> Option<usize> {
    let raw = part.strip_prefix(expected_prefix)?;
    let start = raw.split_once(',').map(|(left, _)| left).unwrap_or(raw);
    start.parse::<usize>().ok()
}

fn worktree_diff_modal_popup(anchor_area: Rect) -> Option<Rect> {
    if anchor_area.width < 60 || anchor_area.height < 12 {
        return None;
    }

    let width = ((anchor_area.width as f32) * 0.90).round() as u16;
    let height = ((anchor_area.height as f32) * 0.86).round() as u16;
    let width = width.clamp(60, anchor_area.width.saturating_sub(2));
    let height = height.clamp(12, anchor_area.height.saturating_sub(2));

    Some(Rect {
        x: anchor_area.x + (anchor_area.width.saturating_sub(width)) / 2,
        y: anchor_area.y + (anchor_area.height.saturating_sub(height)) / 2,
        width,
        height,
    })
}

fn render_review_merge_confirm_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
) {
    let labels = session_display_labels(domain, session_id);
    let content = format!(
        "Merge pull request and reconcile completion?\n\nTicket: {}\n\nEnter/y: confirm merge\nEsc/n: cancel",
        labels.full_label
    );
    let Some(popup) = review_merge_confirm_popup(anchor_area) else {
        return;
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("review merge").borders(Borders::ALL)),
        popup,
    );
}

fn render_archive_session_confirm_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
) {
    let labels = session_display_labels(domain, session_id);
    let content = format!(
        "Archive selected session and clean up worktree/branch?\n\nTicket: {}\n\nEnter/y: confirm archive\nEsc/n: cancel",
        labels.full_label
    );
    let Some(popup) = review_merge_confirm_popup(anchor_area) else {
        return;
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .title("archive session")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

fn render_ticket_archive_confirm_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    ticket: &TicketSummary,
) {
    let content = format!(
        "Archive ticket from start ticket window?\n\nTicket: {} - {}\n\nEnter/y: confirm archive\nEsc/n: cancel",
        ticket.identifier,
        compact_focus_card_text(ticket.title.as_str())
    );
    let Some(popup) = review_merge_confirm_popup(anchor_area) else {
        return;
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .title("archive ticket")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

fn review_merge_confirm_popup(anchor_area: Rect) -> Option<Rect> {
    if anchor_area.width < 40 || anchor_area.height < 8 {
        return None;
    }
    let width = ((anchor_area.width as f32) * 0.55).round() as u16;
    let height = 8u16;
    let width = width.clamp(40, anchor_area.width.saturating_sub(2));
    let height = height.min(anchor_area.height.saturating_sub(2));
    Some(Rect {
        x: anchor_area.x + (anchor_area.width.saturating_sub(width)) / 2,
        y: anchor_area.y + (anchor_area.height.saturating_sub(height)) / 2,
        width,
        height,
    })
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
    project_names: Vec<String>,
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

    for project_name in project_names {
        let project_name = project_name.trim().to_owned();
        if project_name.is_empty() {
            continue;
        }
        if !projects
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(project_name.as_str()))
        {
            projects.push((project_name, Vec::new()));
        }
    }

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
    SetApplicationModeAutopilot,
    SetApplicationModeManual,
    StartTerminalEscapeChord,
    ToggleWorktreeDiffModal,
    AdvanceTerminalWorkflowStage,
    ArchiveSelectedSession,
    OpenSessionOutputForSelectedInbox,
}

impl UiCommand {
    const ALL: [Self; 33] = [
        Self::EnterNormalMode,
        Self::EnterInsertMode,
        Self::ToggleGlobalSupervisorChat,
        Self::OpenTerminalForSelected,
        Self::OpenDiffInspectorForSelected,
        Self::OpenTestInspectorForSelected,
        Self::OpenPrInspectorForSelected,
        Self::OpenChatInspectorForSelected,
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
        Self::SetApplicationModeAutopilot,
        Self::SetApplicationModeManual,
        Self::StartTerminalEscapeChord,
        Self::ToggleWorktreeDiffModal,
        Self::AdvanceTerminalWorkflowStage,
        Self::ArchiveSelectedSession,
        Self::OpenSessionOutputForSelectedInbox,
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
            Self::SetApplicationModeAutopilot => "ui.app_mode.autopilot",
            Self::SetApplicationModeManual => "ui.app_mode.manual",
            Self::StartTerminalEscapeChord => "ui.mode.terminal_escape_prefix",
            Self::ToggleWorktreeDiffModal => "ui.worktree.diff.toggle",
            Self::AdvanceTerminalWorkflowStage => "ui.terminal.workflow.advance",
            Self::ArchiveSelectedSession => "ui.terminal.archive_selected_session",
            Self::OpenSessionOutputForSelectedInbox => "ui.open_session_output_for_selected_inbox",
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
            Self::QuitShell => "Quit shell",
            Self::FocusNextInbox => "Focus next session",
            Self::FocusPreviousInbox => "Focus previous session",
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
            Self::SetApplicationModeAutopilot => "Set application mode to autopilot",
            Self::SetApplicationModeManual => "Set application mode to manual",
            Self::StartTerminalEscapeChord => "Start terminal escape chord prefix",
            Self::ToggleWorktreeDiffModal => "Toggle worktree diff modal for selected session",
            Self::AdvanceTerminalWorkflowStage => "Advance terminal workflow stage",
            Self::ArchiveSelectedSession => "Archive selected terminal session",
            Self::OpenSessionOutputForSelectedInbox => {
                "Open session output for selected inbox item"
            }
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
                    binding(&["]"], UiCommand::CycleBatchNext),
                    binding(&["["], UiCommand::CycleBatchPrevious),
                    binding(&["g"], UiCommand::JumpFirstInbox),
                    binding(&["G"], UiCommand::JumpLastInbox),
                    binding(&["1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["2"], UiCommand::JumpBatchApprovals),
                    binding(&["3"], UiCommand::JumpBatchReviewReady),
                    binding(&["4"], UiCommand::JumpBatchFyiDigest),
                    binding(&["s"], UiCommand::OpenTicketPicker),
                    binding(&["c"], UiCommand::ToggleGlobalSupervisorChat),
                    binding(&["i"], UiCommand::EnterInsertMode),
                    binding(&["I"], UiCommand::OpenTerminalForSelected),
                    binding(&["o"], UiCommand::OpenSessionOutputForSelectedInbox),
                    binding(&["D"], UiCommand::ToggleWorktreeDiffModal),
                    binding(&["w", "n"], UiCommand::AdvanceTerminalWorkflowStage),
                    binding(&["x"], UiCommand::ArchiveSelectedSession),
                    binding(&["z", "1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["z", "2"], UiCommand::JumpBatchApprovals),
                    binding(&["z", "3"], UiCommand::JumpBatchReviewReady),
                    binding(&["z", "4"], UiCommand::JumpBatchFyiDigest),
                    binding(&["v", "d"], UiCommand::OpenDiffInspectorForSelected),
                    binding(&["v", "t"], UiCommand::OpenTestInspectorForSelected),
                    binding(&["v", "p"], UiCommand::OpenPrInspectorForSelected),
                    binding(&["v", "c"], UiCommand::OpenChatInspectorForSelected),
                    binding(&["m", "a"], UiCommand::SetApplicationModeAutopilot),
                    binding(&["m", "m"], UiCommand::SetApplicationModeManual),
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
                    KeyPrefixConfig {
                        keys: vec!["w".to_owned()],
                        label: "Workflow Actions".to_owned(),
                    },
                    KeyPrefixConfig {
                        keys: vec!["m".to_owned()],
                        label: "Application modes".to_owned(),
                    },
                ],
            },
            ModeKeymapConfig {
                mode: UiMode::Insert,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BottomBarHintGroup {
    label: &'static str,
    hints: &'static [&'static str],
}

fn bottom_bar_hint_groups(mode: UiMode) -> &'static [BottomBarHintGroup] {
    match mode {
        UiMode::Normal => &[
            BottomBarHintGroup {
                label: "Navigate:",
                hints: &["j/k sessions", "Shift+J/K output", "g/G", "1-4 or z{1-4}", "[ ]"],
            },
            BottomBarHintGroup {
                label: "Views:",
                hints: &["i/I", "o", "s", "c", "v{d/t/p/c}", "D"],
            },
            BottomBarHintGroup {
                label: "Workflow:",
                hints: &["w n", "x", "q"],
            },
        ],
        UiMode::Insert => &[
            BottomBarHintGroup {
                label: "Insert:",
                hints: &["type/edit input"],
            },
            BottomBarHintGroup {
                label: "Back:",
                hints: &["Esc", "Ctrl-["],
            },
        ],
        UiMode::Terminal => &[
            BottomBarHintGroup {
                label: "Terminal:",
                hints: &["Shift+J/K output", "i", "Esc"],
            },
            BottomBarHintGroup {
                label: "Actions:",
                hints: &["w n", "x", "D", "q"],
            },
        ],
    }
}

fn mode_help(mode: UiMode) -> String {
    bottom_bar_hint_groups(mode)
        .iter()
        .map(|group| format!("{} {}", group.label, group.hints.join(", ")))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn append_terminal_output(
    state: &mut TerminalViewState,
    bytes: Vec<u8>,
    transcript_line_limit: usize,
) {
    let chunk = sanitize_terminal_display_text(String::from_utf8_lossy(&bytes).as_ref());
    if chunk.is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

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
        if let Some((before, kind, content)) = parse_terminal_meta_line_embedded(line) {
            let before = before.trim();
            if !before.is_empty() {
                if is_outgoing_transcript_line(before) {
                    append_terminal_transcript_entry(
                        state,
                        TerminalTranscriptEntry::Message(format!(
                            "> {}",
                            normalize_outgoing_user_line(before)
                        )),
                        transcript_line_limit,
                    );
                } else {
                    append_terminal_transcript_entry(
                        state,
                        TerminalTranscriptEntry::Message(before.to_owned()),
                        transcript_line_limit,
                    );
                }
            }
            append_terminal_foldable_content(state, kind, content.as_str(), transcript_line_limit);
            continue;
        }
        if is_outgoing_transcript_line(line) {
            append_terminal_transcript_entry(
                state,
                TerminalTranscriptEntry::Message(format!(
                    "> {}",
                    normalize_outgoing_user_line(line)
                )),
                transcript_line_limit,
            );
        } else {
            append_terminal_transcript_entry(
                state,
                TerminalTranscriptEntry::Message(line.to_owned()),
                transcript_line_limit,
            );
        }
    }
}

fn append_terminal_assistant_output(
    state: &mut TerminalViewState,
    bytes: Vec<u8>,
    transcript_line_limit: usize,
) {
    append_terminal_output(state, bytes, transcript_line_limit);
}

fn append_terminal_user_message(
    state: &mut TerminalViewState,
    message: &str,
    transcript_line_limit: usize,
) {
    flush_terminal_output_fragment(state, transcript_line_limit);
    let text = sanitize_terminal_display_text(message);
    if text.trim().is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let _ = index;
        append_terminal_transcript_entry(
            state,
            TerminalTranscriptEntry::Message(format!("> {line}")),
            transcript_line_limit,
        );
    }
}

fn parse_terminal_meta_line_embedded(line: &str) -> Option<(String, TerminalFoldKind, String)> {
    const PREFIX: &str = "[[orchestrator-meta|";
    let start = line.find(PREFIX)?;
    let before = line[..start].to_owned();
    let suffix = &line[start + PREFIX.len()..];
    let closing = suffix.find("]]")?;
    let kind = match suffix[..closing].trim() {
        "reasoning" => TerminalFoldKind::Reasoning,
        "file-change" => TerminalFoldKind::FileChange,
        "tool-call" => TerminalFoldKind::ToolCall,
        "command" => TerminalFoldKind::CommandExecution,
        _ => TerminalFoldKind::Other,
    };
    let content = suffix[closing + 2..].trim().to_owned();
    Some((before, kind, content))
}

fn append_terminal_foldable_content(
    state: &mut TerminalViewState,
    kind: TerminalFoldKind,
    content: &str,
    transcript_line_limit: usize,
) {
    invalidate_terminal_render_cache(state);
    let entry_content = if content.trim().is_empty() {
        "(no details)"
    } else {
        content.trim()
    };

    if let Some(TerminalTranscriptEntry::Foldable(previous)) = state.entries.last_mut() {
        if previous.kind == kind {
            if !previous.content.is_empty() {
                previous.content.push('\n');
            }
            previous.content.push_str(entry_content);
            return;
        }
    }

    append_terminal_transcript_entry(
        state,
        TerminalTranscriptEntry::Foldable(TerminalFoldSection {
            kind,
            content: entry_content.to_owned(),
            folded: true,
        }),
        transcript_line_limit,
    );
}

fn is_outgoing_transcript_line(line: &str) -> bool {
    let normalized = line.trim_start();
    normalized.starts_with("system:")
        || normalized.starts_with("you:")
        || normalized.starts_with("user:")
}

fn flush_terminal_output_fragment(state: &mut TerminalViewState, transcript_line_limit: usize) {
    let fragment = std::mem::take(&mut state.output_fragment);
    let line = fragment.trim_end_matches('\r');
    if line.trim().is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

    if let Some((before, kind, content)) = parse_terminal_meta_line_embedded(line) {
        let before = before.trim();
        if !before.is_empty() {
            if is_outgoing_transcript_line(before) {
                append_terminal_transcript_entry(
                    state,
                    TerminalTranscriptEntry::Message(format!(
                        "> {}",
                        normalize_outgoing_user_line(before)
                    )),
                    transcript_line_limit,
                );
            } else {
                append_terminal_transcript_entry(
                    state,
                    TerminalTranscriptEntry::Message(before.to_owned()),
                    transcript_line_limit,
                );
            }
        }
        append_terminal_foldable_content(state, kind, content.as_str(), transcript_line_limit);
        return;
    }

    if is_outgoing_transcript_line(line) {
        append_terminal_transcript_entry(
            state,
            TerminalTranscriptEntry::Message(format!("> {}", normalize_outgoing_user_line(line))),
            transcript_line_limit,
        );
    } else {
        append_terminal_transcript_entry(
            state,
            TerminalTranscriptEntry::Message(line.to_owned()),
            transcript_line_limit,
        );
    }
}

fn append_terminal_transcript_entry(
    state: &mut TerminalViewState,
    entry: TerminalTranscriptEntry,
    transcript_line_limit: usize,
) {
    state.entries.push(entry);
    if state.entries.len() <= transcript_line_limit {
        return;
    }

    let dropped = state.entries.len() - transcript_line_limit;
    state.entries.drain(0..dropped);
    state.transcript_truncated = true;
    state.transcript_truncated_line_count = state
        .transcript_truncated_line_count
        .saturating_add(dropped);
}

fn normalize_outgoing_user_line(line: &str) -> &str {
    line.strip_prefix("you: ")
        .or_else(|| line.strip_prefix("user: "))
        .unwrap_or(line)
}

fn invalidate_terminal_render_cache(state: &mut TerminalViewState) {
    state.render_cache.invalidate_all();
    state.output_rendered_line_count = 0;
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
    if shell_state.mode == UiMode::Terminal && !shell_state.is_terminal_view_active() {
        shell_state.enter_normal_mode();
    }

    if shell_state.terminal_escape_pending {
        shell_state.terminal_escape_pending = false;
        if is_ctrl_char(key, 'n') {
            shell_state.enter_normal_mode();
        }
        return RoutedInput::Ignore;
    }

    if shell_state.mode == UiMode::Terminal && is_ctrl_char(key, '\\') {
        shell_state.begin_terminal_escape_chord();
        return RoutedInput::Ignore;
    }

    if shell_state.worktree_diff_modal.is_some() {
        return route_worktree_diff_modal_key(shell_state, key);
    }
    if shell_state.is_ticket_picker_visible() {
        return route_ticket_picker_key(shell_state, key);
    }
    if shell_state.archive_session_confirm_session.is_some() {
        return route_archive_session_confirm_key(shell_state, key);
    }
    if shell_state.review_merge_confirm_session.is_some() {
        return route_review_merge_confirm_key(shell_state, key);
    }

    if shell_state.terminal_session_has_active_needs_input() && shell_state.is_terminal_view_active()
    {
        return route_needs_input_modal_key(shell_state, key);
    }
    if shell_state.terminal_session_has_any_needs_input()
        && !shell_state.terminal_session_has_active_needs_input()
        && shell_state.is_terminal_view_active()
        && key.modifiers.is_empty()
        && is_needs_input_interaction_key(key.code)
    {
        let _ = shell_state.activate_terminal_needs_input(false);
        return route_needs_input_modal_key(shell_state, key);
    }

    if shell_state.mode == UiMode::Normal && shell_state.is_terminal_view_active() {
        match (key.code, key.modifiers) {
            (KeyCode::Char('J') | KeyCode::Char('j'), KeyModifiers::SHIFT) => {
                shell_state.scroll_terminal_output_view(1);
                return RoutedInput::Ignore;
            }
            (KeyCode::Char('K') | KeyCode::Char('k'), KeyModifiers::SHIFT) => {
                shell_state.scroll_terminal_output_view(-1);
                return RoutedInput::Ignore;
            }
            _ => {}
        }
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

    route_configured_mode_key(shell_state, key)
}

fn is_needs_input_interaction_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('i')
            | KeyCode::Right
            | KeyCode::Char('l')
            | KeyCode::Left
            | KeyCode::Char('h')
            | KeyCode::Enter
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char(' ')
            | KeyCode::Char('j')
            | KeyCode::Char('k')
    )
}

fn route_worktree_diff_modal_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.close_worktree_diff_modal();
        return RoutedInput::Ignore;
    }

    if matches!(key.code, KeyCode::Char('D')) {
        shell_state.close_worktree_diff_modal();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter => {
                shell_state.insert_selected_worktree_diff_refs_into_compose();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('q') => {
                shell_state.close_worktree_diff_modal();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('g') => {
                shell_state.jump_worktree_diff_addition_block(false);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('G') => {
                shell_state.jump_worktree_diff_addition_block(true);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                shell_state.scroll_worktree_diff_modal(1);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                shell_state.scroll_worktree_diff_modal(-1);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                shell_state.focus_worktree_diff_files_pane();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('l') | KeyCode::Right => {
                shell_state.focus_worktree_diff_detail_pane();
                return RoutedInput::Ignore;
            }
            KeyCode::PageDown => {
                shell_state.scroll_worktree_diff_modal(12);
                return RoutedInput::Ignore;
            }
            KeyCode::PageUp => {
                shell_state.scroll_worktree_diff_modal(-12);
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
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

fn dispatch_command(shell_state: &mut UiShellState, command: UiCommand) -> bool {
    let _command_id = command.id();
    match command {
        UiCommand::EnterNormalMode => {
            shell_state.enter_normal_mode();
            false
        }
        UiCommand::EnterInsertMode => {
            shell_state.enter_insert_mode_for_current_focus();
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
        UiCommand::SetApplicationModeAutopilot => {
            shell_state.set_application_mode_autopilot();
            false
        }
        UiCommand::SetApplicationModeManual => {
            shell_state.set_application_mode_manual();
            false
        }
        UiCommand::StartTerminalEscapeChord => {
            shell_state.begin_terminal_escape_chord();
            false
        }
        UiCommand::ToggleWorktreeDiffModal => {
            shell_state.toggle_worktree_diff_modal();
            false
        }
        UiCommand::AdvanceTerminalWorkflowStage => {
            shell_state.advance_terminal_workflow_stage();
            false
        }
        UiCommand::ArchiveSelectedSession => {
            shell_state.begin_archive_selected_session_confirmation();
            false
        }
        UiCommand::OpenSessionOutputForSelectedInbox => {
            shell_state.open_session_output_for_selected_inbox();
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


pub(crate) mod bootstrap;
pub(crate) mod view_state;
pub(crate) mod input;
pub(crate) mod render;
pub(crate) mod features;
pub(crate) mod theme;

#[cfg(test)]
pub(crate) mod tests;
