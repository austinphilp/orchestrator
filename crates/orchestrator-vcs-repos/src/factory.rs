use crate::interface::{VcsRepoProviderError, VcsRepoProviderKind};
use crate::providers::github_gh_cli::{GitHubGhCliRepoProvider, GitHubGhCliRepoProviderConfig};
use std::fmt;

const SUPPORTED_PROVIDER_KEYS: [&str; 1] = [VcsRepoProviderKind::GitHubGhCli.as_key()];

pub enum VcsRepoProviderFactoryOutput {
    GitHubGhCli(GitHubGhCliRepoProvider),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VcsRepoProviderFactoryConfig {
    pub github_gh_cli: GitHubGhCliRepoProviderConfig,
}

impl fmt::Debug for VcsRepoProviderFactoryOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let provider_key = match self {
            Self::GitHubGhCli(_) => VcsRepoProviderKind::GitHubGhCli.as_key(),
        };
        formatter
            .debug_struct("VcsRepoProviderFactoryOutput")
            .field("provider_key", &provider_key)
            .finish()
    }
}

pub fn supported_provider_keys() -> &'static [&'static str] {
    &SUPPORTED_PROVIDER_KEYS
}

pub fn resolve_provider_kind(
    provider_key: &str,
) -> Result<VcsRepoProviderKind, VcsRepoProviderError> {
    VcsRepoProviderKind::from_key(provider_key)
        .ok_or_else(|| VcsRepoProviderError::UnknownProviderKey(provider_key.to_owned()))
}

pub fn build_provider(
    provider_key: &str,
) -> Result<VcsRepoProviderFactoryOutput, VcsRepoProviderError> {
    build_provider_with_config(provider_key, VcsRepoProviderFactoryConfig::default())
}

pub fn build_provider_with_config(
    provider_key: &str,
    config: VcsRepoProviderFactoryConfig,
) -> Result<VcsRepoProviderFactoryOutput, VcsRepoProviderError> {
    let kind = resolve_provider_kind(provider_key)?;
    let provider = match kind {
        VcsRepoProviderKind::GitHubGhCli => VcsRepoProviderFactoryOutput::GitHubGhCli(
            GitHubGhCliRepoProvider::from_config(config.github_gh_cli)
                .map_err(|error| VcsRepoProviderError::ProviderInitialization(error.to_string()))?,
        ),
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
        VcsRepoProviderFactoryConfig, VcsRepoProviderFactoryOutput, SUPPORTED_PROVIDER_KEYS,
    };
    use crate::interface::{VcsRepoProviderError, VcsRepoProviderKind};
    use std::path::PathBuf;

    #[test]
    fn supported_provider_keys_are_namespaced() {
        assert_eq!(supported_provider_keys(), &SUPPORTED_PROVIDER_KEYS);
    }

    #[test]
    fn supported_provider_keys_roundtrip_through_kind_resolution() {
        for key in supported_provider_keys() {
            let kind = resolve_provider_kind(key).expect("resolve key");
            assert_eq!(kind.as_key(), *key);
        }
    }

    #[test]
    fn resolve_provider_kind_accepts_known_keys() {
        assert_eq!(
            resolve_provider_kind("vcs_repos.github_gh_cli").expect("resolve github gh key"),
            VcsRepoProviderKind::GitHubGhCli
        );
    }

    #[test]
    fn resolve_provider_kind_rejects_unknown_keys() {
        let error = resolve_provider_kind("github_gh_cli").expect_err("reject legacy key");
        assert_eq!(
            error.to_string(),
            "unknown vcs repo provider key: github_gh_cli"
        );
    }

    #[test]
    fn build_provider_returns_expected_variant_for_each_key() {
        let provider =
            build_provider("vcs_repos.github_gh_cli").expect("build github gh cli provider");
        assert!(matches!(
            provider,
            VcsRepoProviderFactoryOutput::GitHubGhCli(_)
        ));
    }

    #[test]
    fn build_provider_with_config_applies_gh_binary_setting() {
        let provider = build_provider_with_config(
            "vcs_repos.github_gh_cli",
            VcsRepoProviderFactoryConfig {
                github_gh_cli: crate::providers::github_gh_cli::GitHubGhCliRepoProviderConfig {
                    binary: PathBuf::from("gh-real"),
                    ..crate::providers::github_gh_cli::GitHubGhCliRepoProviderConfig::default()
                },
            },
        )
        .expect("build github gh cli provider");

        match provider {
            VcsRepoProviderFactoryOutput::GitHubGhCli(provider) => {
                assert_eq!(provider.binary(), PathBuf::from("gh-real").as_path());
            }
        }
    }

    #[test]
    fn build_provider_with_config_rejects_empty_gh_binary() {
        let error = build_provider_with_config(
            "vcs_repos.github_gh_cli",
            VcsRepoProviderFactoryConfig {
                github_gh_cli: crate::providers::github_gh_cli::GitHubGhCliRepoProviderConfig {
                    binary: PathBuf::new(),
                    ..crate::providers::github_gh_cli::GitHubGhCliRepoProviderConfig::default()
                },
            },
        )
        .expect_err("empty gh binary should fail");

        assert!(matches!(
            error,
            VcsRepoProviderError::ProviderInitialization(message)
                if message.contains("ORCHESTRATOR_GH_BIN is set but empty")
        ));
    }
}
