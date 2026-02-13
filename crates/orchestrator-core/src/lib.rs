use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfig {
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationState {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrchestrationEvent {
    Started,
    QuitRequested,
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
}

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HealthySupervisor;

    #[async_trait]
    impl Supervisor for HealthySupervisor {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn supervisor_trait_contract() {
        let supervisor = HealthySupervisor;
        let result = supervisor.health_check().await;
        assert!(result.is_ok());
    }

    #[test]
    fn core_error_message_is_actionable() {
        let error = CoreError::DependencyUnavailable("gh not found".to_owned());
        assert!(error.to_string().contains("gh not found"));
    }
}
