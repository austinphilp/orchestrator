use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const ENV_ORCHESTRATOR_CONFIG: &str = "ORCHESTRATOR_CONFIG";

const LEGACY_DEFAULT_WORKSPACE_PATH: &str = "./";
const LEGACY_DEFAULT_EVENT_STORE_PATH: &str = "./orchestrator-events.db";
const DEFAULT_TICKETING_PROVIDER: &str = "ticketing.linear";
const DEFAULT_HARNESS_PROVIDER: &str = "harness.codex";
const DEFAULT_VCS_PROVIDER: &str = "vcs.git_cli";
const DEFAULT_VCS_REPO_PROVIDER: &str = "vcs_repos.github_gh_cli";
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
const DEFAULT_PR_PIPELINE_POLL_INTERVAL_SECS: u64 = 15;
const DEFAULT_DATABASE_MAX_CONNECTIONS: u32 = 8;
const DEFAULT_DATABASE_BUSY_TIMEOUT_MS: u64 = 5000;
const DEFAULT_DATABASE_WAL_ENABLED: bool = true;
const DEFAULT_DATABASE_SYNCHRONOUS: &str = "NORMAL";
const DEFAULT_DATABASE_CHUNK_EVENT_FLUSH_MS: u64 = 250;
const DEFAULT_UI_THEME: &str = "nord";
const DEFAULT_UI_TRANSCRIPT_LINE_LIMIT: usize = 100;
const DEFAULT_TICKET_PICKER_PRIORITY_STATES: &[&str] =
    &["In Progress", "Final Approval", "Todo", "Backlog"];

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    Message(String),
}

