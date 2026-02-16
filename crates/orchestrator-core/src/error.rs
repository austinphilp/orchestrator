use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("persistence error: {0}")]
    Persistence(String),
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
