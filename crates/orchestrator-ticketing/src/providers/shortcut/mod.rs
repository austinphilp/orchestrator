mod config;
mod transport;

use crate::interface::{TicketingProvider, TicketingProviderKind};
pub use config::ShortcutTicketingProviderConfig;
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

impl TicketingProvider for ShortcutTicketingProvider {
    fn kind(&self) -> TicketingProviderKind {
        TicketingProviderKind::Shortcut
    }
}

#[cfg(test)]
mod tests {
    use super::ShortcutTicketingProvider;
    use crate::interface::{TicketingProvider, TicketingProviderKind};

    #[test]
    fn scaffold_default_uses_shortcut_kind() {
        let provider = ShortcutTicketingProvider::scaffold_default();
        assert_eq!(provider.kind(), TicketingProviderKind::Shortcut);
        assert_eq!(provider.provider_key(), "ticketing.shortcut");
        assert_eq!(
            provider.config().api_url,
            "https://api.app.shortcut.com/api/v3"
        );
        assert_eq!(provider.transport().protocol, "rest");
    }
}
