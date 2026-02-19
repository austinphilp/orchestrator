use integration_linear::LinearTicketingProvider;
use orchestrator_core::{
    rebuild_projection, CodeHostProvider, CoreError, EventStore, GithubClient, LlmProvider,
    NewEventEnvelope, OrchestrationEventPayload, ProjectionState, SelectedTicketFlowConfig,
    SelectedTicketFlowResult, SessionCrashedPayload, SqliteEventStore, Supervisor, TicketSummary,
    TicketingProvider, UntypedCommandInvocation, VcsProvider, WorkerBackend, WorkerSessionId,
    WorkerSessionStatus, DOMAIN_EVENT_SCHEMA_VERSION,
};
use orchestrator_ui::{SupervisorCommandContext, SupervisorCommandDispatcher};
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
}

const LEGACY_DEFAULT_WORKSPACE_PATH: &str = "./";
const LEGACY_DEFAULT_EVENT_STORE_PATH: &str = "./orchestrator-events.db";
const DEFAULT_TICKETING_PROVIDER: &str = "linear";
const DEFAULT_HARNESS_PROVIDER: &str = "codex";

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

const DEFAULT_SUPERVISOR_MODEL: &str = "openai/gpt-4o-mini";

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

    let mut changed = false;
    if config.workspace.trim() == LEGACY_DEFAULT_WORKSPACE_PATH {
        config.workspace = default_workspace_path();
        changed = true;
    }
    if config.event_store_path.trim() == LEGACY_DEFAULT_EVENT_STORE_PATH {
        config.event_store_path = default_event_store_path();
        changed = true;
    }
    if config.ticketing_provider.trim().is_empty() {
        config.ticketing_provider = default_ticketing_provider();
        changed = true;
    }
    if config.harness_provider.trim().is_empty() {
        config.harness_provider = default_harness_provider();
        changed = true;
    }

    if changed {
        persist_config(path, &config)?;
    }

    Ok(config)
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