impl ConfigError {
    fn configuration(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestratorConfig {
    pub workspace: String,
    #[serde(default = "default_event_store_path")]
    pub event_store_path: String,
    #[serde(default = "default_ticketing_provider")]
    pub ticketing_provider: String,
    #[serde(default = "default_harness_provider")]
    pub harness_provider: String,
    #[serde(default = "default_vcs_provider")]
    pub vcs_provider: String,
    #[serde(default = "default_vcs_repo_provider")]
    pub vcs_repo_provider: String,
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
    pub database: DatabaseConfigToml,
    #[serde(default)]
    pub ui: UiConfigToml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSelectionConfig {
    pub ticketing_provider: String,
    pub harness_provider: String,
    pub vcs_provider: String,
    pub vcs_repo_provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorRuntimeConfig {
    pub model: String,
    pub openrouter_base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRuntimeConfig {
    pub binary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseRuntimeConfig {
    pub max_connections: u32,
    pub busy_timeout_ms: u64,
    pub wal_enabled: bool,
    pub synchronous: String,
    pub chunk_event_flush_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiViewConfig {
    pub theme: String,
    pub ticket_picker_priority_states: Vec<String>,
    pub transcript_line_limit: usize,
    pub background_session_refresh_secs: u64,
    pub session_info_background_refresh_secs: u64,
    pub merge_poll_base_interval_secs: u64,
    pub merge_poll_max_backoff_secs: u64,
    pub merge_poll_backoff_multiplier: u64,
}

impl OrchestratorConfig {
    pub fn providers(&self) -> ProviderSelectionConfig {
        ProviderSelectionConfig {
            ticketing_provider: self.ticketing_provider.clone(),
            harness_provider: self.harness_provider.clone(),
            vcs_provider: self.vcs_provider.clone(),
            vcs_repo_provider: self.vcs_repo_provider.clone(),
        }
    }

    pub fn supervisor_runtime(&self) -> SupervisorRuntimeConfig {
        SupervisorRuntimeConfig {
            model: self.supervisor.model.clone(),
            openrouter_base_url: self.supervisor.openrouter_base_url.clone(),
        }
    }

    pub fn git_runtime(&self) -> GitRuntimeConfig {
        GitRuntimeConfig {
            binary: self.git.binary.clone(),
        }
    }

    pub fn database_runtime(&self) -> DatabaseRuntimeConfig {
        DatabaseRuntimeConfig {
            max_connections: self.database.max_connections,
            busy_timeout_ms: self.database.busy_timeout_ms,
            wal_enabled: self.database.wal_enabled,
            synchronous: self.database.synchronous.clone(),
            chunk_event_flush_ms: self.database.chunk_event_flush_ms,
        }
    }

    pub fn ui_view(&self) -> UiViewConfig {
        UiViewConfig {
            theme: self.ui.theme.clone(),
            ticket_picker_priority_states: self.ui.ticket_picker_priority_states.clone(),
            transcript_line_limit: self.ui.transcript_line_limit,
            background_session_refresh_secs: self.ui.background_session_refresh_secs,
            session_info_background_refresh_secs: self.ui.session_info_background_refresh_secs,
            merge_poll_base_interval_secs: self.ui.merge_poll_base_interval_secs,
            merge_poll_max_backoff_secs: self.ui.merge_poll_max_backoff_secs,
            merge_poll_backoff_multiplier: self.ui.merge_poll_backoff_multiplier,
        }
    }
}

pub fn load_from_env() -> Result<OrchestratorConfig, ConfigError> {
    let path = config_path_from_env()?;
    load_from_path(path)
}

pub fn load_from_path(path: impl AsRef<Path>) -> Result<OrchestratorConfig, ConfigError> {
    load_or_create_config(path.as_ref())
}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    let home = resolve_home_dir().ok_or_else(|| {
        ConfigError::configuration("Unable to resolve home directory from HOME or USERPROFILE")
    })?;

    Ok(home
        .join(".config")
        .join("orchestrator")
        .join("config.toml"))
}

fn config_path_from_env() -> Result<PathBuf, ConfigError> {
    match std::env::var(ENV_ORCHESTRATOR_CONFIG) {
        Ok(raw) => {
            if raw.trim().is_empty() {
                default_config_path()
            } else {
                Ok(raw.into())
            }
        }
        Err(std::env::VarError::NotPresent) => default_config_path(),
        Err(_) => Err(ConfigError::configuration(
            "ORCHESTRATOR_CONFIG contained invalid UTF-8",
        )),
    }
}

fn default_orchestrator_data_dir() -> PathBuf {
    resolve_data_local_dir().join("orchestrator")
}

fn resolve_data_local_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(path) = std::env::var("LOCALAPPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(PathBuf::from(path));
            }
        }
        if let Ok(path) = std::env::var("APPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(PathBuf::from(path));
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
                return absolutize_path(PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join(".local").join("share");
        }
    }

    std::env::temp_dir()
}

fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

fn absolutize_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }

    if let Ok(current) = std::env::current_dir() {
        return current.join(path);
    }

    std::env::temp_dir().join(path)
}

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

fn default_vcs_provider() -> String {
    DEFAULT_VCS_PROVIDER.to_owned()
}

fn default_vcs_repo_provider() -> String {
    DEFAULT_VCS_REPO_PROVIDER.to_owned()
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
    #[serde(default = "default_pr_pipeline_poll_interval_secs")]
    pub pr_pipeline_poll_interval_secs: u64,
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
            pr_pipeline_poll_interval_secs: default_pr_pipeline_poll_interval_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DatabaseConfigToml {
    #[serde(default = "default_database_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_database_busy_timeout_ms")]
    pub busy_timeout_ms: u64,
    #[serde(default = "default_database_wal_enabled")]
    pub wal_enabled: bool,
    #[serde(default = "default_database_synchronous")]
    pub synchronous: String,
    #[serde(default = "default_database_chunk_event_flush_ms")]
    pub chunk_event_flush_ms: u64,
}

impl Default for DatabaseConfigToml {
    fn default() -> Self {
        Self {
            max_connections: default_database_max_connections(),
            busy_timeout_ms: default_database_busy_timeout_ms(),
            wal_enabled: default_database_wal_enabled(),
            synchronous: default_database_synchronous(),
            chunk_event_flush_ms: default_database_chunk_event_flush_ms(),
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

fn default_pr_pipeline_poll_interval_secs() -> u64 {
    DEFAULT_PR_PIPELINE_POLL_INTERVAL_SECS
}

fn default_database_max_connections() -> u32 {
    DEFAULT_DATABASE_MAX_CONNECTIONS
}

fn default_database_busy_timeout_ms() -> u64 {
    DEFAULT_DATABASE_BUSY_TIMEOUT_MS
}

fn default_database_wal_enabled() -> bool {
    DEFAULT_DATABASE_WAL_ENABLED
}

fn default_database_synchronous() -> String {
    DEFAULT_DATABASE_SYNCHRONOUS.to_owned()
}

fn default_database_chunk_event_flush_ms() -> u64 {
    DEFAULT_DATABASE_CHUNK_EVENT_FLUSH_MS
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

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            workspace: default_workspace_path(),
            event_store_path: default_event_store_path(),
            ticketing_provider: default_ticketing_provider(),
            harness_provider: default_harness_provider(),
            vcs_provider: default_vcs_provider(),
            vcs_repo_provider: default_vcs_repo_provider(),
            supervisor: SupervisorConfig::default(),
            linear: LinearConfigToml::default(),
            shortcut: ShortcutConfigToml::default(),
            git: GitConfigToml::default(),
            github: GithubConfigToml::default(),
            runtime: RuntimeConfigToml::default(),
            database: DatabaseConfigToml::default(),
            ui: UiConfigToml::default(),
        }
    }
}

fn persist_config(path: &Path, config: &OrchestratorConfig) -> Result<(), ConfigError> {
    let rendered = toml::to_string_pretty(config).map_err(|err| {
        ConfigError::configuration(format!(
            "Failed to serialize ORCHESTRATOR_CONFIG for {}: {err}",
            path.display()
        ))
    })?;

    std::fs::write(path, rendered.as_bytes()).map_err(|err| {
        ConfigError::configuration(format!(
            "Failed to write ORCHESTRATOR_CONFIG to {}: {err}",
            path.display()
        ))
    })
}

fn load_or_create_config(path: &Path) -> Result<OrchestratorConfig, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|err| {
                        ConfigError::configuration(format!(
                            "Failed to create parent directory {} for ORCHESTRATOR_CONFIG: {err}",
                            parent.display()
                        ))
                    })?;
                }
            }

