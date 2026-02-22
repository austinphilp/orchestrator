pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, resolve_provider_kind, supported_provider_keys, HarnessProviderFactoryOutput,
};
pub use interface::{
    HarnessBackendCapabilities, HarnessBackendInfo, HarnessBackendKind, HarnessEvent,
    HarnessEventStream, HarnessEventSubscription, HarnessProvider, HarnessProviderError,
    HarnessProviderKind, HarnessProviderSessionContract, HarnessRuntimeError,
    HarnessRuntimeProvider, HarnessRuntimeResult, HarnessSessionControl, HarnessSessionHandle,
    HarnessSessionId, HarnessSessionStreamSource, HarnessSpawnRequest,
};
pub use providers::codex::CodexHarnessProvider;
pub use providers::opencode::{
    OpenCodeBackend, OpenCodeBackendConfig, OpenCodeHarnessProvider, OpenCodeHarnessProviderConfig,
};
