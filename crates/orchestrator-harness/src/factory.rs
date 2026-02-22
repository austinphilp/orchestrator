use crate::interface::{HarnessProviderError, HarnessProviderKind};
use crate::providers::{codex::CodexHarnessProvider, opencode::OpenCodeHarnessProvider};
use std::fmt;

const SUPPORTED_PROVIDER_KEYS: [&str; 2] = [
    HarnessProviderKind::OpenCode.as_key(),
    HarnessProviderKind::Codex.as_key(),
];

#[derive(Clone)]
pub enum HarnessProviderFactoryOutput {
    OpenCode(OpenCodeHarnessProvider),
    Codex(CodexHarnessProvider),
}

impl fmt::Debug for HarnessProviderFactoryOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::OpenCode(_) => HarnessProviderKind::OpenCode.as_key(),
            Self::Codex(_) => HarnessProviderKind::Codex.as_key(),
        };
        formatter
            .debug_struct("HarnessProviderFactoryOutput")
            .field("provider_key", &kind)
            .finish()
    }
}

pub fn supported_provider_keys() -> &'static [&'static str] {
    &SUPPORTED_PROVIDER_KEYS
}

pub fn resolve_provider_kind(
    provider_key: &str,
) -> Result<HarnessProviderKind, HarnessProviderError> {
    HarnessProviderKind::from_key(provider_key)
        .ok_or_else(|| HarnessProviderError::UnknownProviderKey(provider_key.to_owned()))
}

pub fn build_provider(
    provider_key: &str,
) -> Result<HarnessProviderFactoryOutput, HarnessProviderError> {
    let kind = resolve_provider_kind(provider_key)?;
    let provider = match kind {
        HarnessProviderKind::OpenCode => {
            HarnessProviderFactoryOutput::OpenCode(OpenCodeHarnessProvider::scaffold_default())
        }
        HarnessProviderKind::Codex => {
            HarnessProviderFactoryOutput::Codex(CodexHarnessProvider::scaffold_default())
        }
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider, resolve_provider_kind, supported_provider_keys,
        HarnessProviderFactoryOutput, SUPPORTED_PROVIDER_KEYS,
    };
    use crate::interface::{
        HarnessBackendInfo, HarnessProvider, HarnessProviderKind, HarnessRuntimeProvider,
        HarnessSessionControl, HarnessSessionStreamSource,
    };

    fn assert_runtime_provider_contract<T>(provider: &T)
    where
        T: HarnessRuntimeProvider
            + HarnessSessionControl
            + HarnessSessionStreamSource
            + HarnessBackendInfo,
    {
        let _ = provider;
    }

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
            resolve_provider_kind("harness.opencode").expect("resolve opencode key"),
            HarnessProviderKind::OpenCode
        );
        assert_eq!(
            resolve_provider_kind("harness.codex").expect("resolve codex key"),
            HarnessProviderKind::Codex
        );
    }

    #[test]
    fn resolve_provider_kind_rejects_unknown_keys() {
        let error = resolve_provider_kind("opencode").expect_err("reject legacy key");
        assert_eq!(error.to_string(), "unknown harness provider key: opencode");
    }

    #[test]
    fn build_provider_returns_expected_variant_for_each_key() {
        let opencode = build_provider("harness.opencode").expect("build opencode provider");
        let codex = build_provider("harness.codex").expect("build codex provider");

        match opencode {
            HarnessProviderFactoryOutput::OpenCode(provider) => {
                assert_runtime_provider_contract(&provider);
                assert_eq!(HarnessProvider::kind(&provider), HarnessProviderKind::OpenCode);
            }
            HarnessProviderFactoryOutput::Codex(_) => panic!("expected opencode provider"),
        }
        match codex {
            HarnessProviderFactoryOutput::Codex(provider) => {
                assert_eq!(provider.kind(), HarnessProviderKind::Codex);
            }
            HarnessProviderFactoryOutput::OpenCode(_) => panic!("expected codex provider"),
        }
    }
}
