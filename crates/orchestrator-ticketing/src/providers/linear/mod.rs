mod config;
mod transport;

use crate::interface::{TicketingProvider, TicketingProviderKind};
pub use config::LinearTicketingProviderConfig;
pub use transport::LinearTicketingTransport;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinearTicketingProvider {
    config: LinearTicketingProviderConfig,
    transport: LinearTicketingTransport,
}

impl LinearTicketingProvider {
    pub fn new(config: LinearTicketingProviderConfig, transport: LinearTicketingTransport) -> Self {
        Self { config, transport }
    }

    pub fn scaffold_default() -> Self {
        Self::default()
    }

    pub fn config(&self) -> &LinearTicketingProviderConfig {
        &self.config
    }

    pub fn transport(&self) -> &LinearTicketingTransport {
        &self.transport
    }
}

impl TicketingProvider for LinearTicketingProvider {
    fn kind(&self) -> TicketingProviderKind {
        TicketingProviderKind::Linear
    }
}

#[cfg(test)]
mod tests {
    use super::LinearTicketingProvider;
    use crate::interface::{TicketingProvider, TicketingProviderKind};

    #[test]
    fn scaffold_default_uses_linear_kind() {
        let provider = LinearTicketingProvider::scaffold_default();
        assert_eq!(provider.kind(), TicketingProviderKind::Linear);
        assert_eq!(provider.provider_key(), "ticketing.linear");
        assert_eq!(provider.config().api_url, "https://api.linear.app/graphql");
        assert_eq!(provider.transport().protocol, "graphql");
    }
}
