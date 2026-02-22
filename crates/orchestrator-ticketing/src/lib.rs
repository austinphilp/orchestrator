pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, resolve_provider_kind, supported_provider_keys, TicketingProviderFactoryOutput,
};
pub use interface::{
    TicketingProvider, TicketingProviderError, TicketingProviderKind, TicketingTicketCreateRequest,
    TicketingTicketCreateResponse,
};
pub use providers::linear::LinearTicketingProvider;
pub use providers::shortcut::ShortcutTicketingProvider;
