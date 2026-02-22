mod provider_impl;

use crate::interface::{HarnessProvider, HarnessProviderKind};

pub use provider_impl::{CodexBackend, CodexBackendConfig};

pub type CodexHarnessProvider = CodexBackend;
pub type CodexHarnessProviderConfig = CodexBackendConfig;

impl CodexBackend {
    pub fn scaffold_default() -> Self {
        Self::new(CodexBackendConfig::default())
    }
}

impl HarnessProvider for CodexBackend {
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
    }
}
