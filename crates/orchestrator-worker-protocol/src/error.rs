use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkerRuntimeError {
    #[error("runtime configuration error: {0}")]
    Configuration(String),
    #[error("runtime dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("runtime session not found: {0}")]
    SessionNotFound(String),
    #[error("runtime process error: {0}")]
    Process(String),
    #[error("runtime protocol error: {0}")]
    Protocol(String),
    #[error("runtime internal error: {0}")]
    Internal(String),
    #[error("runtime unimplemented: {0}")]
    Unimplemented(&'static str),
}

pub type WorkerRuntimeResult<T> = Result<T, WorkerRuntimeError>;
