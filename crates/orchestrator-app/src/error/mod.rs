//! App-level orchestration error surface boundary.
//!
//! During the C-phase migration this owns app-facing conversion glue at
//! integration edges while legacy `orchestrator-core::CoreError` paths remain.

use orchestrator_core::{CoreError, RuntimeError};
use orchestrator_harness::HarnessProviderError;
use orchestrator_ticketing::TicketingProviderError;
use orchestrator_vcs::VcsProviderError;
use orchestrator_vcs_repos::VcsRepoProviderError;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
#[error(transparent)]
pub struct AppError(#[from] CoreError);

impl AppError {
    pub fn configuration(message: impl Into<String>) -> Self {
        Self(CoreError::Configuration(message.into()))
    }

    pub fn dependency_unavailable(message: impl Into<String>) -> Self {
        Self(CoreError::DependencyUnavailable(message.into()))
    }

    pub fn persistence(message: impl Into<String>) -> Self {
        Self(CoreError::Persistence(message.into()))
    }

    pub fn as_core(&self) -> &CoreError {
        &self.0
    }

    pub fn into_core(self) -> CoreError {
        self.0
    }

    pub fn configuration_message(&self) -> Option<&str> {
        match self.as_core() {
            CoreError::Configuration(message) => Some(message.as_str()),
            _ => None,
        }
    }
}

impl From<AppError> for CoreError {
    fn from(value: AppError) -> Self {
        value.0
    }
}

impl From<RuntimeError> for AppError {
    fn from(value: RuntimeError) -> Self {
        Self(CoreError::Runtime(value))
    }
}

impl From<HarnessProviderError> for AppError {
    fn from(value: HarnessProviderError) -> Self {
        Self::configuration(value.to_string())
    }
}

impl From<TicketingProviderError> for AppError {
    fn from(value: TicketingProviderError) -> Self {
        Self::configuration(value.to_string())
    }
}

impl From<VcsProviderError> for AppError {
    fn from(value: VcsProviderError) -> Self {
        Self::configuration(value.to_string())
    }
}

impl From<VcsRepoProviderError> for AppError {
    fn from(value: VcsRepoProviderError) -> Self {
        Self::configuration(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_error_conversion_preserves_runtime_variant() {
        let error = AppError::from(RuntimeError::Protocol("tag parse failed".to_owned()));

        match error.into_core() {
            CoreError::Runtime(RuntimeError::Protocol(reason)) => {
                assert_eq!(reason, "tag parse failed");
            }
            other => panic!("unexpected app error conversion result: {other:?}"),
        }
    }

    #[test]
    fn provider_factory_error_conversions_map_to_configuration_errors() {
        let harness = AppError::from(HarnessProviderError::UnknownProviderKey(
            "harness.legacy".to_owned(),
        ));
        let ticketing = AppError::from(TicketingProviderError::ProviderInitialization(
            "ticketing failure".to_owned(),
        ));
        let vcs = AppError::from(VcsProviderError::UnknownProviderKey(
            "vcs.legacy".to_owned(),
        ));
        let vcs_repos = AppError::from(VcsRepoProviderError::ProviderInitialization(
            "repo init failure".to_owned(),
        ));

        for error in [harness, ticketing, vcs, vcs_repos] {
            assert!(
                matches!(error.as_core(), CoreError::Configuration(_)),
                "expected configuration mapping for provider error"
            );
        }
    }

    #[test]
    fn provider_factory_error_conversions_preserve_message_details() {
        let cases = [
            (
                AppError::from(HarnessProviderError::UnknownProviderKey(
                    "harness.legacy".to_owned(),
                )),
                "unknown harness provider key: harness.legacy",
            ),
            (
                AppError::from(TicketingProviderError::ProviderInitialization(
                    "ticketing failure".to_owned(),
                )),
                "failed to initialize ticketing provider: ticketing failure",
            ),
            (
                AppError::from(VcsProviderError::UnknownProviderKey(
                    "vcs.legacy".to_owned(),
                )),
                "unknown vcs provider key: vcs.legacy",
            ),
            (
                AppError::from(VcsRepoProviderError::ProviderInitialization(
                    "repo init failure".to_owned(),
                )),
                "failed to initialize vcs repo provider: repo init failure",
            ),
        ];

        for (error, expected_detail) in cases {
            let Some(message) = error.configuration_message() else {
                panic!("expected configuration message in converted app error");
            };
            assert_eq!(message, expected_detail);
            assert!(error.to_string().contains(expected_detail));
        }
    }

    #[test]
    fn app_error_wraps_core_error_as_source() {
        let error = AppError::from(CoreError::Runtime(RuntimeError::Protocol(
            "tag parse failed".to_owned(),
        )));

        assert!(matches!(
            error.as_core(),
            CoreError::Runtime(RuntimeError::Protocol(reason)) if reason == "tag parse failed"
        ));
    }
}