            let default_config = OrchestratorConfig::default();
            persist_config(path, &default_config)?;

            toml::to_string_pretty(&default_config).map_err(|err| {
                ConfigError::configuration(format!(
                    "Failed to serialize default ORCHESTRATOR_CONFIG: {err}"
                ))
            })?
        }
        Err(err) => {
            return Err(ConfigError::configuration(format!(
                "Failed to read ORCHESTRATOR_CONFIG from {}: {err}",
                path.display()
            )));
        }
    };

    let mut config: OrchestratorConfig = toml::from_str(&raw).map_err(|err| {
        ConfigError::configuration(format!(
            "Failed to parse ORCHESTRATOR_CONFIG from {}: {err}",
            path.display()
        ))
    })?;

    let changed = normalize_config(&mut config)?;
    if changed {
        persist_config(path, &config)?;
    }

    Ok(config)
}

fn normalize_config(config: &mut OrchestratorConfig) -> Result<bool, ConfigError> {
    let mut changed = false;

    if config.workspace.trim() == LEGACY_DEFAULT_WORKSPACE_PATH
        || config.workspace.trim().is_empty()
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
    changed |= normalize_provider_selection(
        &mut config.ticketing_provider,
        DEFAULT_TICKETING_PROVIDER,
        "ticketing_provider",
        "ticketing",
    )?;
    changed |= normalize_provider_selection(
        &mut config.harness_provider,
        DEFAULT_HARNESS_PROVIDER,
        "harness_provider",
        "harness",
    )?;
    changed |= normalize_provider_selection(
        &mut config.vcs_provider,
        DEFAULT_VCS_PROVIDER,
        "vcs_provider",
        "vcs",
    )?;
    changed |= normalize_provider_selection(
        &mut config.vcs_repo_provider,
        DEFAULT_VCS_REPO_PROVIDER,
        "vcs_repo_provider",
        "vcs_repos",
    )?;

    changed |= normalize_supervisor_model(&mut config.supervisor.model);
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
        config
            .linear
            .workflow_state_map
            .retain(|entry| !entry.workflow_state.is_empty() && !entry.linear_state.is_empty());
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
    changed |= normalize_non_empty_string(
        &mut config.runtime.opencode_binary,
        default_opencode_binary(),
    );
    changed |= normalize_non_empty_string(
        &mut config.runtime.opencode_server_base_url,
        default_opencode_server_base_url(),
    );
    changed |= normalize_non_empty_string(&mut config.runtime.codex_binary, default_codex_binary());
    if config.runtime.event_retention_days == 0 {
        config.runtime.event_retention_days = default_event_retention_days();
        changed = true;
    }
    let normalized_pr_pipeline_poll_interval_secs =
        config.runtime.pr_pipeline_poll_interval_secs.clamp(1, 300);
    if normalized_pr_pipeline_poll_interval_secs != config.runtime.pr_pipeline_poll_interval_secs {
        config.runtime.pr_pipeline_poll_interval_secs = normalized_pr_pipeline_poll_interval_secs;
        changed = true;
    }

    changed |= normalize_database_config(&mut config.database);
    changed |= normalize_ui_config(&mut config.ui);

    Ok(changed)
}

