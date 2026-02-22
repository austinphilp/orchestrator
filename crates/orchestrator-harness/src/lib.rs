pub mod factory;
pub mod interface;
pub mod providers;

pub use factory::{
    build_provider, build_provider_with_config, resolve_provider_kind, supported_provider_keys,
    HarnessProviderFactoryConfig, HarnessProviderFactoryOutput,
};
pub use interface::{
    HarnessBackendCapabilities, HarnessBackendInfo, HarnessBackendKind, HarnessEvent,
    HarnessEventStream, HarnessEventSubscription, HarnessProvider, HarnessProviderError,
    HarnessProviderKind, HarnessProviderSessionContract, HarnessRuntimeError,
    HarnessRuntimeProvider, HarnessRuntimeResult, HarnessSessionControl, HarnessSessionHandle,
    HarnessSessionId, HarnessSessionStreamSource, HarnessSpawnRequest,
};
pub use providers::codex::{
    CodexBackend, CodexBackendConfig, CodexHarnessProvider, CodexHarnessProviderConfig,
};
pub use providers::opencode::{
    OpenCodeBackend, OpenCodeBackendConfig, OpenCodeHarnessProvider, OpenCodeHarnessProviderConfig,
};
