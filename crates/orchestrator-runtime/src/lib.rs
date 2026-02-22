//! Legacy runtime crate.
//!
//! Frozen for feature work as of RRP26 A01. Additive runtime decomposition work
//! must land in the `orchestrator-worker-*` crates instead.

use async_trait::async_trait;

mod worker_manager;
pub use worker_manager::{
    ManagedSessionStatus, ManagedSessionSummary, SessionEventSubscription, SessionVisibility,
    WorkerManager, WorkerManagerConfig, WorkerManagerEvent, WorkerManagerEventSubscription,
    WorkerManagerPerfSessionSnapshot, WorkerManagerPerfSnapshot, WorkerManagerPerfTotals,
};

pub use orchestrator_worker_protocol::backend::{
    WorkerBackendCapabilities as BackendCapabilities, WorkerBackendInfo,
    WorkerBackendKind as BackendKind, WorkerEventStream, WorkerEventSubscription,
    WorkerSessionControl as SessionLifecycle, WorkerSessionStreamSource,
};
pub use orchestrator_worker_protocol::error::{
    WorkerRuntimeError as RuntimeError, WorkerRuntimeResult as RuntimeResult,
};
pub use orchestrator_worker_protocol::event::{
    WorkerArtifactEvent as BackendArtifactEvent, WorkerArtifactKind as BackendArtifactKind,
    WorkerBlockedEvent as BackendBlockedEvent, WorkerCheckpointEvent as BackendCheckpointEvent,
    WorkerCrashedEvent as BackendCrashedEvent, WorkerDoneEvent as BackendDoneEvent,
    WorkerEvent as BackendEvent, WorkerNeedsInputAnswer as BackendNeedsInputAnswer,
    WorkerNeedsInputEvent as BackendNeedsInputEvent,
    WorkerNeedsInputOption as BackendNeedsInputOption,
    WorkerNeedsInputQuestion as BackendNeedsInputQuestion, WorkerOutputEvent as BackendOutputEvent,
    WorkerOutputStream as BackendOutputStream, WorkerTurnStateEvent as BackendTurnStateEvent,
};
pub use orchestrator_worker_protocol::ids::{
    WorkerArtifactId as RuntimeArtifactId, WorkerSessionId as RuntimeSessionId,
};
pub use orchestrator_worker_protocol::session::{
    WorkerSessionHandle as SessionHandle, WorkerSpawnRequest as SpawnSpec,
};

#[async_trait]
pub trait WorkerBackend: SessionLifecycle + Send + Sync {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> BackendCapabilities;

    async fn health_check(&self) -> RuntimeResult<()>;
    async fn subscribe(&self, session: &SessionHandle) -> RuntimeResult<WorkerEventStream>;
    async fn harness_session_id(&self, _session: &SessionHandle) -> RuntimeResult<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyWorkerStream;

    #[async_trait]
    impl WorkerEventSubscription for EmptyWorkerStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<BackendEvent>> {
            Ok(None)
        }
    }

    #[test]
    fn runtime_session_id_round_trips_as_string() {
        let session_id = RuntimeSessionId::new("sess-1");
        let serialized = serde_json::to_string(&session_id).expect("serialize session id");
        let deserialized: RuntimeSessionId =
            serde_json::from_str(&serialized).expect("deserialize session id");

        assert_eq!(serialized, "\"sess-1\"");
        assert_eq!(deserialized, session_id);
    }

    #[test]
    fn backend_kind_serialization_is_stable_for_persistence() {
        let serialized = serde_json::to_string(&BackendKind::OpenCode).expect("serialize kind");
        let parsed: BackendKind = serde_json::from_str("\"OpenCode\"").expect("parse kind");

        assert_eq!(serialized, "\"OpenCode\"");
        assert_eq!(parsed, BackendKind::OpenCode);
    }

    #[test]
    fn worker_event_stream_alias_accepts_trait_objects() {
        let _stream: WorkerEventStream = Box::new(EmptyWorkerStream);
    }

    #[test]
    fn runtime_aliases_round_trip_with_worker_protocol_json() {
        let protocol_session = orchestrator_worker_protocol::WorkerSessionId::new("sess-interop");
        let runtime_session: RuntimeSessionId = serde_json::from_str(
            &serde_json::to_string(&protocol_session).expect("serialize protocol session id"),
        )
        .expect("deserialize runtime session id");
        assert_eq!(runtime_session.as_str(), "sess-interop");

        let protocol_event = orchestrator_worker_protocol::WorkerEvent::Done(
            orchestrator_worker_protocol::WorkerDoneEvent {
                summary: Some("finished".to_owned()),
            },
        );
        let runtime_event: BackendEvent = serde_json::from_str(
            &serde_json::to_string(&protocol_event).expect("serialize protocol event"),
        )
        .expect("deserialize runtime event");
        assert_eq!(
            runtime_event,
            BackendEvent::Done(BackendDoneEvent {
                summary: Some("finished".to_owned()),
            })
        );
    }

    #[test]
    fn runtime_error_display_wording_is_stable() {
        assert_eq!(
            RuntimeError::Process("boom".to_owned()).to_string(),
            "runtime process error: boom"
        );
    }
}
