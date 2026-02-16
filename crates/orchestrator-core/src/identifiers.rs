use orchestrator_runtime::RuntimeSessionId;
use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(ProjectId);
string_id!(WorkItemId);
string_id!(WorktreeId);
string_id!(WorkerSessionId);
string_id!(InboxItemId);
string_id!(ArtifactId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketProvider {
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketId(String);

impl TicketId {
    pub fn from_provider_uuid(provider: TicketProvider, provider_uuid: impl AsRef<str>) -> Self {
        let provider_name = match provider {
            TicketProvider::Linear => "linear",
        };

        Self(format!("{provider_name}:{}", provider_uuid.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for TicketId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TicketId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<WorkerSessionId> for RuntimeSessionId {
    fn from(value: WorkerSessionId) -> Self {
        Self::from(value.as_str())
    }
}

impl From<RuntimeSessionId> for WorkerSessionId {
    fn from(value: RuntimeSessionId) -> Self {
        Self::from(value.as_str())
    }
}
