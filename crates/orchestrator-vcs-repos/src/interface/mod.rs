use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
pub struct TicketId(String);

impl TicketId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for TicketId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TicketId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<TicketId> for orchestrator_domain::TicketId {
    fn from(value: TicketId) -> Self {
        Self::from(value.0)
    }
}

impl From<orchestrator_domain::TicketId> for TicketId {
    fn from(value: orchestrator_domain::TicketId) -> Self {
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
pub enum CodeHostKind {
    Github,
    Gitlab,
    Bitbucket,
    Other(String),
}

impl From<CodeHostKind> for orchestrator_domain::CodeHostKind {
    fn from(value: CodeHostKind) -> Self {
        match value {
            CodeHostKind::Github => Self::Github,
            CodeHostKind::Gitlab => Self::Gitlab,
            CodeHostKind::Bitbucket => Self::Bitbucket,
            CodeHostKind::Other(name) => Self::Other(name),
        }
    }
}

impl From<orchestrator_domain::CodeHostKind> for CodeHostKind {
    fn from(value: orchestrator_domain::CodeHostKind) -> Self {
        match value {
            orchestrator_domain::CodeHostKind::Github => Self::Github,
            orchestrator_domain::CodeHostKind::Gitlab => Self::Gitlab,
            orchestrator_domain::CodeHostKind::Bitbucket => Self::Bitbucket,
            orchestrator_domain::CodeHostKind::Other(name) => Self::Other(name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePullRequestRequest {
    pub repository: RepositoryRef,
    pub title: String,
    pub body: String,
    pub base_branch: String,
    pub head_branch: String,
    pub ticket: Option<TicketId>,
}

impl From<CreatePullRequestRequest> for orchestrator_domain::CreatePullRequestRequest {
    fn from(value: CreatePullRequestRequest) -> Self {
        Self {
            repository: value.repository.into(),
            title: value.title,
            body: value.body,
            base_branch: value.base_branch,
            head_branch: value.head_branch,
            ticket: value.ticket.map(Into::into),
        }
    }
}

impl From<orchestrator_domain::CreatePullRequestRequest> for CreatePullRequestRequest {
    fn from(value: orchestrator_domain::CreatePullRequestRequest) -> Self {
        Self {
            repository: value.repository.into(),
            title: value.title,
            body: value.body,
            base_branch: value.base_branch,
            head_branch: value.head_branch,
            ticket: value.ticket.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repository: RepositoryRef,
    pub number: u64,
    pub url: String,
}

impl From<PullRequestRef> for orchestrator_domain::PullRequestRef {
    fn from(value: PullRequestRef) -> Self {
        Self {
            repository: value.repository.into(),
            number: value.number,
            url: value.url,
        }
    }
}

impl From<orchestrator_domain::PullRequestRef> for PullRequestRef {
    fn from(value: orchestrator_domain::PullRequestRef) -> Self {
        Self {
            repository: value.repository.into(),
            number: value.number,
            url: value.url,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    pub reference: PullRequestRef,
    pub title: String,
    pub is_draft: bool,
}

impl From<PullRequestSummary> for orchestrator_domain::PullRequestSummary {
    fn from(value: PullRequestSummary) -> Self {
        Self {
            reference: value.reference.into(),
            title: value.title,
            is_draft: value.is_draft,
        }
    }
}

impl From<orchestrator_domain::PullRequestSummary> for PullRequestSummary {
    fn from(value: orchestrator_domain::PullRequestSummary) -> Self {
        Self {
            reference: value.reference.into(),
            title: value.title,
            is_draft: value.is_draft,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PullRequestReviewSummary {
    pub total: u32,
    pub approved: u32,
    pub changes_requested: u32,
    pub commented: u32,
    pub pending: u32,
    pub dismissed: u32,
}

impl From<PullRequestReviewSummary> for orchestrator_domain::PullRequestReviewSummary {
    fn from(value: PullRequestReviewSummary) -> Self {
        Self {
            total: value.total,
            approved: value.approved,
            changes_requested: value.changes_requested,
            commented: value.commented,
            pending: value.pending,
            dismissed: value.dismissed,
        }
    }
}

impl From<orchestrator_domain::PullRequestReviewSummary> for PullRequestReviewSummary {
    fn from(value: orchestrator_domain::PullRequestReviewSummary) -> Self {
        Self {
            total: value.total,
            approved: value.approved,
            changes_requested: value.changes_requested,
            commented: value.commented,
            pending: value.pending,
            dismissed: value.dismissed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestMergeState {
    pub merged: bool,
    pub is_draft: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_summary: Option<PullRequestReviewSummary>,
    #[serde(default)]
    pub merge_conflict: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
}

impl From<PullRequestMergeState> for orchestrator_domain::PullRequestMergeState {
    fn from(value: PullRequestMergeState) -> Self {
        Self {
            merged: value.merged,
            is_draft: value.is_draft,
            state: value.state,
            review_decision: value.review_decision,
            review_summary: value.review_summary.map(Into::into),
            merge_conflict: value.merge_conflict,
            base_branch: value.base_branch,
            head_branch: value.head_branch,
        }
    }
}

impl From<orchestrator_domain::PullRequestMergeState> for PullRequestMergeState {
    fn from(value: orchestrator_domain::PullRequestMergeState) -> Self {
        Self {
            merged: value.merged,
            is_draft: value.is_draft,
            state: value.state,
            review_decision: value.review_decision,
            review_summary: value.review_summary.map(Into::into),
            merge_conflict: value.merge_conflict,
            base_branch: value.base_branch,
            head_branch: value.head_branch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestCiStatus {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub bucket: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl From<PullRequestCiStatus> for orchestrator_domain::PullRequestCiStatus {
    fn from(value: PullRequestCiStatus) -> Self {
        Self {
            name: value.name,
            workflow: value.workflow,
            bucket: value.bucket,
            state: value.state,
            link: value.link,
        }
    }
}

impl From<orchestrator_domain::PullRequestCiStatus> for PullRequestCiStatus {
    fn from(value: orchestrator_domain::PullRequestCiStatus) -> Self {
        Self {
            name: value.name,
            workflow: value.workflow,
            bucket: value.bucket,
            state: value.state,
            link: value.link,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReviewerRequest {
    pub users: Vec<String>,
    pub teams: Vec<String>,
}

impl From<ReviewerRequest> for orchestrator_domain::ReviewerRequest {
    fn from(value: ReviewerRequest) -> Self {
        Self {
            users: value.users,
            teams: value.teams,
        }
    }
}

impl From<orchestrator_domain::ReviewerRequest> for ReviewerRequest {
    fn from(value: orchestrator_domain::ReviewerRequest) -> Self {
        Self {
            users: value.users,
            teams: value.teams,
        }
    }
}

#[async_trait::async_trait]
pub trait UrlOpener: Send + Sync {
    async fn open_url(&self, url: &str) -> Result<(), CoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsRepoProviderKind {
    GitHubGhCli,
}

impl VcsRepoProviderKind {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::GitHubGhCli => "vcs_repos.github_gh_cli",
        }
    }

    pub fn from_key(provider_key: &str) -> Option<Self> {
        match provider_key {
            "vcs_repos.github_gh_cli" => Some(Self::GitHubGhCli),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait::async_trait]
pub trait CodeHostProvider: Send + Sync {
    fn kind(&self) -> CodeHostKind;
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn create_draft_pull_request(
        &self,
        request: CreatePullRequestRequest,
    ) -> Result<PullRequestSummary, CoreError>;
    async fn mark_ready_for_review(&self, pr: &PullRequestRef) -> Result<(), CoreError>;
    async fn request_reviewers(
        &self,
        pr: &PullRequestRef,
        reviewers: ReviewerRequest,
    ) -> Result<(), CoreError>;
    async fn list_waiting_for_my_review(&self) -> Result<Vec<PullRequestSummary>, CoreError>;
    async fn get_pull_request_merge_state(
        &self,
        pr: &PullRequestRef,
    ) -> Result<PullRequestMergeState, CoreError>;
    async fn list_pull_request_ci_statuses(
        &self,
        _pr: &PullRequestRef,
    ) -> Result<Vec<PullRequestCiStatus>, CoreError> {
        Ok(Vec::new())
    }
    async fn merge_pull_request(&self, pr: &PullRequestRef) -> Result<(), CoreError>;
    async fn find_open_pull_request_for_branch(
        &self,
        _repository: &RepositoryRef,
        _head_branch: &str,
    ) -> Result<Option<PullRequestRef>, CoreError> {
        Ok(None)
    }
}

pub trait VcsRepoProvider: CodeHostProvider + Send + Sync {
    fn kind(&self) -> VcsRepoProviderKind;

    fn provider_key(&self) -> &'static str {
        VcsRepoProvider::kind(self).as_key()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsRepoProviderError {
    #[error("unknown vcs repo provider key: {0}")]
    UnknownProviderKey(String),
    #[error("failed to initialize vcs repo provider: {0}")]
    ProviderInitialization(String),
}

#[cfg(test)]
mod tests {
    use super::{
        CodeHostKind, CodeHostProvider, CoreError, CreatePullRequestRequest, PullRequestCiStatus,
        PullRequestMergeState, PullRequestRef, PullRequestSummary, RepositoryRef, ReviewerRequest,
        TicketId, VcsRepoProvider, VcsRepoProviderKind,
    };

    struct StubCodeHostProvider;

    #[async_trait::async_trait]
    impl CodeHostProvider for StubCodeHostProvider {
        fn kind(&self) -> CodeHostKind {
            CodeHostKind::Other("stub".to_owned())
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn create_draft_pull_request(
            &self,
            _request: CreatePullRequestRequest,
        ) -> Result<PullRequestSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in stub".to_owned(),
            ))
        }

        async fn mark_ready_for_review(&self, _pr: &PullRequestRef) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in stub".to_owned(),
            ))
        }

        async fn request_reviewers(
            &self,
            _pr: &PullRequestRef,
            _reviewers: ReviewerRequest,
        ) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in stub".to_owned(),
            ))
        }

        async fn list_waiting_for_my_review(&self) -> Result<Vec<PullRequestSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn get_pull_request_merge_state(
            &self,
            _pr: &PullRequestRef,
        ) -> Result<PullRequestMergeState, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in stub".to_owned(),
            ))
        }

        async fn list_pull_request_ci_statuses(
            &self,
            _pr: &PullRequestRef,
        ) -> Result<Vec<PullRequestCiStatus>, CoreError> {
            Ok(Vec::new())
        }

        async fn merge_pull_request(&self, _pr: &PullRequestRef) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in stub".to_owned(),
            ))
        }

        async fn find_open_pull_request_for_branch(
            &self,
            _repository: &RepositoryRef,
            _head_branch: &str,
        ) -> Result<Option<PullRequestRef>, CoreError> {
            Ok(None)
        }
    }

    impl VcsRepoProvider for StubCodeHostProvider {
        fn kind(&self) -> VcsRepoProviderKind {
            VcsRepoProviderKind::GitHubGhCli
        }
    }

    #[test]
    fn vcs_repo_provider_does_not_require_github_client_trait() {
        let provider = StubCodeHostProvider;
        assert_eq!(
            VcsRepoProvider::provider_key(&provider),
            VcsRepoProviderKind::GitHubGhCli.as_key()
        );

        fn assert_code_host_provider<T: VcsRepoProvider + CodeHostProvider>(_provider: &T) {}
        assert_code_host_provider(&provider);
    }

    #[test]
    fn core_error_roundtrip_preserves_known_variants() {
        let dependency = CoreError::DependencyUnavailable("gh unavailable".to_owned());
        let domain_dependency: orchestrator_domain::CoreError = dependency.clone().into();
        assert_eq!(CoreError::from(domain_dependency), dependency);

        let configuration = CoreError::Configuration("invalid token".to_owned());
        let domain_configuration: orchestrator_domain::CoreError = configuration.clone().into();
        assert_eq!(CoreError::from(domain_configuration), configuration);
    }

    #[test]
    fn core_error_from_domain_maps_non_contract_variants_to_dependency_unavailable() {
        let converted = CoreError::from(orchestrator_domain::CoreError::Persistence(
            "network timeout".to_owned(),
        ));
        assert!(matches!(converted, CoreError::DependencyUnavailable(_)));
    }

    #[test]
    fn ticket_id_roundtrip_with_domain_preserves_value() {
        let domain = orchestrator_domain::TicketId::from("linear:issue-347");
        let interface: TicketId = domain.clone().into();
        let roundtrip: orchestrator_domain::TicketId = interface.into();
        assert_eq!(roundtrip, domain);
    }
}
