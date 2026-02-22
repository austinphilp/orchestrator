#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearTicketingTransport {
    pub protocol: String,
}

impl Default for LinearTicketingTransport {
    fn default() -> Self {
        Self {
            protocol: "graphql".to_owned(),
        }
    }
}
