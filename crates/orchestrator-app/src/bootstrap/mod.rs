//! Startup/config/process lifecycle boundary for the app kernel.
//! Domain-owned bootstrap runtime wiring for the app kernel.

include!("../lib/config_and_bootstrap.rs");
include!("../lib/frontend_contract.rs");
include!("../lib/app_runtime.rs");
include!("../lib/runtime_stream_coordinator.rs");
include!("../lib/frontend_controller.rs");

#[cfg(test)]
include!("../lib/tests.rs");
