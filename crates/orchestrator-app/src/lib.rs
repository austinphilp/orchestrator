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
pub use error::{AppError, AppResult};
pub use events::{
    ArtifactCreatedPayload, InboxItemCreatedPayload, InboxItemResolvedPayload, NewEventEnvelope,
    OrchestrationEventPayload, SessionBlockedPayload, SessionCompletedPayload,
    SessionNeedsInputPayload, SessionSpawnedPayload, StoredEventEnvelope, UserRespondedPayload,
};
pub use orchestrator_core::{ArtifactId, ArtifactKind, WorkItemId, WorkflowState};
pub use projection::{InboxItemProjection, ProjectionState, SessionProjection, WorkItemProjection};

#[cfg(test)]
mod module_tree_tests {
    use std::any::TypeId;

    #[test]
    #[allow(unused_imports)]
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
    }
}
