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
}
