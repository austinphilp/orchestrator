#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeHarnessProviderConfig {
    pub binary_name: String,
}

impl Default for OpenCodeHarnessProviderConfig {
    fn default() -> Self {
        Self {
            binary_name: "opencode".to_owned(),
        }
    }
}
