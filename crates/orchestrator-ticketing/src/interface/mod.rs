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

#[async_trait::async_trait]
pub trait TicketingProvider: Send + Sync {
    fn provider(&self) -> TicketProvider;
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn list_tickets(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError>;
    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
    }
    async fn create_ticket(&self, request: CreateTicketRequest)
        -> Result<TicketSummary, CoreError>;
    async fn get_ticket(&self, _request: GetTicketRequest) -> Result<TicketDetails, CoreError> {
        Err(CoreError::DependencyUnavailable(format!(
            "get_ticket is not implemented by {:?} provider",
            self.provider()
        )))
    }
    async fn update_ticket_state(&self, request: UpdateTicketStateRequest)
        -> Result<(), CoreError>;
    async fn update_ticket_description(
        &self,
        _request: UpdateTicketDescriptionRequest,
    ) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(format!(
            "update_ticket_description is not implemented by {:?} provider",
            self.provider()
        )))
    }
    async fn archive_ticket(&self, _request: ArchiveTicketRequest) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(format!(
            "archive_ticket is not implemented by {:?} provider",
            self.provider()
        )))
    }
    async fn add_comment(&self, request: AddTicketCommentRequest) -> Result<(), CoreError>;

    fn kind(&self) -> TicketingProviderKind {
        TicketingProviderKind::from_provider(self.provider())
    }

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TicketingProviderError {
    #[error("unknown ticketing provider key: {0}")]
    UnknownProviderKey(String),
    #[error("failed to initialize ticketing provider: {0}")]
    ProviderInitialization(String),
}
