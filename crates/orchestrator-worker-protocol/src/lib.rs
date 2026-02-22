//! Shared worker runtime protocol contracts.
//!
//! This crate owns canonical worker protocol IDs, events, runtime errors/results,
//! session request/handle types, and backend trait contracts.

pub mod backend;
pub mod error;
pub mod event;
pub mod ids;
pub mod session;

pub use backend::{
    WorkerBackendCapabilities, WorkerBackendInfo, WorkerBackendKind, WorkerEventStream,
    WorkerEventSubscription, WorkerSessionControl, WorkerSessionStreamSource,
};
pub use error::{WorkerRuntimeError, WorkerRuntimeResult};
pub use event::{
    WorkerArtifactEvent, WorkerArtifactKind, WorkerBlockedEvent, WorkerCheckpointEvent,
    WorkerCrashedEvent, WorkerDoneEvent, WorkerEvent, WorkerNeedsInputAnswer,
    WorkerNeedsInputEvent, WorkerNeedsInputOption, WorkerNeedsInputQuestion, WorkerOutputEvent,
    WorkerOutputStream, WorkerTurnStateEvent,
};
pub use ids::{WorkerArtifactId, WorkerSessionId};
pub use session::{WorkerSessionHandle, WorkerSpawnRequest};

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use crate::{
        WorkerBackendKind, WorkerEvent, WorkerEventStream, WorkerEventSubscription,
        WorkerRuntimeError, WorkerRuntimeResult, WorkerSessionId,
    };

    struct EmptyWorkerEventSubscription;

    #[async_trait]
    impl WorkerEventSubscription for EmptyWorkerEventSubscription {
        async fn next_event(&mut self) -> WorkerRuntimeResult<Option<WorkerEvent>> {
            Ok(None)
        }
    }

    #[test]
    fn worker_session_id_round_trips_as_json_string() {
        let session_id = WorkerSessionId::new("sess-1");
        let serialized = serde_json::to_string(&session_id).expect("serialize session id");
        let deserialized: WorkerSessionId =
            serde_json::from_str(&serialized).expect("deserialize session id");

        assert_eq!(serialized, "\"sess-1\"");
        assert_eq!(deserialized, session_id);
    }

    #[test]
    fn worker_backend_kind_serialization_is_stable_for_persistence() {
        let serialized =
            serde_json::to_string(&WorkerBackendKind::OpenCode).expect("serialize backend kind");
        let parsed: WorkerBackendKind =
            serde_json::from_str("\"OpenCode\"").expect("deserialize backend kind");

        assert_eq!(serialized, "\"OpenCode\"");
        assert_eq!(parsed, WorkerBackendKind::OpenCode);
    }

    #[test]
    fn worker_event_stream_alias_accepts_trait_objects() {
        let _stream: WorkerEventStream = Box::new(EmptyWorkerEventSubscription);
    }

    #[test]
    fn runtime_error_display_format_matches_legacy_runtime_wording() {
        assert_eq!(
            WorkerRuntimeError::Configuration("bad config".to_owned()).to_string(),
            "runtime configuration error: bad config"
        );
        assert_eq!(
            WorkerRuntimeError::Protocol("bad payload".to_owned()).to_string(),
            "runtime protocol error: bad payload"
        );
    }
}
