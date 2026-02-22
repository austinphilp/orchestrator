use crate::events::{
    InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope, OrchestrationEventPayload,
    SessionCompletedPayload, SessionCrashedPayload, StoredEventEnvelope, WorkflowTransitionPayload,
};
use crate::controller::contracts::{
    InboxPublishRequest, InboxResolveRequest, SessionArchiveOutcome, SessionMergeFinalizeOutcome,
    SessionWorkflowAdvanceOutcome, SessionWorktreeDiff, SupervisorCommandContext,
    SupervisorCommandDispatcher,
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
    DatabaseConfigToml, DatabaseRuntimeConfig, GitConfigToml, GitRuntimeConfig, GithubConfigToml,
    LinearConfigToml, OrchestratorConfig as AppConfig, RuntimeConfigToml, ShortcutConfigToml,
    SupervisorConfig, SupervisorRuntimeConfig, UiConfigToml, UiViewConfig, WorkflowStateMapEntry,
};
pub use ticket_picker::AppTicketPickerProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupState {
    pub status: String,
    pub projection: ProjectionState,
}

pub fn load_app_config_from_env() -> Result<AppConfig, CoreError> {
    canonical_load_from_env().map_err(config_error_to_core)
}

pub fn load_app_config_from_path(path: &std::path::Path) -> Result<AppConfig, CoreError> {
    canonical_load_from_path(path).map_err(config_error_to_core)
}

fn config_error_to_core(error: ConfigError) -> CoreError {
    CoreError::Configuration(error.to_string())
}

#[cfg(test)]
fn default_config_path() -> Result<std::path::PathBuf, CoreError> {
    canonical_default_config_path().map_err(config_error_to_core)
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

struct EventStorePoolEntry {
    config: DatabaseRuntimeConfig,
    pool: Pool<SqliteConnectionManager>,
}

fn event_store_pools() -> &'static Mutex<HashMap<String, EventStorePoolEntry>> {
    static POOLS: OnceLock<Mutex<HashMap<String, EventStorePoolEntry>>> = OnceLock::new();
    POOLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn build_event_store_pool(
    path: &str,
    config: &DatabaseRuntimeConfig,
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
    config: &DatabaseRuntimeConfig,
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

pub(crate) fn supervisor_chunk_event_flush_interval(config: &DatabaseRuntimeConfig) -> Duration {
    Duration::from_millis(config.chunk_event_flush_ms)
}

pub(crate) fn open_event_store(
    path: &str,
    config: &DatabaseRuntimeConfig,
) -> Result<AppEventStore, CoreError> {
    ensure_event_store_parent_dir(path)?;
    let pool = {
        let mut pools = event_store_pools()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = pools.get(path) {
            if existing.config == *config {
                existing.pool.clone()
            } else {
                let created = build_event_store_pool(path, config)?;
                pools.insert(
                    path.to_owned(),
                    EventStorePoolEntry {
                        config: config.clone(),
                        pool: created.clone(),
                    },
                );
                created
            }
        } else {
            let created = build_event_store_pool(path, config)?;
            pools.insert(
                path.to_owned(),
                EventStorePoolEntry {
                    config: config.clone(),
                    pool: created.clone(),
                },
            );
            created
        }
    };
    let mut conn = pool.get().map_err(|err| {
        CoreError::Persistence(format!("failed to acquire sqlite connection: {err}"))
    })?;
    apply_sqlite_runtime_pragmas(&mut conn, config)?;
    Ok(SqliteEventStore::from_initialized_connection(conn))
}

pub(crate) fn open_owned_event_store(path: &str) -> Result<SqliteEventStore, CoreError> {
    ensure_event_store_parent_dir(path)?;
    SqliteEventStore::open(path)
}

#[cfg(test)]
mod config_and_bootstrap_tests {
    use super::*;
    use orchestrator_core::test_support::TestDbPath;

    #[test]
    fn open_event_store_replaces_pool_when_runtime_config_changes() {
        let temp_db = TestDbPath::new("app-event-store-pool-config-change");
        let path = temp_db.path().to_str().expect("db path");

        let config_one = DatabaseRuntimeConfig {
            max_connections: 1,
            ..AppConfig::default().database_runtime()
        };
        let mut config_two = config_one.clone();
        config_two.max_connections = 3;
        config_two.chunk_event_flush_ms = config_one.chunk_event_flush_ms.saturating_add(10);

        let _store_one = open_event_store(path, &config_one).expect("open store with config one");
        {
            let pools = event_store_pools()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = pools.get(path).expect("pool entry should exist");
            assert_eq!(entry.config, config_one);
            assert_eq!(entry.pool.max_size(), config_one.max_connections);
        }

        let _store_two = open_event_store(path, &config_two).expect("open store with config two");
        {
            let pools = event_store_pools()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = pools.get(path).expect("pool entry should exist");
            assert_eq!(entry.config, config_two);
            assert_eq!(entry.pool.max_size(), config_two.max_connections);
        }
    }
}
