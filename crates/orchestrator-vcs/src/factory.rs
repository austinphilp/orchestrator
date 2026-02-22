use crate::interface::{VcsProviderError, VcsProviderKind};
use crate::providers::git_cli::GitCliVcsProvider;

const SUPPORTED_PROVIDER_KEYS: [&str; 1] = [VcsProviderKind::GitCli.as_key()];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcsProviderFactoryOutput {
    GitCli(GitCliVcsProvider),
}

pub fn supported_provider_keys() -> &'static [&'static str] {
    &SUPPORTED_PROVIDER_KEYS
}

pub fn resolve_provider_kind(provider_key: &str) -> Result<VcsProviderKind, VcsProviderError> {
    VcsProviderKind::from_key(provider_key)
        .ok_or_else(|| VcsProviderError::UnknownProviderKey(provider_key.to_owned()))
}

pub fn build_provider(provider_key: &str) -> Result<VcsProviderFactoryOutput, VcsProviderError> {
    let kind = resolve_provider_kind(provider_key)?;
    let provider = match kind {
        VcsProviderKind::GitCli => {
            VcsProviderFactoryOutput::GitCli(GitCliVcsProvider::scaffold_default())
        }
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider, resolve_provider_kind, supported_provider_keys, VcsProviderFactoryOutput,
        SUPPORTED_PROVIDER_KEYS,
    };
    use crate::interface::VcsProviderKind;

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
}
