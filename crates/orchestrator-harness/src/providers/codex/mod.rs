mod config;
mod transport;

use crate::interface::{HarnessProvider, HarnessProviderKind};
pub use config::CodexHarnessProviderConfig;
pub use transport::CodexHarnessTransport;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexHarnessProvider {
    config: CodexHarnessProviderConfig,
    transport: CodexHarnessTransport,
}

impl CodexHarnessProvider {
    pub fn new(config: CodexHarnessProviderConfig, transport: CodexHarnessTransport) -> Self {
        Self { config, transport }
    }

    pub fn scaffold_default() -> Self {
        Self::default()
    }

    pub fn config(&self) -> &CodexHarnessProviderConfig {
        &self.config
    }

    pub fn transport(&self) -> &CodexHarnessTransport {
        &self.transport
    }
}

impl HarnessProvider for CodexHarnessProvider {
    fn kind(&self) -> HarnessProviderKind {
        HarnessProviderKind::Codex
    }
}

#[cfg(test)]
mod tests {
    use super::CodexHarnessProvider;
    use crate::interface::{HarnessProvider, HarnessProviderKind};

    #[test]
    fn scaffold_default_uses_codex_kind() {
        let provider = CodexHarnessProvider::scaffold_default();
        assert_eq!(provider.kind(), HarnessProviderKind::Codex);
        assert_eq!(provider.provider_key(), "harness.codex");
        assert_eq!(provider.config().endpoint_name, "codex-app-server");
        assert_eq!(provider.transport().protocol, "json-rpc");
    }
}
