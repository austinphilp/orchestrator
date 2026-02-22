use crate::events::{
    InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope, OrchestrationEventPayload,
    SessionCompletedPayload, SessionCrashedPayload, StoredEventEnvelope, WorkflowTransitionPayload,
};
use crate::normalization::DOMAIN_EVENT_SCHEMA_VERSION;
use crate::projection::{rebuild_projection, ProjectionState, SessionRuntimeProjection};
#[cfg(test)]
use orchestrator_config::default_config_path as canonical_default_config_path;
use orchestrator_config::{
    load_from_env as canonical_load_from_env, load_from_path as canonical_load_from_path,
    ConfigError,
};
use orchestrator_core::{
    apply_workflow_transition, CoreError, EventStore, InboxItemId, RuntimeSessionId,
    SelectedTicketFlowConfig, SelectedTicketFlowResult, SessionHandle, SqliteEventStore,
    WorkItemId, WorkerBackend, WorkerSessionId, WorkerSessionStatus, WorkflowGuardContext,
    WorkflowState, WorkflowTransitionReason,
};
use orchestrator_supervisor::{LlmProvider, Supervisor};
use orchestrator_ticketing::{
    GetTicketRequest, LinearTicketingProvider, TicketSummary, TicketingProvider,
};
use orchestrator_ui::{
    InboxPublishRequest, InboxResolveRequest, SessionArchiveOutcome, SessionMergeFinalizeOutcome,
    SessionWorkflowAdvanceOutcome, SupervisorCommandContext, SupervisorCommandDispatcher,
};
use orchestrator_vcs::VcsProvider;
use orchestrator_vcs_repos::{CodeHostProvider, GithubClient};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[path = "../command_dispatch.rs"]
mod command_dispatch;
#[path = "../ticket_picker.rs"]
mod ticket_picker;
pub use orchestrator_config::{
    DatabaseConfigToml, GitConfigToml, GithubConfigToml, LinearConfigToml,
    OrchestratorConfig as AppConfig, RuntimeConfigToml, ShortcutConfigToml, SupervisorConfig,
    UiConfigToml, UiViewConfig, WorkflowStateMapEntry,
};
pub use ticket_picker::AppTicketPickerProvider;

const DEFAULT_SUPERVISOR_MODEL: &str = "c/claude-haiku-4.5";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupState {
    pub status: String,
    pub projection: ProjectionState,
}

pub fn load_app_config_from_env() -> Result<AppConfig, CoreError> {
    let config = canonical_load_from_env().map_err(config_error_to_core)?;
    set_database_runtime_config(config.database.clone());
    Ok(config)
}

pub fn load_app_config_from_path(path: &std::path::Path) -> Result<AppConfig, CoreError> {
    let config = canonical_load_from_path(path).map_err(config_error_to_core)?;
    set_database_runtime_config(config.database.clone());
    Ok(config)
}

fn config_error_to_core(error: ConfigError) -> CoreError {
    CoreError::Configuration(error.to_string())
}

#[cfg(test)]
fn default_config_path() -> Result<std::path::PathBuf, CoreError> {
    canonical_default_config_path().map_err(config_error_to_core)
}

fn default_database_max_connections() -> u32 {
    DatabaseConfigToml::default().max_connections
}

fn default_database_busy_timeout_ms() -> u64 {
    DatabaseConfigToml::default().busy_timeout_ms
}

fn default_database_chunk_event_flush_ms() -> u64 {
    DatabaseConfigToml::default().chunk_event_flush_ms
}

fn default_database_synchronous() -> String {
    DatabaseConfigToml::default().synchronous
}