pub fn normalize_supervisor_model(value: &mut String) -> bool {
    normalize_non_empty_string(value, default_supervisor_model())
}

pub fn normalize_database_config(config: &mut DatabaseConfigToml) -> bool {
    let mut changed = false;

    let normalized_max_connections = if config.max_connections == 0 {
        default_database_max_connections()
    } else {
        config.max_connections.clamp(1, 64)
    };
    if normalized_max_connections != config.max_connections {
        config.max_connections = normalized_max_connections;
        changed = true;
    }

    let normalized_busy_timeout_ms = if config.busy_timeout_ms == 0 {
        default_database_busy_timeout_ms()
    } else {
        config.busy_timeout_ms.clamp(100, 60_000)
    };
    if normalized_busy_timeout_ms != config.busy_timeout_ms {
        config.busy_timeout_ms = normalized_busy_timeout_ms;
        changed = true;
    }

    let normalized_chunk_event_flush_ms = if config.chunk_event_flush_ms == 0 {
        default_database_chunk_event_flush_ms()
    } else {
        config.chunk_event_flush_ms.clamp(50, 5_000)
    };
    if normalized_chunk_event_flush_ms != config.chunk_event_flush_ms {
        config.chunk_event_flush_ms = normalized_chunk_event_flush_ms;
        changed = true;
    }

    let normalized_synchronous = normalize_database_synchronous(config.synchronous.as_str());
    if normalized_synchronous != config.synchronous {
        config.synchronous = normalized_synchronous;
        changed = true;
    }

    changed
}

