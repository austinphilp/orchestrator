#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutTicketingTransport {
    pub protocol: String,
}

impl Default for ShortcutTicketingTransport {
    fn default() -> Self {
        Self {
            protocol: "rest".to_owned(),
        }
    }
}
