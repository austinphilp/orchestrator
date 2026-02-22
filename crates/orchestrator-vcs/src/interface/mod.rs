use thiserror::Error;

pub use orchestrator_core::VcsProvider as CoreVcsProvider;
pub use orchestrator_core::{
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

pub trait VcsProvider: CoreVcsProvider + Send + Sync {
    fn kind(&self) -> VcsProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
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
