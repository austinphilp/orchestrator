pub mod attention;
pub mod bootstrap;
pub mod commands;
pub mod composition;
pub mod controller;
pub mod error;
pub mod events;
pub mod frontend;
pub mod jobs;
pub mod normalization;
pub mod projection;

// Temporary compatibility bridge while C-phase tickets move symbols into domain modules.
pub use bootstrap::*;

#[cfg(test)]
mod module_tree_tests {
    use std::any::TypeId;

    #[test]
    fn exposes_domain_first_modules() {
        use crate::attention as _;
        use crate::bootstrap as _;
        use crate::commands as _;
        use crate::composition as _;
        use crate::controller as _;
        use crate::error as _;
        use crate::events as _;
        use crate::frontend as _;
        use crate::jobs as _;
        use crate::normalization as _;
        use crate::projection as _;
    }

    #[test]
    fn root_exports_match_bootstrap_bridge() {
        assert_eq!(
            TypeId::of::<crate::AppConfig>(),
            TypeId::of::<crate::bootstrap::AppConfig>()
        );
        assert_eq!(
            TypeId::of::<crate::DatabaseConfigToml>(),
            TypeId::of::<crate::bootstrap::DatabaseConfigToml>()
        );
        assert_eq!(
            TypeId::of::<crate::WorkerManagerBackend>(),
            TypeId::of::<crate::bootstrap::WorkerManagerBackend>()
        );

        let _set_supervisor_model: fn(String) = crate::set_supervisor_model_config;
        let _set_git_binary: fn(String) = crate::set_git_binary_config;
        let _set_database_runtime: fn(crate::DatabaseConfigToml) =
            crate::set_database_runtime_config;
    }
}
