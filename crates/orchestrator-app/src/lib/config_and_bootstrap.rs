use integration_linear::LinearTicketingProvider;
use orchestrator_core::{
    apply_workflow_transition, rebuild_projection, CodeHostProvider, CoreError, EventStore,
    GetTicketRequest, GithubClient, InboxItemCreatedPayload, InboxItemId, InboxItemResolvedPayload,
    LlmProvider, NewEventEnvelope, OrchestrationEventPayload, ProjectionState, RuntimeSessionId,
    SelectedTicketFlowConfig, SelectedTicketFlowResult, SessionCompletedPayload,
    SessionCrashedPayload, SessionHandle, SqliteEventStore, Supervisor, TicketSummary,
    StoredEventEnvelope, TicketingProvider, UntypedCommandInvocation, VcsProvider, WorkItemId,
    WorkerBackend, WorkerSessionId, WorkerSessionStatus, WorkflowGuardContext, WorkflowState,
    WorkflowTransitionPayload, WorkflowTransitionReason, DOMAIN_EVENT_SCHEMA_VERSION,
    SessionRuntimeProjection,
};
use orchestrator_ui::{
    InboxPublishRequest, InboxResolveRequest, SessionArchiveOutcome,
    SessionMergeFinalizeOutcome, SessionWorkflowAdvanceOutcome, SupervisorCommandContext,
    SupervisorCommandDispatcher,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
#[path = "../command_dispatch.rs"]
mod command_dispatch;
#[path = "../ticket_picker.rs"]
mod ticket_picker;
pub use ticket_picker::AppTicketPickerProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub workspace: String,
    #[serde(default = "default_event_store_path")]
    pub event_store_path: String,
    #[serde(default = "default_ticketing_provider")]
    pub ticketing_provider: String,
    #[serde(default = "default_harness_provider")]
    pub harness_provider: String,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    #[serde(default)]
    pub linear: LinearConfigToml,
    #[serde(default)]
    pub shortcut: ShortcutConfigToml,
    #[serde(default)]
    pub git: GitConfigToml,
    #[serde(default)]
    pub github: GithubConfigToml,
    #[serde(default)]
    pub runtime: RuntimeConfigToml,
    #[serde(default)]
    pub ui: UiConfigToml,
}

const LEGACY_DEFAULT_WORKSPACE_PATH: &str = "./";
const LEGACY_DEFAULT_EVENT_STORE_PATH: &str = "./orchestrator-events.db";
const DEFAULT_TICKETING_PROVIDER: &str = "linear";
const DEFAULT_HARNESS_PROVIDER: &str = "codex";
const DEFAULT_SUPERVISOR_MODEL: &str = "c/claude-haiku-4.5";
const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const DEFAULT_LINEAR_SYNC_INTERVAL_SECS: u64 = 60;
const DEFAULT_LINEAR_FETCH_LIMIT: u32 = 100;
const DEFAULT_LINEAR_SYNC_ASSIGNED_TO_ME: bool = true;
const DEFAULT_LINEAR_WORKFLOW_COMMENT_SUMMARIES: bool = false;
const DEFAULT_LINEAR_WORKFLOW_ATTACH_PR_LINKS: bool = true;
const DEFAULT_SHORTCUT_API_URL: &str = "https://api.app.shortcut.com/api/v3";
const DEFAULT_SHORTCUT_FETCH_LIMIT: u32 = 100;
const DEFAULT_GIT_BINARY: &str = "git";
const DEFAULT_GH_BINARY: &str = "gh";
const DEFAULT_ALLOW_UNSAFE_COMMAND_PATHS: bool = false;
const DEFAULT_HARNESS_SERVER_STARTUP_TIMEOUT_SECS: u64 = 10;
const DEFAULT_HARNESS_LOG_RAW_EVENTS: bool = false;
const DEFAULT_HARNESS_LOG_NORMALIZED_EVENTS: bool = false;
const DEFAULT_OPENCODE_BINARY: &str = "opencode";
const DEFAULT_OPENCODE_SERVER_BASE_URL: &str = "http://127.0.0.1:8787";
const DEFAULT_CODEX_BINARY: &str = "codex";
const DEFAULT_EVENT_RETENTION_DAYS: u64 = 14;
const DEFAULT_EVENT_PRUNE_ENABLED: bool = true;
const DEFAULT_UI_THEME: &str = "nord";
const DEFAULT_UI_TRANSCRIPT_LINE_LIMIT: usize = 100;
const DEFAULT_TICKET_PICKER_PRIORITY_STATES: &[&str] =
    &["In Progress", "Final Approval", "Todo", "Backlog"];