pub fn normalize_ui_config(config: &mut UiConfigToml) -> bool {
    let mut changed = false;

    changed |= normalize_non_empty_string(&mut config.theme, default_ui_theme());
    changed |= normalize_string_vec(&mut config.ticket_picker_priority_states);
    if config.ticket_picker_priority_states.is_empty() {
        config.ticket_picker_priority_states = default_ticket_picker_priority_states();
        changed = true;
    }

    let normalized_transcript_line_limit = config.transcript_line_limit.max(1);
    if normalized_transcript_line_limit != config.transcript_line_limit {
        config.transcript_line_limit = normalized_transcript_line_limit;
        changed = true;
    }

    let normalized_background_refresh_secs = config.background_session_refresh_secs.clamp(2, 15);
    if normalized_background_refresh_secs != config.background_session_refresh_secs {
        config.background_session_refresh_secs = normalized_background_refresh_secs;
        changed = true;
    }

    let normalized_session_info_background_refresh_secs =
        config.session_info_background_refresh_secs.max(15);
    if normalized_session_info_background_refresh_secs
        != config.session_info_background_refresh_secs
    {
        config.session_info_background_refresh_secs =
            normalized_session_info_background_refresh_secs;
        changed = true;
    }

    let normalized_merge_poll_base_interval_secs =
        config.merge_poll_base_interval_secs.clamp(5, 300);
    if normalized_merge_poll_base_interval_secs != config.merge_poll_base_interval_secs {
        config.merge_poll_base_interval_secs = normalized_merge_poll_base_interval_secs;
        changed = true;
    }

    let normalized_merge_poll_max_backoff_secs = config.merge_poll_max_backoff_secs.clamp(15, 900);
    if normalized_merge_poll_max_backoff_secs != config.merge_poll_max_backoff_secs {
        config.merge_poll_max_backoff_secs = normalized_merge_poll_max_backoff_secs;
        changed = true;
    }

    let normalized_merge_poll_backoff_multiplier = config.merge_poll_backoff_multiplier.clamp(1, 8);
    if normalized_merge_poll_backoff_multiplier != config.merge_poll_backoff_multiplier {
        config.merge_poll_backoff_multiplier = normalized_merge_poll_backoff_multiplier;
        changed = true;
    }

    changed
}

