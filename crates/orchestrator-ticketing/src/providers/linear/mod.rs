include!("config_and_graphql.rs");
include!("provider_core.rs");
include!("provider_trait_and_models.rs");

impl LinearTicketingProvider {
    pub fn scaffold_default() -> Self {
        let mut config = LinearConfig::default();
        if config.api_key.trim().is_empty() {
            config.api_key = "scaffold-token".to_owned();
        }
        Self::new(config).expect("construct default linear provider")
    }
}

#[cfg(test)]
mod provider_key_tests {
    use super::LinearTicketingProvider;
    use crate::interface::{TicketingProvider, TicketingProviderKind};

    #[test]
    fn scaffold_default_uses_linear_kind() {
        let provider = LinearTicketingProvider::scaffold_default();
        assert_eq!(provider.kind(), TicketingProviderKind::Linear);
        assert_eq!(provider.provider_key(), "ticketing.linear");
    }
}

include!("tests.rs");
