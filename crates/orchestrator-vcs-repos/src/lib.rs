pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
    VcsRepoProviderFactoryConfig, VcsRepoProviderFactoryOutput,
};
pub use interface::{
    CodeHostKind, CodeHostProvider, CoreCodeHostProvider, CoreError, CoreGithubClient,
    CreatePullRequestRequest, GithubClient, PullRequestCiStatus, PullRequestMergeState,
    PullRequestRef, PullRequestReviewSummary, PullRequestSummary, RepositoryRef, ReviewerRequest,
    UrlOpener, VcsRepoProvider, VcsRepoProviderError, VcsRepoProviderKind,
};
pub use providers::github_gh_cli::{
    default_system_url_opener, CommandRunner, GitHubGhCliRepoProvider,
    GitHubGhCliRepoProviderConfig, ProcessCommandRunner, SystemUrlOpener,
};
