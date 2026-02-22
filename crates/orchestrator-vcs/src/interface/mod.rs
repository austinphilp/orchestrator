use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_BASE_BRANCH: &str = "main";

fn default_base_branch() -> String {
    DEFAULT_BASE_BRANCH.to_owned()
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
}

impl From<CoreError> for orchestrator_domain::CoreError {
    fn from(value: CoreError) -> Self {
        match value {
            CoreError::DependencyUnavailable(message) => Self::DependencyUnavailable(message),
            CoreError::Configuration(message) => Self::Configuration(message),
        }
    }
}

impl From<orchestrator_domain::CoreError> for CoreError {
    fn from(value: orchestrator_domain::CoreError) -> Self {
        match value {
            orchestrator_domain::CoreError::DependencyUnavailable(message) => {
                Self::DependencyUnavailable(message)
            }
            orchestrator_domain::CoreError::Configuration(message) => Self::Configuration(message),
            other => Self::DependencyUnavailable(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorktreeId(String);

impl WorktreeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for WorktreeId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for WorktreeId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<WorktreeId> for orchestrator_domain::WorktreeId {
    fn from(value: WorktreeId) -> Self {
        Self::from(value.0)
    }
}

impl From<orchestrator_domain::WorktreeId> for WorktreeId {
    fn from(value: orchestrator_domain::WorktreeId) -> Self {
        Self::from(value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
}

impl From<RepositoryRef> for orchestrator_domain::RepositoryRef {
    fn from(value: RepositoryRef) -> Self {
        Self {
            id: value.id,
            name: value.name,
            root: value.root,
        }
    }
}

impl From<orchestrator_domain::RepositoryRef> for RepositoryRef {
    fn from(value: orchestrator_domain::RepositoryRef) -> Self {
        Self {
            id: value.id,
            name: value.name,
            root: value.root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorktreeRequest {
    pub worktree_id: WorktreeId,
    pub repository: RepositoryRef,
    pub worktree_path: PathBuf,
    pub branch: String,
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
    pub ticket_identifier: Option<String>,
}

impl From<CreateWorktreeRequest> for orchestrator_domain::CreateWorktreeRequest {
    fn from(value: CreateWorktreeRequest) -> Self {
        Self {
            worktree_id: value.worktree_id.into(),
            repository: value.repository.into(),
            worktree_path: value.worktree_path,
            branch: value.branch,
            base_branch: value.base_branch,
            ticket_identifier: value.ticket_identifier,
        }
    }
}

impl From<orchestrator_domain::CreateWorktreeRequest> for CreateWorktreeRequest {
    fn from(value: orchestrator_domain::CreateWorktreeRequest) -> Self {
        Self {
            worktree_id: value.worktree_id.into(),
            repository: value.repository.into(),
            worktree_path: value.worktree_path,
            branch: value.branch,
            base_branch: value.base_branch,
            ticket_identifier: value.ticket_identifier,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSummary {
    pub worktree_id: WorktreeId,
    pub repository: RepositoryRef,
    pub path: PathBuf,
    pub branch: String,
    pub base_branch: String,
}

impl From<WorktreeSummary> for orchestrator_domain::WorktreeSummary {
    fn from(value: WorktreeSummary) -> Self {
        Self {
            worktree_id: value.worktree_id.into(),
            repository: value.repository.into(),
            path: value.path,
            branch: value.branch,
            base_branch: value.base_branch,
        }
    }
}

impl From<orchestrator_domain::WorktreeSummary> for WorktreeSummary {
    fn from(value: orchestrator_domain::WorktreeSummary) -> Self {
        Self {
            worktree_id: value.worktree_id.into(),
            repository: value.repository.into(),
            path: value.path,
            branch: value.branch,
            base_branch: value.base_branch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteWorktreeRequest {
    pub worktree: WorktreeSummary,
    #[serde(default)]
    pub delete_branch: bool,
    #[serde(default)]
    pub delete_directory: bool,
}

impl DeleteWorktreeRequest {
    pub fn non_destructive(worktree: WorktreeSummary) -> Self {
        Self {
            worktree,
            delete_branch: false,
            delete_directory: false,
        }
    }
}

impl From<DeleteWorktreeRequest> for orchestrator_domain::DeleteWorktreeRequest {
    fn from(value: DeleteWorktreeRequest) -> Self {
        Self {
            worktree: value.worktree.into(),
            delete_branch: value.delete_branch,
            delete_directory: value.delete_directory,
        }
    }
}

impl From<orchestrator_domain::DeleteWorktreeRequest> for DeleteWorktreeRequest {
    fn from(value: orchestrator_domain::DeleteWorktreeRequest) -> Self {
        Self {
            worktree: value.worktree.into(),
            delete_branch: value.delete_branch,
            delete_directory: value.delete_directory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeStatus {
    pub is_dirty: bool,
    pub commits_ahead: u32,
    pub commits_behind: u32,
}

impl From<WorktreeStatus> for orchestrator_domain::WorktreeStatus {
    fn from(value: WorktreeStatus) -> Self {
        Self {
            is_dirty: value.is_dirty,
            commits_ahead: value.commits_ahead,
            commits_behind: value.commits_behind,
        }
    }
}

impl From<orchestrator_domain::WorktreeStatus> for WorktreeStatus {
    fn from(value: orchestrator_domain::WorktreeStatus) -> Self {
        Self {
            is_dirty: value.is_dirty,
            commits_ahead: value.commits_ahead,
            commits_behind: value.commits_behind,
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::{CoreError, VcsProviderKind, WorktreeId};

    #[test]
    fn core_error_roundtrip_preserves_known_variants() {
        let dependency = CoreError::DependencyUnavailable("git missing".to_owned());
        let domain_dependency: orchestrator_domain::CoreError = dependency.clone().into();
        assert_eq!(CoreError::from(domain_dependency), dependency);

        let configuration = CoreError::Configuration("bad config".to_owned());
        let domain_configuration: orchestrator_domain::CoreError = configuration.clone().into();
        assert_eq!(CoreError::from(domain_configuration), configuration);
    }

    #[test]
    fn core_error_from_domain_maps_non_contract_variants_to_dependency_unavailable() {
        let converted = CoreError::from(orchestrator_domain::CoreError::InvalidCommandArgs {
            command_id: "vcs.create_worktree".to_owned(),
            reason: "invalid branch".to_owned(),
        });
        assert!(matches!(converted, CoreError::DependencyUnavailable(_)));
    }

    #[test]
    fn worktree_id_roundtrip_with_domain_preserves_value() {
        let domain = orchestrator_domain::WorktreeId::new("wt-347");
        let interface: WorktreeId = domain.clone().into();
        let roundtrip: orchestrator_domain::WorktreeId = interface.into();
        assert_eq!(roundtrip, domain);
    }

    #[test]
    fn provider_key_mapping_is_stable() {
        assert_eq!(
            VcsProviderKind::from_key("vcs.git_cli"),
            Some(VcsProviderKind::GitCli)
        );
        assert_eq!(VcsProviderKind::GitCli.as_key(), "vcs.git_cli");
        assert_eq!(VcsProviderKind::from_key("vcs.unknown"), None);
    }
}