fn default_workspace_path() -> String {
    default_orchestrator_data_dir()
        .join("workspace")
        .to_string_lossy()
        .to_string()
}

fn default_event_store_path() -> String {
    default_orchestrator_data_dir()
        .join("orchestrator-events.db")
        .to_string_lossy()
        .to_string()
}
fn default_ticketing_provider() -> String {
    DEFAULT_TICKETING_PROVIDER.to_owned()
}
fn default_harness_provider() -> String {
    DEFAULT_HARNESS_PROVIDER.to_owned()
}

fn default_linear_sync_states() -> Vec<String> {
    Vec::new()
}

fn default_linear_workflow_state_map() -> Vec<WorkflowStateMapEntry> {
    vec![
        WorkflowStateMapEntry::new("Implementing", "In Progress"),
        WorkflowStateMapEntry::new("PRDrafted", "In Review"),
        WorkflowStateMapEntry::new("AwaitingYourReview", "In Review"),
        WorkflowStateMapEntry::new("ReadyForReview", "In Review"),
        WorkflowStateMapEntry::new("InReview", "In Review"),
        WorkflowStateMapEntry::new("PendingMerge", "In Review"),
        WorkflowStateMapEntry::new("Done", "Done"),
        WorkflowStateMapEntry::new("Abandoned", "Canceled"),
    ]
}

