//! Dependency wiring and assembly boundary.
//! RRP26 D03 wires typed config slices through app bootstrap/composition paths.

use crate::bootstrap::{
    AppConfig, DatabaseRuntimeConfig, GitRuntimeConfig, SupervisorRuntimeConfig, UiViewConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRuntimeConfigSlices {
    pub supervisor: SupervisorRuntimeConfig,
    pub database: DatabaseRuntimeConfig,
    pub git: GitRuntimeConfig,
    pub ui_view: UiViewConfig,
}

pub fn runtime_config_slices(config: &AppConfig) -> AppRuntimeConfigSlices {
    AppRuntimeConfigSlices {
        supervisor: config.supervisor_runtime(),
        database: config.database_runtime(),
        git: config.git_runtime(),
        ui_view: config.ui_view(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_config_slices_match_config_views() {
        let config = AppConfig::default();
        let slices = runtime_config_slices(&config);

        assert_eq!(slices.supervisor, config.supervisor_runtime());
        assert_eq!(slices.database, config.database_runtime());
        assert_eq!(slices.git, config.git_runtime());
        assert_eq!(slices.ui_view, config.ui_view());
    }
}
