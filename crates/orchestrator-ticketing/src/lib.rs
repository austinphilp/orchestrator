pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
    TicketingProviderFactoryConfig, TicketingProviderFactoryOutput,
};
pub use interface::{
    AddTicketCommentRequest, ArchiveTicketRequest, CoreError, CreateTicketRequest,
    GetTicketRequest, TicketAttachment, TicketDetails, TicketId, TicketProvider, TicketQuery,
    TicketSummary, TicketingProvider, TicketingProviderError, TicketingProviderKind,
    UpdateTicketDescriptionRequest, UpdateTicketStateRequest, WorkflowState,
};
pub use providers::linear::{
    GraphqlRequest, GraphqlTransport, LinearConfig, LinearRuntimeSettings, LinearTicketingProvider,
    LinearWorkflowSyncConfig, LinearWorkflowTransitionSyncRequest, ReqwestGraphqlTransport,
    TicketCacheSnapshot, WorkflowStateMapSetting, WorkflowStateMapping,
};
pub use providers::shortcut::ShortcutTicketingProvider;
