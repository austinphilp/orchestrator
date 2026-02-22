use crate::interface::{VcsProviderError, VcsProviderKind};
use crate::providers::git_cli::{GitCliVcsProvider, GitCliVcsProviderConfig};
use std::fmt;

const SUPPORTED_PROVIDER_KEYS: [&str; 1] = [VcsProviderKind::GitCli.as_key()];

pub enum VcsProviderFactoryOutput {
    GitCli(GitCliVcsProvider),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VcsProviderFactoryConfig {
    pub git_cli: GitCliVcsProviderConfig,
}

impl fmt::Debug for VcsProviderFactoryOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let provider_key = match self {
            Self::GitCli(_) => VcsProviderKind::GitCli.as_key(),
        };
        formatter
            .debug_struct("VcsProviderFactoryOutput")
            .field("provider_key", &provider_key)
            .finish()
    }
}

pub fn supported_provider_keys() -> &'static [&'static str] {
    &SUPPORTED_PROVIDER_KEYS
}

pub fn resolve_provider_kind(provider_key: &str) -> Result<VcsProviderKind, VcsProviderError> {
    VcsProviderKind::from_key(provider_key)
        .ok_or_else(|| VcsProviderError::UnknownProviderKey(provider_key.to_owned()))
}

pub fn build_provider(provider_key: &str) -> Result<VcsProviderFactoryOutput, VcsProviderError> {
    build_provider_with_config(provider_key, VcsProviderFactoryConfig::default())
}

pub fn build_provider_with_config(
    provider_key: &str,
    config: VcsProviderFactoryConfig,
) -> Result<VcsProviderFactoryOutput, VcsProviderError> {
    let kind = resolve_provider_kind(provider_key)?;
    let provider = match kind {
        VcsProviderKind::GitCli => VcsProviderFactoryOutput::GitCli(
            GitCliVcsProvider::from_config(config.git_cli)
                .map_err(|error| VcsProviderError::ProviderInitialization(error.to_string()))?,
        ),
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
        VcsProviderFactoryConfig, VcsProviderFactoryOutput, SUPPORTED_PROVIDER_KEYS,
    };
    use crate::interface::{VcsProviderError, VcsProviderKind};
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
            resolve_provider_kind("vcs.git_cli").expect("resolve git cli key"),
            VcsProviderKind::GitCli
        );
    }

    #[test]
    fn resolve_provider_kind_rejects_unknown_keys() {
        let error = resolve_provider_kind("git").expect_err("reject legacy key");
        assert_eq!(error.to_string(), "unknown vcs provider key: git");
    }

    #[test]
    fn build_provider_returns_expected_variant_for_each_key() {
        let provider = build_provider("vcs.git_cli").expect("build git cli provider");
        assert!(matches!(provider, VcsProviderFactoryOutput::GitCli(_)));
    }

    #[test]
    fn build_provider_with_config_applies_git_binary_setting() {
        let provider = build_provider_with_config(
            "vcs.git_cli",
            VcsProviderFactoryConfig {
                git_cli: crate::providers::git_cli::GitCliVcsProviderConfig {
                    binary: PathBuf::from("git-real"),
                    ..crate::providers::git_cli::GitCliVcsProviderConfig::default()
                },
            },
        )
        .expect("build git cli provider");
        match provider {
            VcsProviderFactoryOutput::GitCli(provider) => {
                assert_eq!(provider.binary(), PathBuf::from("git-real").as_path());
            }
        }
    }

    #[test]
    fn build_provider_with_config_rejects_empty_git_binary() {
        let error = build_provider_with_config(
            "vcs.git_cli",
            VcsProviderFactoryConfig {
                git_cli: crate::providers::git_cli::GitCliVcsProviderConfig {
                    binary: PathBuf::new(),
                    ..crate::providers::git_cli::GitCliVcsProviderConfig::default()
                },
            },
        )
        .expect_err("empty git binary should fail");

        assert!(matches!(
            error,
            VcsProviderError::ProviderInitialization(message)
                if message.contains("ORCHESTRATOR_GIT_BIN is set but empty")
        ));
    }
}