fn default_ticket_picker_priority_states() -> Vec<String> {
    DEFAULT_TICKET_PICKER_PRIORITY_STATES
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SupervisorConfig {
    #[serde(default = "default_supervisor_model")]
    pub model: String,
    #[serde(default = "default_openrouter_base_url")]
    pub openrouter_base_url: String,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            model: default_supervisor_model(),
            openrouter_base_url: default_openrouter_base_url(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowStateMapEntry {
    pub workflow_state: String,
    pub linear_state: String,
}

impl WorkflowStateMapEntry {
    fn new(workflow_state: &str, linear_state: &str) -> Self {
        Self {
            workflow_state: workflow_state.to_owned(),
            linear_state: linear_state.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinearConfigToml {
    #[serde(default = "default_linear_api_url")]
    pub api_url: String,
    #[serde(default = "default_linear_sync_interval_secs")]
    pub sync_interval_secs: u64,
    #[serde(default = "default_linear_fetch_limit")]
    pub fetch_limit: u32,
    #[serde(default = "default_linear_sync_assigned_to_me")]
    pub sync_assigned_to_me: bool,
    #[serde(default = "default_linear_sync_states")]
    pub sync_states: Vec<String>,
    #[serde(default = "default_linear_workflow_state_map")]
    pub workflow_state_map: Vec<WorkflowStateMapEntry>,
    #[serde(default = "default_linear_workflow_comment_summaries")]
    pub workflow_comment_summaries: bool,
    #[serde(default = "default_linear_workflow_attach_pr_links")]
    pub workflow_attach_pr_links: bool,
}

impl Default for LinearConfigToml {
    fn default() -> Self {
        Self {
            api_url: default_linear_api_url(),
            sync_interval_secs: default_linear_sync_interval_secs(),
            fetch_limit: default_linear_fetch_limit(),
            sync_assigned_to_me: default_linear_sync_assigned_to_me(),
            sync_states: default_linear_sync_states(),
            workflow_state_map: default_linear_workflow_state_map(),
            workflow_comment_summaries: default_linear_workflow_comment_summaries(),
            workflow_attach_pr_links: default_linear_workflow_attach_pr_links(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShortcutConfigToml {
    #[serde(default = "default_shortcut_api_url")]
    pub api_url: String,
    #[serde(default = "default_shortcut_fetch_limit")]
    pub fetch_limit: u32,
}

impl Default for ShortcutConfigToml {
    fn default() -> Self {
        Self {
            api_url: default_shortcut_api_url(),
            fetch_limit: default_shortcut_fetch_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitConfigToml {
    #[serde(default = "default_git_binary")]
    pub binary: String,
    #[serde(default)]
    pub allow_delete_unmerged_branches: bool,
    #[serde(default)]
    pub allow_destructive_automation: bool,
    #[serde(default)]
    pub allow_force_push: bool,
}

impl Default for GitConfigToml {
    fn default() -> Self {
        Self {
            binary: default_git_binary(),
            allow_delete_unmerged_branches: false,
            allow_destructive_automation: false,
            allow_force_push: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubConfigToml {
    #[serde(default = "default_gh_binary")]
    pub binary: String,
}

impl Default for GithubConfigToml {
    fn default() -> Self {
        Self {
            binary: default_gh_binary(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfigToml {
    #[serde(default = "default_allow_unsafe_command_paths")]
    pub allow_unsafe_command_paths: bool,
    #[serde(default = "default_harness_server_startup_timeout_secs")]
    pub harness_server_startup_timeout_secs: u64,
    #[serde(default = "default_harness_log_raw_events")]
    pub harness_log_raw_events: bool,
    #[serde(default = "default_harness_log_normalized_events")]
    pub harness_log_normalized_events: bool,
    #[serde(default = "default_opencode_binary")]
    pub opencode_binary: String,
    #[serde(default = "default_opencode_server_base_url")]
    pub opencode_server_base_url: String,
    #[serde(default = "default_codex_binary")]
    pub codex_binary: String,
    #[serde(default = "default_event_retention_days")]
    pub event_retention_days: u64,
    #[serde(default = "default_event_prune_enabled")]
    pub event_prune_enabled: bool,
}

impl Default for RuntimeConfigToml {
    fn default() -> Self {
        Self {
            allow_unsafe_command_paths: default_allow_unsafe_command_paths(),
            harness_server_startup_timeout_secs: default_harness_server_startup_timeout_secs(),
            harness_log_raw_events: default_harness_log_raw_events(),
            harness_log_normalized_events: default_harness_log_normalized_events(),
            opencode_binary: default_opencode_binary(),
            opencode_server_base_url: default_opencode_server_base_url(),
            codex_binary: default_codex_binary(),
            event_retention_days: default_event_retention_days(),
            event_prune_enabled: default_event_prune_enabled(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiConfigToml {
    #[serde(default = "default_ui_theme")]
    pub theme: String,
    #[serde(default = "default_ticket_picker_priority_states")]
    pub ticket_picker_priority_states: Vec<String>,
    #[serde(default = "default_ui_transcript_line_limit")]
    pub transcript_line_limit: usize,
    #[serde(default = "default_ui_background_session_refresh_secs")]
    pub background_session_refresh_secs: u64,
    #[serde(default = "default_ui_session_info_background_refresh_secs")]
    pub session_info_background_refresh_secs: u64,
    #[serde(default = "default_ui_merge_poll_base_interval_secs")]
    pub merge_poll_base_interval_secs: u64,
    #[serde(default = "default_ui_merge_poll_max_backoff_secs")]
    pub merge_poll_max_backoff_secs: u64,
    #[serde(default = "default_ui_merge_poll_backoff_multiplier")]
    pub merge_poll_backoff_multiplier: u64,
}

impl Default for UiConfigToml {
    fn default() -> Self {
        Self {
            theme: default_ui_theme(),
            ticket_picker_priority_states: default_ticket_picker_priority_states(),
            transcript_line_limit: default_ui_transcript_line_limit(),
            background_session_refresh_secs: default_ui_background_session_refresh_secs(),
            session_info_background_refresh_secs: default_ui_session_info_background_refresh_secs(),
            merge_poll_base_interval_secs: default_ui_merge_poll_base_interval_secs(),
            merge_poll_max_backoff_secs: default_ui_merge_poll_max_backoff_secs(),
            merge_poll_backoff_multiplier: default_ui_merge_poll_backoff_multiplier(),
        }
    }
}

fn default_supervisor_model() -> String {
    DEFAULT_SUPERVISOR_MODEL.to_owned()
}

fn default_openrouter_base_url() -> String {
    DEFAULT_OPENROUTER_BASE_URL.to_owned()
}

fn default_linear_api_url() -> String {
    DEFAULT_LINEAR_API_URL.to_owned()
}

fn default_linear_sync_interval_secs() -> u64 {
    DEFAULT_LINEAR_SYNC_INTERVAL_SECS
}

fn default_linear_fetch_limit() -> u32 {
    DEFAULT_LINEAR_FETCH_LIMIT
}

fn default_linear_sync_assigned_to_me() -> bool {
    DEFAULT_LINEAR_SYNC_ASSIGNED_TO_ME
}

fn default_linear_workflow_comment_summaries() -> bool {
    DEFAULT_LINEAR_WORKFLOW_COMMENT_SUMMARIES
}

fn default_linear_workflow_attach_pr_links() -> bool {
    DEFAULT_LINEAR_WORKFLOW_ATTACH_PR_LINKS
}

fn default_shortcut_api_url() -> String {
    DEFAULT_SHORTCUT_API_URL.to_owned()
}

fn default_shortcut_fetch_limit() -> u32 {
    DEFAULT_SHORTCUT_FETCH_LIMIT
}

fn default_git_binary() -> String {
    DEFAULT_GIT_BINARY.to_owned()
}

fn default_gh_binary() -> String {
    DEFAULT_GH_BINARY.to_owned()
}

fn default_allow_unsafe_command_paths() -> bool {
    DEFAULT_ALLOW_UNSAFE_COMMAND_PATHS
}

fn default_harness_server_startup_timeout_secs() -> u64 {
    DEFAULT_HARNESS_SERVER_STARTUP_TIMEOUT_SECS
}

fn default_harness_log_raw_events() -> bool {
    DEFAULT_HARNESS_LOG_RAW_EVENTS
}

fn default_harness_log_normalized_events() -> bool {
    DEFAULT_HARNESS_LOG_NORMALIZED_EVENTS
}

fn default_opencode_binary() -> String {
    DEFAULT_OPENCODE_BINARY.to_owned()
}

fn default_opencode_server_base_url() -> String {
    DEFAULT_OPENCODE_SERVER_BASE_URL.to_owned()
}

fn default_codex_binary() -> String {
    DEFAULT_CODEX_BINARY.to_owned()
}

fn default_event_retention_days() -> u64 {
    DEFAULT_EVENT_RETENTION_DAYS
}

fn default_event_prune_enabled() -> bool {
    DEFAULT_EVENT_PRUNE_ENABLED
}

fn default_ui_theme() -> String {
    DEFAULT_UI_THEME.to_owned()
}

fn default_ui_background_session_refresh_secs() -> u64 {
    15
}

fn default_ui_session_info_background_refresh_secs() -> u64 {
    15
}

fn default_ui_transcript_line_limit() -> usize {
    DEFAULT_UI_TRANSCRIPT_LINE_LIMIT
}

fn default_ui_merge_poll_base_interval_secs() -> u64 {
    15
}

fn default_ui_merge_poll_max_backoff_secs() -> u64 {
    120
}

fn default_ui_merge_poll_backoff_multiplier() -> u64 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupState {
    pub status: String,
    pub projection: ProjectionState,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            workspace: default_workspace_path(),
            event_store_path: default_event_store_path(),
            ticketing_provider: default_ticketing_provider(),
            harness_provider: default_harness_provider(),
            supervisor: SupervisorConfig::default(),
            linear: LinearConfigToml::default(),
            shortcut: ShortcutConfigToml::default(),
            git: GitConfigToml::default(),
            github: GithubConfigToml::default(),
            runtime: RuntimeConfigToml::default(),
            ui: UiConfigToml::default(),
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Result<Self, CoreError> {
        let path = config_path_from_env()?;
        load_or_create_config(&path)
    }
}

fn config_path_from_env() -> Result<std::path::PathBuf, CoreError> {
    match std::env::var("ORCHESTRATOR_CONFIG") {
        Ok(raw) => Ok(raw.into()),
        Err(std::env::VarError::NotPresent) => default_config_path(),
        Err(_) => Err(CoreError::Configuration(
            "ORCHESTRATOR_CONFIG contained invalid UTF-8".to_owned(),
        )),
    }
}

fn default_config_path() -> Result<std::path::PathBuf, CoreError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| {
            CoreError::Configuration(
                "Unable to resolve home directory from HOME or USERPROFILE".to_owned(),
            )
        })?;
    if home.trim().is_empty() {
        return Err(CoreError::Configuration(
            "HOME and USERPROFILE values are empty".to_owned(),
        ));
    }

    Ok(std::path::PathBuf::from(home)
        .join(".config")
        .join("orchestrator")
        .join("config.toml"))
}

fn default_orchestrator_data_dir() -> std::path::PathBuf {
    resolve_data_local_dir().join("orchestrator")
}

fn resolve_data_local_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(path) = std::env::var("LOCALAPPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(std::path::PathBuf::from(path));
            }
        }
        if let Ok(path) = std::env::var("APPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(std::path::PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join("AppData").join("Local");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = resolve_home_dir() {
            return home.join("Library").join("Application Support");
        }
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Ok(path) = std::env::var("XDG_DATA_HOME") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(std::path::PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join(".local").join("share");
        }
    }

    std::env::temp_dir()
}

fn resolve_home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
        })
}

fn absolutize_path(path: std::path::PathBuf) -> std::path::PathBuf {
    if path.is_absolute() {
        return path;
    }

    if let Ok(current) = std::env::current_dir() {
        return current.join(path);
    }

    std::env::temp_dir().join(path)
}

fn persist_config(path: &std::path::Path, config: &AppConfig) -> Result<(), CoreError> {
    let rendered = toml::to_string_pretty(config).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to serialize ORCHESTRATOR_CONFIG for {}: {err}",
            path.display()
        ))
    })?;

    std::fs::write(path, rendered.as_bytes()).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to write ORCHESTRATOR_CONFIG to {}: {err}",
            path.display()
        ))
    })
}

fn load_or_create_config(path: &std::path::Path) -> Result<AppConfig, CoreError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|err| {
                        CoreError::Configuration(format!(
                            "Failed to create parent directory {} for ORCHESTRATOR_CONFIG: {err}",
                            parent.display()
                        ))
                    })?;
                }
            }

            let default_config = AppConfig::default();
            persist_config(path, &default_config)?;

            toml::to_string_pretty(&default_config).map_err(|err| {
                CoreError::Configuration(format!(
                    "Failed to serialize default ORCHESTRATOR_CONFIG: {err}"
                ))
            })?
        }
        Err(err) => {
            return Err(CoreError::Configuration(format!(
                "Failed to read ORCHESTRATOR_CONFIG from {}: {err}",
                path.display()
            )));
        }
    };

    let mut config: AppConfig = toml::from_str(&raw).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to parse ORCHESTRATOR_CONFIG from {}: {err}",
            path.display()
        ))
    })?;

    let changed = normalize_config(&mut config);

    if changed {
        persist_config(path, &config)?;
    }

    Ok(config)
}

