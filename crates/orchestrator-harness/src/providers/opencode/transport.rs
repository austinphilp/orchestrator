#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeHarnessTransport {
    pub protocol: String,
}

impl Default for OpenCodeHarnessTransport {
    fn default() -> Self {
        Self {
            protocol: "http".to_owned(),
        }
    }
}
