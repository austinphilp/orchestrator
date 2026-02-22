#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHarnessTransport {
    pub protocol: String,
}

impl Default for CodexHarnessTransport {
    fn default() -> Self {
        Self {
            protocol: "json-rpc".to_owned(),
        }
    }
}
