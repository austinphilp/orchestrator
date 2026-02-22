mod config;
mod transport;

use crate::interface::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, TicketProvider, TicketQuery,
    TicketSummary, UpdateTicketStateRequest,
};
pub use config::ShortcutTicketingProviderConfig;
use orchestrator_core::TicketingProvider;
pub use transport::ShortcutTicketingTransport;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShortcutTicketingProvider {
    config: ShortcutTicketingProviderConfig,
    transport: ShortcutTicketingTransport,
}

impl ShortcutTicketingProvider {
    pub fn new(
        config: ShortcutTicketingProviderConfig,
        transport: ShortcutTicketingTransport,
    ) -> Self {
        Self { config, transport }
    }

    pub fn scaffold_default() -> Self {
        Self::default()
    }

    pub fn config(&self) -> &ShortcutTicketingProviderConfig {
        &self.config
    }

    pub fn transport(&self) -> &ShortcutTicketingTransport {
        &self.transport
    }
}

#[async_trait::async_trait]
impl TicketingProvider for ShortcutTicketingProvider {
    fn provider(&self) -> TicketProvider {
        TicketProvider::Shortcut
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "shortcut ticketing provider is not yet ported to orchestrator-ticketing".to_owned(),
        ))
    }

    async fn list_tickets(&self, _query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "shortcut ticketing provider is not yet ported to orchestrator-ticketing".to_owned(),
        ))
    }

    async fn create_ticket(
        &self,
        _request: CreateTicketRequest,
    ) -> Result<TicketSummary, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "shortcut ticketing provider is not yet ported to orchestrator-ticketing".to_owned(),
        ))
    }

    async fn update_ticket_state(
        &self,
        _request: UpdateTicketStateRequest,
    ) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "shortcut ticketing provider is not yet ported to orchestrator-ticketing".to_owned(),
        ))
    }

    async fn add_comment(&self, _request: AddTicketCommentRequest) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "shortcut ticketing provider is not yet ported to orchestrator-ticketing".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::ShortcutTicketingProvider;
    use crate::interface::{TicketingProvider, TicketingProviderKind};
    use orchestrator_core::TicketingProvider as CoreTicketingProvider;

    #[test]
    fn scaffold_default_uses_shortcut_kind() {
        let provider = ShortcutTicketingProvider::scaffold_default();
        assert_eq!(
            CoreTicketingProvider::provider(&provider),
            crate::interface::TicketProvider::Shortcut
        );
        assert_eq!(provider.kind(), TicketingProviderKind::Shortcut);
        assert_eq!(provider.provider_key(), "ticketing.shortcut");
        assert_eq!(
            provider.config().api_url,
            "https://api.app.shortcut.com/api/v3"
        );
        assert_eq!(provider.transport().protocol, "rest");
    }
}
