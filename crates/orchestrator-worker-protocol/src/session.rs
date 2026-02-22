use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::backend::WorkerBackendKind;
use crate::ids::WorkerSessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSpawnRequest {
    pub session_id: WorkerSessionId,
    pub workdir: PathBuf,
    pub model: Option<String>,
    pub instruction_prelude: Option<String>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSessionHandle {
    pub session_id: WorkerSessionId,
    pub backend: WorkerBackendKind,
}
