use serde::{Deserialize, Serialize};
use thiserror::Error;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("configuration error: {0}")]
    Configuration(String),
}

impl From<CoreError> for orchestrator_domain::CoreError {
    fn from(value: CoreError) -> Self {
        match value {
            CoreError::DependencyUnavailable(message) => Self::DependencyUnavailable(message),
            CoreError::Configuration(message) => Self::Configuration(message),
        }
    }
}

impl From<orchestrator_domain::CoreError> for CoreError {
    fn from(value: orchestrator_domain::CoreError) -> Self {
        match value {
            orchestrator_domain::CoreError::DependencyUnavailable(message) => {
                Self::DependencyUnavailable(message)
            }
            orchestrator_domain::CoreError::Configuration(message) => Self::Configuration(message),
            other => Self::DependencyUnavailable(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketProvider {
    Linear,
    Shortcut,
}

impl From<TicketProvider> for orchestrator_domain::TicketProvider {
    fn from(value: TicketProvider) -> Self {
        match value {
            TicketProvider::Linear => Self::Linear,
            TicketProvider::Shortcut => Self::Shortcut,
        }
    }
}

impl From<orchestrator_domain::TicketProvider> for TicketProvider {
    fn from(value: orchestrator_domain::TicketProvider) -> Self {
        match value {
            orchestrator_domain::TicketProvider::Linear => Self::Linear,
            orchestrator_domain::TicketProvider::Shortcut => Self::Shortcut,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TicketId(String);

impl TicketId {
    pub fn from_provider_uuid(provider: TicketProvider, provider_uuid: impl AsRef<str>) -> Self {
        let provider_name = match provider {
            TicketProvider::Linear => "linear",
            TicketProvider::Shortcut => "shortcut",
        };

        Self(format!("{provider_name}:{}", provider_uuid.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for TicketId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TicketId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<TicketId> for orchestrator_domain::TicketId {
    fn from(value: TicketId) -> Self {
        Self::from(value.0)
    }
}

impl From<orchestrator_domain::TicketId> for TicketId {
    fn from(value: orchestrator_domain::TicketId) -> Self {
        Self::from(value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowState {
    New,
    Planning,
    #[serde(alias = "Testing")]
    Implementing,
    PRDrafted,
    AwaitingYourReview,
    ReadyForReview,
    InReview,
    #[serde(alias = "Merging")]
    PendingMerge,
    Done,
    Abandoned,
}

impl From<WorkflowState> for orchestrator_domain::WorkflowState {
    fn from(value: WorkflowState) -> Self {
        match value {
            WorkflowState::New => Self::New,
            WorkflowState::Planning => Self::Planning,
            WorkflowState::Implementing => Self::Implementing,
            WorkflowState::PRDrafted => Self::PRDrafted,
            WorkflowState::AwaitingYourReview => Self::AwaitingYourReview,
            WorkflowState::ReadyForReview => Self::ReadyForReview,
            WorkflowState::InReview => Self::InReview,
            WorkflowState::PendingMerge => Self::PendingMerge,
            WorkflowState::Done => Self::Done,
            WorkflowState::Abandoned => Self::Abandoned,
        }
    }
}

impl From<orchestrator_domain::WorkflowState> for WorkflowState {
    fn from(value: orchestrator_domain::WorkflowState) -> Self {
        match value {
            orchestrator_domain::WorkflowState::New => Self::New,
            orchestrator_domain::WorkflowState::Planning => Self::Planning,
            orchestrator_domain::WorkflowState::Implementing => Self::Implementing,
            orchestrator_domain::WorkflowState::PRDrafted => Self::PRDrafted,
            orchestrator_domain::WorkflowState::AwaitingYourReview => Self::AwaitingYourReview,
            orchestrator_domain::WorkflowState::ReadyForReview => Self::ReadyForReview,
            orchestrator_domain::WorkflowState::InReview => Self::InReview,
            orchestrator_domain::WorkflowState::PendingMerge => Self::PendingMerge,
            orchestrator_domain::WorkflowState::Done => Self::Done,
            orchestrator_domain::WorkflowState::Abandoned => Self::Abandoned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TicketQuery {
    pub assigned_to_me: bool,
    pub states: Vec<String>,
    pub search: Option<String>,
    pub limit: Option<u32>,
}

impl From<TicketQuery> for orchestrator_domain::TicketQuery {
    fn from(value: TicketQuery) -> Self {
        Self {
            assigned_to_me: value.assigned_to_me,
            states: value.states,
            search: value.search,
            limit: value.limit,
        }
    }
}

impl From<orchestrator_domain::TicketQuery> for TicketQuery {
    fn from(value: orchestrator_domain::TicketQuery) -> Self {
        Self {
            assigned_to_me: value.assigned_to_me,
            states: value.states,
            search: value.search,
            limit: value.limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSummary {
    pub ticket_id: TicketId,
    pub identifier: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub state: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    pub updated_at: String,
}

impl From<TicketSummary> for orchestrator_domain::TicketSummary {
    fn from(value: TicketSummary) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            identifier: value.identifier,
            title: value.title,
            project: value.project,
            state: value.state,
            url: value.url,
            assignee: value.assignee,
            priority: value.priority,
            labels: value.labels,
            updated_at: value.updated_at,
        }
    }
}

impl From<orchestrator_domain::TicketSummary> for TicketSummary {
    fn from(value: orchestrator_domain::TicketSummary) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            identifier: value.identifier,
            title: value.title,
            project: value.project,
            state: value.state,
            url: value.url,
            assignee: value.assignee,
            priority: value.priority,
            labels: value.labels,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTicketRequest {
    pub title: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub state: Option<String>,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub assign_to_api_key_user: bool,
}

impl From<CreateTicketRequest> for orchestrator_domain::CreateTicketRequest {
    fn from(value: CreateTicketRequest) -> Self {
        Self {
            title: value.title,
            description: value.description,
            project: value.project,
            state: value.state,
            priority: value.priority,
            labels: value.labels,
            assign_to_api_key_user: value.assign_to_api_key_user,
        }
    }
}

impl From<orchestrator_domain::CreateTicketRequest> for CreateTicketRequest {
    fn from(value: orchestrator_domain::CreateTicketRequest) -> Self {
        Self {
            title: value.title,
            description: value.description,
            project: value.project,
            state: value.state,
            priority: value.priority,
            labels: value.labels,
            assign_to_api_key_user: value.assign_to_api_key_user,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTicketStateRequest {
    pub ticket_id: TicketId,
    pub state: String,
}

impl From<UpdateTicketStateRequest> for orchestrator_domain::UpdateTicketStateRequest {
    fn from(value: UpdateTicketStateRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            state: value.state,
        }
    }
}

impl From<orchestrator_domain::UpdateTicketStateRequest> for UpdateTicketStateRequest {
    fn from(value: orchestrator_domain::UpdateTicketStateRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            state: value.state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketAttachment {
    pub label: String,
    pub url: String,
}

impl From<TicketAttachment> for orchestrator_domain::TicketAttachment {
    fn from(value: TicketAttachment) -> Self {
        Self {
            label: value.label,
            url: value.url,
        }
    }
}

impl From<orchestrator_domain::TicketAttachment> for TicketAttachment {
    fn from(value: orchestrator_domain::TicketAttachment) -> Self {
        Self {
            label: value.label,
            url: value.url,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddTicketCommentRequest {
    pub ticket_id: TicketId,
    pub comment: String,
    pub attachments: Vec<TicketAttachment>,
}

impl From<AddTicketCommentRequest> for orchestrator_domain::AddTicketCommentRequest {
    fn from(value: AddTicketCommentRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            comment: value.comment,
            attachments: value.attachments.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<orchestrator_domain::AddTicketCommentRequest> for AddTicketCommentRequest {
    fn from(value: orchestrator_domain::AddTicketCommentRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            comment: value.comment,
            attachments: value.attachments.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTicketRequest {
    pub ticket_id: TicketId,
}

impl From<GetTicketRequest> for orchestrator_domain::GetTicketRequest {
    fn from(value: GetTicketRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
        }
    }
}

impl From<orchestrator_domain::GetTicketRequest> for GetTicketRequest {
    fn from(value: orchestrator_domain::GetTicketRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketDetails {
    #[serde(flatten)]
    pub summary: TicketSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl From<TicketDetails> for orchestrator_domain::TicketDetails {
    fn from(value: TicketDetails) -> Self {
        Self {
            summary: value.summary.into(),
            description: value.description,
        }
    }
}

impl From<orchestrator_domain::TicketDetails> for TicketDetails {
    fn from(value: orchestrator_domain::TicketDetails) -> Self {
        Self {
            summary: value.summary.into(),
            description: value.description,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTicketDescriptionRequest {
    pub ticket_id: TicketId,
    pub description: String,
}

impl From<UpdateTicketDescriptionRequest> for orchestrator_domain::UpdateTicketDescriptionRequest {
    fn from(value: UpdateTicketDescriptionRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            description: value.description,
        }
    }
}

impl From<orchestrator_domain::UpdateTicketDescriptionRequest> for UpdateTicketDescriptionRequest {
    fn from(value: orchestrator_domain::UpdateTicketDescriptionRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
            description: value.description,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTicketRequest {
    pub ticket_id: TicketId,
}

impl From<ArchiveTicketRequest> for orchestrator_domain::ArchiveTicketRequest {
    fn from(value: ArchiveTicketRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
        }
    }
}

impl From<orchestrator_domain::ArchiveTicketRequest> for ArchiveTicketRequest {
    fn from(value: orchestrator_domain::ArchiveTicketRequest) -> Self {
        Self {
            ticket_id: value.ticket_id.into(),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::{CoreError, TicketId, TicketProvider, TicketingProviderKind};

    #[test]
    fn core_error_roundtrip_preserves_known_variants() {
        let dependency = CoreError::DependencyUnavailable("missing tool".to_owned());
        let domain_dependency: orchestrator_domain::CoreError = dependency.clone().into();
        assert_eq!(CoreError::from(domain_dependency), dependency);

        let configuration = CoreError::Configuration("invalid key".to_owned());
        let domain_configuration: orchestrator_domain::CoreError = configuration.clone().into();
        assert_eq!(CoreError::from(domain_configuration), configuration);
    }

    #[test]
    fn core_error_from_domain_maps_non_contract_variants_to_dependency_unavailable() {
        let converted = CoreError::from(orchestrator_domain::CoreError::Persistence(
            "sqlite busy".to_owned(),
        ));
        assert!(matches!(converted, CoreError::DependencyUnavailable(_)));
    }

    #[test]
    fn ticket_id_roundtrip_with_domain_preserves_value() {
        let domain_id = orchestrator_domain::TicketId::from_provider_uuid(
            orchestrator_domain::TicketProvider::Linear,
            "issue-347",
        );
        let interface_id: TicketId = domain_id.clone().into();
        let roundtrip: orchestrator_domain::TicketId = interface_id.into();
        assert_eq!(roundtrip, domain_id);
    }

    #[test]
    fn provider_key_mapping_matches_enum_values() {
        assert_eq!(
            TicketingProviderKind::from_key("ticketing.linear"),
            Some(TicketingProviderKind::Linear)
        );
        assert_eq!(
            TicketingProviderKind::from_key("ticketing.shortcut"),
            Some(TicketingProviderKind::Shortcut)
        );
        assert_eq!(
            TicketingProviderKind::from_provider(TicketProvider::Linear).as_key(),
            "ticketing.linear"
        );
        assert_eq!(
            TicketingProviderKind::from_provider(TicketProvider::Shortcut).as_key(),
            "ticketing.shortcut"
        );
    }
}
