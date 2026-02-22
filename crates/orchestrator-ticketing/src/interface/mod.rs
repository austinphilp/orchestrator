pub use orchestrator_core::TicketingProvider as CoreTicketingProvider;
use thiserror::Error;

pub use orchestrator_core::{
    AddTicketCommentRequest, ArchiveTicketRequest, CoreError, CreateTicketRequest,
    GetTicketRequest, TicketAttachment, TicketDetails, TicketId, TicketProvider, TicketQuery,
    TicketSummary, UpdateTicketDescriptionRequest, UpdateTicketStateRequest, WorkflowState,
};

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

    pub const fn from_provider(provider: TicketProvider) -> Self {
        match provider {
            TicketProvider::Linear => Self::Linear,
            TicketProvider::Shortcut => Self::Shortcut,
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

pub trait TicketingProvider: CoreTicketingProvider + Send + Sync {
    fn kind(&self) -> TicketingProviderKind {
        TicketingProviderKind::from_provider(self.provider())
    }

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

impl<T> TicketingProvider for T where T: CoreTicketingProvider + Send + Sync + ?Sized {}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TicketingProviderError {
    #[error("unknown ticketing provider key: {0}")]
    UnknownProviderKey(String),
    #[error("failed to initialize ticketing provider: {0}")]
    ProviderInitialization(String),
}
