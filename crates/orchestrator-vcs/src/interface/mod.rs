use std::path::{Path, PathBuf};

use thiserror::Error;

pub use orchestrator_domain::{
    CoreError, CreateWorktreeRequest, DeleteWorktreeRequest, RepositoryRef, WorktreeId,
    WorktreeStatus, WorktreeSummary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsProviderKind {
    GitCli,
}

impl VcsProviderKind {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::GitCli => "vcs.git_cli",
        }
    }

    pub fn from_key(provider_key: &str) -> Option<Self> {
        match provider_key {
            "vcs.git_cli" => Some(Self::GitCli),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
pub trait VcsProvider: Send + Sync {
    fn kind(&self) -> VcsProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }

    async fn health_check(&self) -> Result<(), CoreError>;
    async fn discover_repositories(
        &self,
        roots: &[PathBuf],
    ) -> Result<Vec<RepositoryRef>, CoreError>;
    async fn create_worktree(
        &self,
        request: CreateWorktreeRequest,
    ) -> Result<WorktreeSummary, CoreError>;
    async fn delete_worktree(&self, request: DeleteWorktreeRequest) -> Result<(), CoreError>;
    async fn worktree_status(&self, worktree_path: &Path) -> Result<WorktreeStatus, CoreError>;
}

pub trait WorktreeManager: VcsProvider {}

impl<T: VcsProvider + ?Sized> WorktreeManager for T {}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsProviderError {
    #[error("unknown vcs provider key: {0}")]
    UnknownProviderKey(String),
    #[error("failed to initialize vcs provider: {0}")]
    ProviderInitialization(String),
}
