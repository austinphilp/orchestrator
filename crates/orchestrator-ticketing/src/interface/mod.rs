use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketingProviderKind {
    Linear,
    Shortcut,
}

impl TicketingProviderKind {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Linear => "ticketing.linear",
            Self::Shortcut => "ticketing.shortcut",
        }
    }

    pub fn from_key(provider_key: &str) -> Option<Self> {
        match provider_key {
            "ticketing.linear" => Some(Self::Linear),
            "ticketing.shortcut" => Some(Self::Shortcut),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketingTicketCreateRequest {
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketingTicketCreateResponse {
    pub provider_ticket_id: String,
}

pub trait TicketingProvider: Send + Sync {
    fn kind(&self) -> TicketingProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TicketingProviderError {
    #[error("unknown ticketing provider key: {0}")]
    UnknownProviderKey(String),
}
