use crate::interface::{TicketingProviderError, TicketingProviderKind};
use crate::providers::{
    linear::{LinearConfig, LinearTicketingProvider},
    shortcut::ShortcutTicketingProvider,
};
use std::fmt;

const SUPPORTED_PROVIDER_KEYS: [&str; 2] = [
    TicketingProviderKind::Linear.as_key(),
    TicketingProviderKind::Shortcut.as_key(),
];

pub enum TicketingProviderFactoryOutput {
    Linear(LinearTicketingProvider),
    Shortcut(ShortcutTicketingProvider),
}

#[derive(Debug, Clone, Default)]
pub struct TicketingProviderFactoryConfig {
    pub linear: LinearConfig,
}

impl fmt::Display for TicketingProviderFactoryOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let provider_key = match self {
            Self::Linear(_) => TicketingProviderKind::Linear.as_key(),
            Self::Shortcut(_) => TicketingProviderKind::Shortcut.as_key(),
        };
        formatter.write_str(provider_key)
    }
}

impl fmt::Debug for TicketingProviderFactoryOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let provider_key = match self {
            Self::Linear(_) => TicketingProviderKind::Linear.as_key(),
            Self::Shortcut(_) => TicketingProviderKind::Shortcut.as_key(),
        };
        formatter
            .debug_struct("TicketingProviderFactoryOutput")
            .field("provider_key", &provider_key)
            .finish()
    }
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
    let mut config = TicketingProviderFactoryConfig::default();
    if config.linear.api_key.trim().is_empty() {
        config.linear.api_key = "scaffold-token".to_owned();
    }
    build_provider_with_config(provider_key, config)
}

pub fn build_provider_with_config(
    provider_key: &str,
    config: TicketingProviderFactoryConfig,
) -> Result<TicketingProviderFactoryOutput, TicketingProviderError> {
    let kind = resolve_provider_kind(provider_key)?;
    let provider = match kind {
        TicketingProviderKind::Linear => TicketingProviderFactoryOutput::Linear(
            LinearTicketingProvider::new(config.linear).map_err(|error| {
                TicketingProviderError::ProviderInitialization(error.to_string())
            })?,
        ),
        TicketingProviderKind::Shortcut => {
            TicketingProviderFactoryOutput::Shortcut(ShortcutTicketingProvider::scaffold_default())
        }
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
        TicketingProviderFactoryConfig, TicketingProviderFactoryOutput, SUPPORTED_PROVIDER_KEYS,
    };
    use crate::interface::{TicketingProviderError, TicketingProviderKind};
    use crate::providers::linear::LinearConfig;

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

    #[test]
    fn build_provider_with_config_rejects_empty_linear_api_key() {
        let linear_error = build_provider_with_config(
            "ticketing.linear",
            TicketingProviderFactoryConfig {
                linear: LinearConfig::default(),
            },
        )
        .expect_err("linear with empty API key should fail");

        assert!(matches!(
            linear_error,
            TicketingProviderError::ProviderInitialization(message)
                if message.contains("LINEAR_API_KEY is empty")
        ));
    }

    #[test]
    fn build_provider_with_config_applies_linear_configuration() {
        let mut config = TicketingProviderFactoryConfig::default();
        config.linear = LinearConfig {
            api_url: "https://linear.example/graphql".to_owned(),
            api_key: "token".to_owned(),
            ..LinearConfig::default()
        };

        let provider =
            build_provider_with_config("ticketing.linear", config).expect("build linear provider");
        let TicketingProviderFactoryOutput::Linear(provider) = provider else {
            panic!("expected linear provider");
        };
        assert_eq!(provider.sync_query().assigned_to_me, true);
    }
}
