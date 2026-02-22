//! Startup/config/process lifecycle boundary for the app kernel.
//! RRP26 C01 keeps legacy logic wired here while domain modules are scaffolded.

mod legacy_kernel {
    include!("../lib/config_and_bootstrap.rs");
    include!("../lib/frontend_contract.rs");
    include!("../lib/app_runtime.rs");
    include!("../lib/runtime_stream_coordinator.rs");
    include!("../lib/frontend_controller.rs");
    include!("../lib/tests.rs");
}

pub use legacy_kernel::*;
