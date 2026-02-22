use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkerRuntimeError {
    #[error("worker runtime configuration error: {0}")]
    Configuration(String),
    #[error("worker runtime dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("worker runtime session not found: {0}")]
    SessionNotFound(String),
    #[error("worker runtime process error: {0}")]
    Process(String),
    #[error("worker runtime protocol error: {0}")]
    Protocol(String),
    #[error("worker runtime internal error: {0}")]
    Internal(String),
    #[error("worker runtime unimplemented: {0}")]
    Unimplemented(&'static str),
}

pub type WorkerRuntimeResult<T> = Result<T, WorkerRuntimeError>;
