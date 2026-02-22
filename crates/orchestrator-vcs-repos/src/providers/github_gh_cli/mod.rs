mod config;
mod transport;

use crate::interface::{VcsRepoProvider, VcsRepoProviderKind};
pub use config::GitHubGhCliRepoProviderConfig;
pub use transport::GitHubGhCliRepoTransport;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitHubGhCliRepoProvider {
    config: GitHubGhCliRepoProviderConfig,
    transport: GitHubGhCliRepoTransport,
}

impl GitHubGhCliRepoProvider {
    pub fn new(config: GitHubGhCliRepoProviderConfig, transport: GitHubGhCliRepoTransport) -> Self {
        Self { config, transport }
    }

    pub fn scaffold_default() -> Self {
        Self::default()
    }

    pub fn config(&self) -> &GitHubGhCliRepoProviderConfig {
        &self.config
    }

    pub fn transport(&self) -> &GitHubGhCliRepoTransport {
        &self.transport
    }
}

impl VcsRepoProvider for GitHubGhCliRepoProvider {
    fn kind(&self) -> VcsRepoProviderKind {
        VcsRepoProviderKind::GitHubGhCli
    }
}

#[cfg(test)]
mod tests {
    use super::GitHubGhCliRepoProvider;
    use crate::interface::{VcsRepoProvider, VcsRepoProviderKind};

    #[test]
    fn scaffold_default_uses_github_gh_cli_kind() {
        let provider = GitHubGhCliRepoProvider::scaffold_default();
        assert_eq!(provider.kind(), VcsRepoProviderKind::GitHubGhCli);
        assert_eq!(provider.provider_key(), "vcs_repos.github_gh_cli");
        assert_eq!(provider.config().binary_name, "gh");
        assert_eq!(provider.transport().protocol, "cli");
    }
}
