pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, resolve_provider_kind, supported_provider_keys, VcsProviderFactoryOutput,
};
pub use interface::{
    VcsCommandRequest, VcsCommandResult, VcsProvider, VcsProviderError, VcsProviderKind,
};
pub use providers::git_cli::GitCliVcsProvider;
