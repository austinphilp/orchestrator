mod provider_impl;

use crate::interface::{HarnessProvider, HarnessProviderKind};

pub use provider_impl::{OpenCodeBackend, OpenCodeBackendConfig};

pub type OpenCodeHarnessProvider = OpenCodeBackend;
pub type OpenCodeHarnessProviderConfig = OpenCodeBackendConfig;

impl OpenCodeBackend {
    pub fn scaffold_default() -> Self {
        Self::new(OpenCodeBackendConfig::default())
    }
}

impl HarnessProvider for OpenCodeBackend {
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
    }
}
