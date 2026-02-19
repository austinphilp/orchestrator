use orchestrator_runtime::RuntimeError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("no repository mapping exists for project '{project}' in provider '{provider}'")]
    MissingProjectRepositoryMapping { provider: String, project: String },
    #[error(
        "mapped repository '{repository_path}' for project '{project}' (provider '{provider}') does not resolve to a single repository: {reason}"
    )]
    InvalidMappedRepository {
        provider: String,
        project: String,
        repository_path: String,
        reason: String,
    },
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(
        "unsupported database schema version {found}; this binary supports up to {supported}. Please upgrade orchestrator-core."
    )]
    UnsupportedSchemaVersion { supported: u32, found: u32 },
    #[error("unknown command id: {command_id}")]
    UnknownCommand { command_id: String },
    #[error("invalid args for command {command_id}: {reason}")]
    InvalidCommandArgs { command_id: String, reason: String },
    #[error("command {command_id} expected schema {expected}; details: {details}")]
    CommandSchemaMismatch {
        command_id: String,
        expected: String,
        details: String,
    },
    #[error("duplicate command id in registry: {command_id}")]
    DuplicateCommandId { command_id: String },
}
