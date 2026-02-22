pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
    VcsProviderFactoryConfig, VcsProviderFactoryOutput,
};
pub use interface::{
    CoreError, CreateWorktreeRequest, DeleteWorktreeRequest, RepositoryRef, VcsProvider,
    VcsProviderError, VcsProviderKind, WorktreeManager, WorktreeStatus, WorktreeSummary,
};
pub use providers::git_cli::{
    CommandRunner, GitCliVcsProvider, GitCliVcsProviderConfig, ProcessCommandRunner,
};
