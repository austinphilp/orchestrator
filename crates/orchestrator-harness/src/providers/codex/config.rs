#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHarnessProviderConfig {
    pub endpoint_name: String,
}

impl Default for CodexHarnessProviderConfig {
    fn default() -> Self {
        Self {
            endpoint_name: "codex-app-server".to_owned(),
        }
    }
}
