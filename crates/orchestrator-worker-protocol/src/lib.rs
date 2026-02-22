//! Shared worker runtime protocol scaffold.
//!
//! This crate is intentionally minimal in RRP26 A01 and only establishes
//! crate/module boundaries for follow-on implementation phases.

pub mod backend;
pub mod error;
pub mod event;
pub mod ids;
pub mod session;

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use crate::backend::{WorkerBackendKind, WorkerEventStream, WorkerEventSubscription};
    use crate::error::WorkerRuntimeResult;
    use crate::event::WorkerEvent;
    use crate::ids::WorkerSessionId;

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
}