fn normalize_database_synchronous(value: &str) -> String {
    let candidate = value.trim().to_ascii_uppercase();
    match candidate.as_str() {
        "OFF" | "NORMAL" | "FULL" | "EXTRA" => candidate,
        _ => default_database_synchronous(),
    }
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

pub(crate) type AppEventStore = SqliteEventStore<PooledConnection<SqliteConnectionManager>>;

fn event_store_pools() -> &'static Mutex<HashMap<String, Pool<SqliteConnectionManager>>> {
    static POOLS: OnceLock<Mutex<HashMap<String, Pool<SqliteConnectionManager>>>> = OnceLock::new();
    POOLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn database_runtime_config_cell() -> &'static Mutex<DatabaseConfigToml> {
    static CONFIG: OnceLock<Mutex<DatabaseConfigToml>> = OnceLock::new();
    CONFIG.get_or_init(|| Mutex::new(DatabaseConfigToml::default()))
}

pub fn set_database_runtime_config(config: DatabaseConfigToml) {
    let mut normalized = config;
    normalized.max_connections = if normalized.max_connections == 0 {
        default_database_max_connections()
    } else {
        normalized.max_connections.clamp(1, 64)
    };
    normalized.busy_timeout_ms = if normalized.busy_timeout_ms == 0 {
        default_database_busy_timeout_ms()
    } else {
        normalized.busy_timeout_ms.clamp(100, 60_000)
    };
    normalized.chunk_event_flush_ms = if normalized.chunk_event_flush_ms == 0 {
        default_database_chunk_event_flush_ms()
    } else {
        normalized.chunk_event_flush_ms.clamp(50, 5_000)
    };
    normalized.synchronous = normalize_database_synchronous(normalized.synchronous.as_str());

    {
        let mut guard = database_runtime_config_cell()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = normalized;
    }

    let mut pools = event_store_pools()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pools.clear();
}

fn database_runtime_config() -> DatabaseConfigToml {
    database_runtime_config_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn build_event_store_pool(
    path: &str,
    config: &DatabaseConfigToml,
) -> Result<Pool<SqliteConnectionManager>, CoreError> {
    let manager = SqliteConnectionManager::file(path);
    let pool = Pool::builder()
        .max_size(config.max_connections)
        .build(manager)
        .map_err(|err| CoreError::Persistence(format!("failed to build sqlite pool: {err}")))?;

    {
        let mut conn = pool.get().map_err(|err| {
            CoreError::Persistence(format!("failed to acquire sqlite connection: {err}"))
        })?;
        apply_sqlite_runtime_pragmas(&mut conn, config)?;
        let _ = SqliteEventStore::from_connection(conn)?;
    }

    Ok(pool)
}

fn apply_sqlite_runtime_pragmas(
    conn: &mut PooledConnection<SqliteConnectionManager>,
    config: &DatabaseConfigToml,
) -> Result<(), CoreError> {
    let journal_mode = if config.wal_enabled { "WAL" } else { "DELETE" };
    conn.execute_batch(
        format!(
            "PRAGMA journal_mode = {journal_mode}; PRAGMA synchronous = {}; PRAGMA busy_timeout = {}; PRAGMA foreign_keys = ON;",
            config.synchronous, config.busy_timeout_ms
        )
        .as_str(),
    )
    .map_err(|err| CoreError::Persistence(err.to_string()))
}

pub(crate) fn supervisor_chunk_event_flush_interval() -> Duration {
    let config = database_runtime_config();
    Duration::from_millis(config.chunk_event_flush_ms)
}

pub(crate) fn open_event_store(path: &str) -> Result<AppEventStore, CoreError> {
    ensure_event_store_parent_dir(path)?;
    let config = database_runtime_config();
    let pool = {
        let mut pools = event_store_pools()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = pools.get(path) {
            existing.clone()
        } else {
            let created = build_event_store_pool(path, &config)?;
            pools.insert(path.to_owned(), created.clone());
            created
        }
    };
    let mut conn = pool.get().map_err(|err| {
        CoreError::Persistence(format!("failed to acquire sqlite connection: {err}"))
    })?;
    apply_sqlite_runtime_pragmas(&mut conn, &config)?;
    Ok(SqliteEventStore::from_initialized_connection(conn))
}

pub(crate) fn open_owned_event_store(path: &str) -> Result<SqliteEventStore, CoreError> {
    ensure_event_store_parent_dir(path)?;
    SqliteEventStore::open(path)
}
