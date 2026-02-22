#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearTicketingProviderConfig {
    pub api_url: String,
}

impl Default for LinearTicketingProviderConfig {
    fn default() -> Self {
        Self {
            api_url: "https://api.linear.app/graphql".to_owned(),
        }
    }
}
