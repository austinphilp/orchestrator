mod config;
mod transport;

use crate::interface::{VcsProvider, VcsProviderKind};
pub use config::GitCliVcsProviderConfig;
pub use transport::GitCliVcsTransport;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitCliVcsProvider {
    config: GitCliVcsProviderConfig,
    transport: GitCliVcsTransport,
}

impl GitCliVcsProvider {
    pub fn new(config: GitCliVcsProviderConfig, transport: GitCliVcsTransport) -> Self {
        Self { config, transport }
    }

    pub fn scaffold_default() -> Self {
        Self::default()
    }

    pub fn config(&self) -> &GitCliVcsProviderConfig {
        &self.config
    }

    pub fn transport(&self) -> &GitCliVcsTransport {
        &self.transport
    }
}

impl VcsProvider for GitCliVcsProvider {
    fn kind(&self) -> VcsProviderKind {
        VcsProviderKind::GitCli
    }
}

#[cfg(test)]
mod tests {
    use super::GitCliVcsProvider;
    use crate::interface::{VcsProvider, VcsProviderKind};

    #[test]
    fn scaffold_default_uses_git_cli_kind() {
        let provider = GitCliVcsProvider::scaffold_default();
        assert_eq!(provider.kind(), VcsProviderKind::GitCli);
        assert_eq!(provider.provider_key(), "vcs.git_cli");
        assert_eq!(provider.config().binary_name, "git");
        assert_eq!(provider.transport().protocol, "cli");
    }
}
