use crate::interface::{TicketingProviderError, TicketingProviderKind};
use crate::providers::{linear::LinearTicketingProvider, shortcut::ShortcutTicketingProvider};

const SUPPORTED_PROVIDER_KEYS: [&str; 2] = [
    TicketingProviderKind::Linear.as_key(),
    TicketingProviderKind::Shortcut.as_key(),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketingProviderFactoryOutput {
    Linear(LinearTicketingProvider),
    Shortcut(ShortcutTicketingProvider),
}

pub fn supported_provider_keys() -> &'static [&'static str] {
    &SUPPORTED_PROVIDER_KEYS
}

pub fn resolve_provider_kind(
    provider_key: &str,
) -> Result<TicketingProviderKind, TicketingProviderError> {
    TicketingProviderKind::from_key(provider_key)
        .ok_or_else(|| TicketingProviderError::UnknownProviderKey(provider_key.to_owned()))
}

pub fn build_provider(
    provider_key: &str,
) -> Result<TicketingProviderFactoryOutput, TicketingProviderError> {
    let kind = resolve_provider_kind(provider_key)?;
    let provider = match kind {
        TicketingProviderKind::Linear => {
            TicketingProviderFactoryOutput::Linear(LinearTicketingProvider::scaffold_default())
        }
        TicketingProviderKind::Shortcut => {
            TicketingProviderFactoryOutput::Shortcut(ShortcutTicketingProvider::scaffold_default())
        }
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider, resolve_provider_kind, supported_provider_keys,
        TicketingProviderFactoryOutput, SUPPORTED_PROVIDER_KEYS,
    };
    use crate::interface::TicketingProviderKind;

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
            resolve_provider_kind("ticketing.linear").expect("resolve linear key"),
            TicketingProviderKind::Linear
        );
        assert_eq!(
            resolve_provider_kind("ticketing.shortcut").expect("resolve shortcut key"),
            TicketingProviderKind::Shortcut
        );
    }

    #[test]
    fn resolve_provider_kind_rejects_unknown_keys() {
        let error = resolve_provider_kind("linear").expect_err("reject legacy key");
        assert_eq!(error.to_string(), "unknown ticketing provider key: linear");
    }

    #[test]
    fn build_provider_returns_expected_variant_for_each_key() {
        let linear = build_provider("ticketing.linear").expect("build linear provider");
        let shortcut = build_provider("ticketing.shortcut").expect("build shortcut provider");

        assert!(matches!(linear, TicketingProviderFactoryOutput::Linear(_)));
        assert!(matches!(
            shortcut,
            TicketingProviderFactoryOutput::Shortcut(_)
        ));
    }
}
