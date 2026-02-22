use thiserror::Error;

pub use orchestrator_core::{
    CodeHostKind, CoreError, CreatePullRequestRequest, PullRequestCiStatus, PullRequestMergeState,
    PullRequestRef, PullRequestReviewSummary, PullRequestSummary, RepositoryRef, ReviewerRequest,
    UrlOpener,
};

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
        VcsRepoProvider, VcsRepoProviderKind,
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
}
