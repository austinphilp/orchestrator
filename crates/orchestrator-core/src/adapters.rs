use async_trait::async_trait;

use crate::CoreError;

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}
