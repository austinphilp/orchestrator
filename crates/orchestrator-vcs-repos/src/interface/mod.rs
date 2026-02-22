use thiserror::Error;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsRepoPullRequestCreateRequest {
    pub title: String,
    pub body: String,
    pub base: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsRepoPullRequestCreateResponse {
    pub number: u64,
    pub url: String,
}

pub trait VcsRepoProvider: Send + Sync {
    fn kind(&self) -> VcsRepoProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsRepoProviderError {
    #[error("unknown vcs repo provider key: {0}")]
    UnknownProviderKey(String),
}