fn normalize_config(config: &mut AppConfig) -> bool {
    let mut changed = false;

    if config.workspace.trim() == LEGACY_DEFAULT_WORKSPACE_PATH || config.workspace.trim().is_empty()
    {
        config.workspace = default_workspace_path();
        changed = true;
    }
    if config.event_store_path.trim() == LEGACY_DEFAULT_EVENT_STORE_PATH
        || config.event_store_path.trim().is_empty()
    {
        config.event_store_path = default_event_store_path();
        changed = true;
    }
    if config.ticketing_provider.trim().is_empty() {
        config.ticketing_provider = default_ticketing_provider();
        changed = true;
    } else {
        let normalized = config.ticketing_provider.trim().to_ascii_lowercase();
        if normalized != config.ticketing_provider {
            config.ticketing_provider = normalized;
            changed = true;
        }
    }
    if config.harness_provider.trim().is_empty() {
        config.harness_provider = default_harness_provider();
        changed = true;
    } else {
        let normalized = config.harness_provider.trim().to_ascii_lowercase();
        if normalized != config.harness_provider {
            config.harness_provider = normalized;
            changed = true;
        }
    }

    changed |= normalize_non_empty_string(
        &mut config.supervisor.model,
        default_supervisor_model(),
    );
    changed |= normalize_non_empty_string(
        &mut config.supervisor.openrouter_base_url,
        default_openrouter_base_url(),
    );
    changed |= normalize_non_empty_string(&mut config.linear.api_url, default_linear_api_url());
    if config.linear.sync_interval_secs == 0 {
        config.linear.sync_interval_secs = default_linear_sync_interval_secs();
        changed = true;
    }
    if config.linear.fetch_limit == 0 {
        config.linear.fetch_limit = default_linear_fetch_limit();
        changed = true;
    }
    changed |= normalize_string_vec(&mut config.linear.sync_states);
    if config.linear.workflow_state_map.is_empty() {
        config.linear.workflow_state_map = default_linear_workflow_state_map();
        changed = true;
    } else {
        for entry in &mut config.linear.workflow_state_map {
            if normalize_non_empty_string(&mut entry.workflow_state, String::new()) {
                changed = true;
            }
            if normalize_non_empty_string(&mut entry.linear_state, String::new()) {
                changed = true;
            }
        }
        config.linear.workflow_state_map.retain(|entry| {
            !entry.workflow_state.is_empty() && !entry.linear_state.is_empty()
        });
        if config.linear.workflow_state_map.is_empty() {
            config.linear.workflow_state_map = default_linear_workflow_state_map();
            changed = true;
        }
    }

    changed |= normalize_non_empty_string(&mut config.shortcut.api_url, default_shortcut_api_url());
    if config.shortcut.fetch_limit == 0 {
        config.shortcut.fetch_limit = default_shortcut_fetch_limit();
        changed = true;
    }

    changed |= normalize_non_empty_string(&mut config.git.binary, default_git_binary());
    changed |= normalize_non_empty_string(&mut config.github.binary, default_gh_binary());

    if config.runtime.harness_server_startup_timeout_secs == 0 {
        config.runtime.harness_server_startup_timeout_secs =
            default_harness_server_startup_timeout_secs();
        changed = true;
    }
    changed |= normalize_non_empty_string(&mut config.runtime.opencode_binary, default_opencode_binary());
    changed |= normalize_non_empty_string(
        &mut config.runtime.opencode_server_base_url,
        default_opencode_server_base_url(),
    );
    changed |= normalize_non_empty_string(&mut config.runtime.codex_binary, default_codex_binary());
    if config.runtime.event_retention_days == 0 {
        config.runtime.event_retention_days = default_event_retention_days();
        changed = true;
    }

    changed |= normalize_non_empty_string(&mut config.ui.theme, default_ui_theme());
    changed |= normalize_string_vec(&mut config.ui.ticket_picker_priority_states);
    if config.ui.ticket_picker_priority_states.is_empty() {
        config.ui.ticket_picker_priority_states = default_ticket_picker_priority_states();
        changed = true;
    }
    let normalized_transcript_line_limit = config.ui.transcript_line_limit.max(1);
    if normalized_transcript_line_limit != config.ui.transcript_line_limit {
        config.ui.transcript_line_limit = normalized_transcript_line_limit;
        changed = true;
    }
    let normalized_background_refresh_secs = config
        .ui
        .background_session_refresh_secs
        .clamp(2, 15);
    if normalized_background_refresh_secs != config.ui.background_session_refresh_secs {
        config.ui.background_session_refresh_secs = normalized_background_refresh_secs;
        changed = true;
    }
    let normalized_session_info_background_refresh_secs =
        config.ui.session_info_background_refresh_secs.max(15);
    if normalized_session_info_background_refresh_secs
        != config.ui.session_info_background_refresh_secs
    {
        config.ui.session_info_background_refresh_secs =
            normalized_session_info_background_refresh_secs;
        changed = true;
    }
    let normalized_merge_poll_base_interval_secs = config.ui.merge_poll_base_interval_secs.clamp(5, 300);
    if normalized_merge_poll_base_interval_secs != config.ui.merge_poll_base_interval_secs {
        config.ui.merge_poll_base_interval_secs = normalized_merge_poll_base_interval_secs;
        changed = true;
    }
    let normalized_merge_poll_max_backoff_secs = config.ui.merge_poll_max_backoff_secs.clamp(15, 900);
    if normalized_merge_poll_max_backoff_secs != config.ui.merge_poll_max_backoff_secs {
        config.ui.merge_poll_max_backoff_secs = normalized_merge_poll_max_backoff_secs;
        changed = true;
    }
    let normalized_merge_poll_backoff_multiplier = config.ui.merge_poll_backoff_multiplier.clamp(1, 8);
    if normalized_merge_poll_backoff_multiplier != config.ui.merge_poll_backoff_multiplier {
        config.ui.merge_poll_backoff_multiplier = normalized_merge_poll_backoff_multiplier;
        changed = true;
    }

    changed
}

fn normalize_non_empty_string(value: &mut String, default: String) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        if *value != default {
            *value = default;
            return true;
        }
        return false;
    }

    if trimmed != value {
        *value = trimmed.to_owned();
        return true;
    }
    false
}

fn normalize_string_vec(values: &mut Vec<String>) -> bool {
    let normalized = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if *values != normalized {
        *values = normalized;
        return true;
    }
    false
}

fn ensure_event_store_parent_dir(path: &str) -> Result<(), CoreError> {
    let parent = std::path::PathBuf::from(path)
        .parent()
        .map(std::path::Path::to_path_buf);

    if let Some(parent) = parent {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(&parent).map_err(|err| {
                CoreError::Configuration(format!(
                    "Failed to create parent directory {} for event store: {err}",
                    parent.display()
                ))
            })?;
        }
    }

    Ok(())
}

fn open_event_store(path: &str) -> Result<SqliteEventStore, CoreError> {
    ensure_event_store_parent_dir(path)?;
    SqliteEventStore::open(path)
}
