#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutTicketingProviderConfig {
    pub api_url: String,
}

impl Default for ShortcutTicketingProviderConfig {
    fn default() -> Self {
        Self {
            api_url: "https://api.app.shortcut.com/api/v3".to_owned(),
        }
    }
}
