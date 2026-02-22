mod config;
mod transport;

use crate::interface::{HarnessProvider, HarnessProviderKind};
pub use config::OpenCodeHarnessProviderConfig;
pub use transport::OpenCodeHarnessTransport;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenCodeHarnessProvider {
    config: OpenCodeHarnessProviderConfig,
    transport: OpenCodeHarnessTransport,
}

impl OpenCodeHarnessProvider {
    pub fn new(config: OpenCodeHarnessProviderConfig, transport: OpenCodeHarnessTransport) -> Self {
        Self { config, transport }
    }

    pub fn scaffold_default() -> Self {
        Self::default()
    }

    pub fn config(&self) -> &OpenCodeHarnessProviderConfig {
        &self.config
    }

    pub fn transport(&self) -> &OpenCodeHarnessTransport {
        &self.transport
    }
}

impl HarnessProvider for OpenCodeHarnessProvider {
    fn kind(&self) -> HarnessProviderKind {
        HarnessProviderKind::OpenCode
    }
}

#[cfg(test)]
mod tests {
    use super::OpenCodeHarnessProvider;
    use crate::interface::{HarnessProvider, HarnessProviderKind};

    #[test]
    fn scaffold_default_uses_opencode_kind() {
        let provider = OpenCodeHarnessProvider::scaffold_default();
        assert_eq!(provider.kind(), HarnessProviderKind::OpenCode);
        assert_eq!(provider.provider_key(), "harness.opencode");
        assert_eq!(provider.config().binary_name, "opencode");
        assert_eq!(provider.transport().protocol, "http");
    }
}