fn normalize_provider_selection(
    value: &mut String,
    default: &str,
    field_name: &str,
    provider_namespace: &str,
) -> Result<bool, ConfigError> {
    let normalized = value.trim().to_ascii_lowercase();
    let canonical = if normalized.is_empty() {
        default.to_owned()
    } else {
        normalized
    };
    let expected_prefix = format!("{provider_namespace}.");

    if !canonical.starts_with(expected_prefix.as_str()) {
        return Err(ConfigError::configuration(format!(
            "Invalid `{field_name}` value '{canonical}' in ORCHESTRATOR_CONFIG: provider keys must be namespaced under `{expected_prefix}*` (for example `{default}`)."
        )));
    }
    let suffix = canonical[expected_prefix.len()..].trim();
    if suffix.is_empty()
        || suffix.split('.').any(|segment| {
            segment.is_empty()
                || !segment.chars().all(|ch| {
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-'
                })
        })
    {
        return Err(ConfigError::configuration(format!(
            "Invalid `{field_name}` value '{canonical}' in ORCHESTRATOR_CONFIG: expected format `{provider_namespace}.<provider_key>` where each key segment contains only lowercase letters, digits, `_`, or `-`."
        )));
    }

    if *value != canonical {
        *value = canonical;
        return Ok(true);
    }

    Ok(false)
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

fn normalize_database_synchronous(value: &str) -> String {
    let candidate = value.trim().to_ascii_uppercase();
    match candidate.as_str() {
        "OFF" | "NORMAL" | "FULL" | "EXTRA" => candidate,
        _ => default_database_synchronous(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_env_vars<F>(vars: &[(&str, Option<&str>)], test: F)
    where
        F: FnOnce(),
    {
        let _guard = env_lock().lock().expect("env lock");
        let backup = vars
            .iter()
            .map(|(name, _)| ((*name).to_owned(), std::env::var(name).ok()))
            .collect::<Vec<_>>();

        for (name, value) in vars {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }

        test();

        for (name, value) in backup {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "orchestrator-config-{prefix}-{nanos}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn remove_temp_path(path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    fn write_config_file(path: &Path, raw: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture config parent");
        }
        std::fs::write(path, raw.as_bytes()).expect("write fixture config");
    }

    #[test]
    fn load_from_env_creates_default_config_when_missing() {
        let home = unique_temp_dir("home-defaults");
        let expected = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().expect("home path"))),
                ("USERPROFILE", None),
                (ENV_ORCHESTRATOR_CONFIG, None),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = load_from_env().expect("load defaults");
                assert_eq!(config.ticketing_provider, "ticketing.linear");
                assert_eq!(config.harness_provider, "harness.codex");
                assert_eq!(config.vcs_provider, "vcs.git_cli");
                assert_eq!(config.vcs_repo_provider, "vcs_repos.github_gh_cli");
                assert!(expected.exists());
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn load_from_env_honors_explicit_orchestrator_config_path() {
        let home = unique_temp_dir("home-explicit-path");
        let root = unique_temp_dir("explicit-path");
        let explicit = root.join("nested").join("custom.toml");
        let default = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().expect("home path"))),
                ("USERPROFILE", None),
                (
                    ENV_ORCHESTRATOR_CONFIG,
                    Some(explicit.to_str().expect("config path")),
                ),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = load_from_env().expect("load explicit path config");
                assert!(explicit.exists());
                assert!(!default.exists());
                assert_eq!(config.ticketing_provider, "ticketing.linear");
            },
        );

        remove_temp_path(&home);
        remove_temp_path(&root);
    }

    #[test]
    fn load_from_env_treats_blank_orchestrator_config_as_unset() {
        let home = unique_temp_dir("home-blank-path");
        let expected = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().expect("home path"))),
                ("USERPROFILE", None),
                (ENV_ORCHESTRATOR_CONFIG, Some("  ")),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = load_from_env().expect("load config from default path");
                assert!(expected.exists());
                assert_eq!(config.ticketing_provider, "ticketing.linear");
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn default_config_path_falls_back_to_userprofile_when_home_is_blank() {
        let userprofile = unique_temp_dir("userprofile-default-path");
        let expected = userprofile
            .join(".config")
            .join("orchestrator")
            .join("config.toml");

        with_env_vars(
            &[
                ("HOME", Some(" ")),
                (
                    "USERPROFILE",
                    Some(userprofile.to_str().expect("userprofile path")),
                ),
            ],
            || {
                let resolved = default_config_path().expect("resolve default config path");
                assert_eq!(resolved, expected);
            },
        );

        remove_temp_path(&userprofile);
    }

    #[test]
    fn load_from_path_rejects_legacy_provider_aliases() {
        let cases = [
            ("ticketing_provider", "linear", "ticketing"),
            ("harness_provider", "codex", "harness"),
            ("vcs_provider", "git", "vcs"),
            ("vcs_repo_provider", "github_gh_cli", "vcs_repos"),
        ];

        for (field, legacy_alias, namespace) in cases {
            let root = unique_temp_dir(&format!("legacy-aliases-{field}"));
            let path = root.join("config.toml");
            let mut ticketing = "ticketing.linear";
            let mut harness = "harness.codex";
            let mut vcs = "vcs.git_cli";
            let mut vcs_repos = "vcs_repos.github_gh_cli";
            match field {
                "ticketing_provider" => ticketing = legacy_alias,
                "harness_provider" => harness = legacy_alias,
                "vcs_provider" => vcs = legacy_alias,
                "vcs_repo_provider" => vcs_repos = legacy_alias,
                _ => unreachable!("unexpected provider field"),
            }
            write_config_file(
                &path,
                format!(
                    "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\nticketing_provider='{ticketing}'\nharness_provider='{harness}'\nvcs_provider='{vcs}'\nvcs_repo_provider='{vcs_repos}'\n"
                )
                .as_str(),
            );

            let error = load_from_path(&path).expect_err("legacy aliases should be rejected");
            let detail = error.to_string();
            assert!(detail.contains(field));
            assert!(detail.contains(format!("{namespace}.*").as_str()));

            remove_temp_path(&root);
        }
    }

    #[test]
    fn load_from_path_rejects_malformed_namespaced_provider_key() {
        let root = unique_temp_dir("malformed-provider-key");
        let path = root.join("config.toml");
        write_config_file(
            &path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\nticketing_provider='ticketing.'\nharness_provider='harness.codex'\nvcs_provider='vcs.git_cli'\nvcs_repo_provider='vcs_repos.github_gh_cli'\n",
        );

        let error = load_from_path(&path).expect_err("malformed provider key should be rejected");
        let detail = error.to_string();
        assert!(detail.contains("ticketing_provider"));
        assert!(detail.contains("expected format `ticketing.<provider_key>`"));

        remove_temp_path(&root);
    }

    #[test]
    fn load_from_path_returns_parse_error_for_invalid_toml() {
        let root = unique_temp_dir("invalid");
        let path = root.join("config.toml");
        write_config_file(&path, "workspace = '/tmp/work'\nevent_store_path = [\n");

        let error = load_from_path(&path).expect_err("expected parse failure");
        assert!(error
            .to_string()
            .contains("Failed to parse ORCHESTRATOR_CONFIG"));

        remove_temp_path(&root);
    }

    #[test]
    fn ui_view_slice_exposes_ui_fields() {
        let config = OrchestratorConfig {
            ui: UiConfigToml {
                theme: "default".to_owned(),
                ticket_picker_priority_states: vec!["In Progress".to_owned()],
                transcript_line_limit: 200,
                background_session_refresh_secs: 5,
                session_info_background_refresh_secs: 30,
                merge_poll_base_interval_secs: 10,
                merge_poll_max_backoff_secs: 120,
                merge_poll_backoff_multiplier: 2,
            },
            ..OrchestratorConfig::default()
        };

        let view = config.ui_view();

        assert_eq!(view.theme, "default");
        assert_eq!(view.ticket_picker_priority_states, vec!["In Progress"]);
        assert_eq!(view.transcript_line_limit, 200);
        assert_eq!(view.background_session_refresh_secs, 5);
    }

    #[test]
    fn typed_config_slices_expose_expected_fields() {
        let config = OrchestratorConfig {
            ticketing_provider: "ticketing.shortcut".to_owned(),
            harness_provider: "harness.opencode".to_owned(),
            vcs_provider: "vcs.git_cli".to_owned(),
            vcs_repo_provider: "vcs_repos.github_gh_cli".to_owned(),
            supervisor: SupervisorConfig {
                model: "c/gpt-5-codex".to_owned(),
                openrouter_base_url: "https://example.invalid/router".to_owned(),
            },
            git: GitConfigToml {
                binary: "/usr/local/bin/git-custom".to_owned(),
                allow_delete_unmerged_branches: false,
                allow_destructive_automation: true,
                allow_force_push: false,
            },
            database: DatabaseConfigToml {
                max_connections: 16,
                busy_timeout_ms: 1_500,
                wal_enabled: false,
                synchronous: "FULL".to_owned(),
                chunk_event_flush_ms: 400,
            },
            ui: UiConfigToml {
                theme: "forest".to_owned(),
                ticket_picker_priority_states: vec!["In Progress".to_owned(), "Backlog".to_owned()],
                transcript_line_limit: 123,
                background_session_refresh_secs: 4,
                session_info_background_refresh_secs: 20,
                merge_poll_base_interval_secs: 8,
                merge_poll_max_backoff_secs: 180,
                merge_poll_backoff_multiplier: 3,
            },
            ..OrchestratorConfig::default()
        };

        let providers = config.providers();
        let supervisor = config.supervisor_runtime();
        let git = config.git_runtime();
        let database = config.database_runtime();
        let ui = config.ui_view();

        assert_eq!(providers.ticketing_provider, "ticketing.shortcut");
        assert_eq!(providers.harness_provider, "harness.opencode");
        assert_eq!(providers.vcs_provider, "vcs.git_cli");
        assert_eq!(providers.vcs_repo_provider, "vcs_repos.github_gh_cli");
        assert_eq!(supervisor.model, "c/gpt-5-codex");
        assert_eq!(
            supervisor.openrouter_base_url,
            "https://example.invalid/router"
        );
        assert_eq!(git.binary, "/usr/local/bin/git-custom");
        assert_eq!(database.max_connections, 16);
        assert_eq!(database.busy_timeout_ms, 1_500);
        assert!(!database.wal_enabled);
        assert_eq!(database.synchronous, "FULL");
        assert_eq!(database.chunk_event_flush_ms, 400);
        assert_eq!(ui.theme, "forest");
        assert_eq!(
            ui.ticket_picker_priority_states,
            vec!["In Progress", "Backlog"]
        );
        assert_eq!(ui.transcript_line_limit, 123);
        assert_eq!(ui.background_session_refresh_secs, 4);
        assert_eq!(ui.session_info_background_refresh_secs, 20);
        assert_eq!(ui.merge_poll_base_interval_secs, 8);
        assert_eq!(ui.merge_poll_max_backoff_secs, 180);
        assert_eq!(ui.merge_poll_backoff_multiplier, 3);
    }

    #[test]
    fn load_from_path_normalizes_and_persists_supported_bounds() {
        let root = unique_temp_dir("normalization");
        let path = root.join("config.toml");
        write_config_file(
            &path,
            r#"
workspace = "/tmp/work"
event_store_path = "/tmp/events.db"
ticketing_provider = "  TICKETING.SHORTCUT  "
harness_provider = "  HARNESS.OPENCODE "
vcs_provider = "  VCS.GIT_CLI  "
vcs_repo_provider = "  VCS_REPOS.GITHUB_GH_CLI "

[linear]
sync_states = [" Ready ", "", "In Progress "]
workflow_state_map = [
  { workflow_state = "  ", linear_state = "In Progress" },
  { workflow_state = "InReview", linear_state = "   " },
]

[runtime]
pr_pipeline_poll_interval_secs = 0

[database]
max_connections = 100
busy_timeout_ms = 1
chunk_event_flush_ms = 99999
synchronous = "unsafe"

[ui]
ticket_picker_priority_states = ["  In Progress  ", "", "Backlog"]
transcript_line_limit = 0
background_session_refresh_secs = 99
session_info_background_refresh_secs = 3
merge_poll_base_interval_secs = 1
merge_poll_max_backoff_secs = 9999
merge_poll_backoff_multiplier = 0
"#,
        );

        let config = load_from_path(&path).expect("load and normalize config");

        assert_eq!(config.ticketing_provider, "ticketing.shortcut");
        assert_eq!(config.harness_provider, "harness.opencode");
        assert_eq!(config.vcs_provider, "vcs.git_cli");
        assert_eq!(config.vcs_repo_provider, "vcs_repos.github_gh_cli");
        assert_eq!(config.linear.sync_states, vec!["Ready", "In Progress"]);
        assert_eq!(
            config.linear.workflow_state_map,
            default_linear_workflow_state_map()
        );
        assert_eq!(config.runtime.pr_pipeline_poll_interval_secs, 1);
        assert_eq!(config.database.max_connections, 64);
        assert_eq!(config.database.busy_timeout_ms, 100);
        assert_eq!(config.database.chunk_event_flush_ms, 5_000);
        assert_eq!(config.database.synchronous, "NORMAL");
        assert_eq!(
            config.ui.ticket_picker_priority_states,
            vec!["In Progress", "Backlog"]
        );
        assert_eq!(config.ui.transcript_line_limit, 1);
        assert_eq!(config.ui.background_session_refresh_secs, 15);
        assert_eq!(config.ui.session_info_background_refresh_secs, 15);
        assert_eq!(config.ui.merge_poll_base_interval_secs, 5);
        assert_eq!(config.ui.merge_poll_max_backoff_secs, 900);
        assert_eq!(config.ui.merge_poll_backoff_multiplier, 1);

        let persisted = std::fs::read_to_string(&path).expect("read persisted config");
        let parsed: OrchestratorConfig =
            toml::from_str(&persisted).expect("parse persisted normalized config");
        assert_eq!(parsed.ticketing_provider, "ticketing.shortcut");
        assert_eq!(parsed.database.max_connections, 64);
        assert_eq!(parsed.ui.merge_poll_max_backoff_secs, 900);
        assert_eq!(parsed.linear.sync_states, vec!["Ready", "In Progress"]);

        remove_temp_path(&root);
    }
}
