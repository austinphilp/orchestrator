use orchestrator_core::{CoreError, GithubClient, OrchestrationState, Supervisor};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub workspace: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            workspace: "./".to_owned(),
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Result<Self, CoreError> {
        match std::env::var("ORCHESTRATOR_CONFIG") {
            Ok(raw) => toml::from_str(&raw).map_err(|err| {
                CoreError::Configuration(format!("Failed to parse ORCHESTRATOR_CONFIG: {err}"))
            }),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => Err(CoreError::Configuration(
                "ORCHESTRATOR_CONFIG contained invalid UTF-8".to_owned(),
            )),
        }
    }
}

pub struct App<S: Supervisor, G: GithubClient> {
    pub config: AppConfig,
    pub supervisor: S,
    pub github: G,
}

impl<S: Supervisor, G: GithubClient> App<S, G> {
    pub async fn startup_state(&self) -> Result<OrchestrationState, CoreError> {
        self.supervisor.health_check().await?;
        self.github.health_check().await?;
        Ok(OrchestrationState {
            status: format!("ready ({})", self.config.workspace),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Healthy;

    #[async_trait::async_trait]
    impl Supervisor for Healthy {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl GithubClient for Healthy {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[test]
    fn config_defaults_when_env_missing() {
        unsafe {
            std::env::remove_var("ORCHESTRATOR_CONFIG");
        }
        let config = AppConfig::from_env().expect("default config");
        assert_eq!(config, AppConfig::default());
    }

    #[test]
    fn config_parses_from_toml_env() {
        unsafe {
            std::env::set_var("ORCHESTRATOR_CONFIG", "workspace = '/tmp/work'");
        }
        let config = AppConfig::from_env().expect("parse config");
        assert_eq!(config.workspace, "/tmp/work");
    }

    #[tokio::test]
    async fn startup_composition_succeeds_with_mock_adapters() {
        let app = App {
            config: AppConfig::default(),
            supervisor: Healthy,
            github: Healthy,
        };

        let state = app.startup_state().await.expect("startup state");
        assert!(state.status.contains("ready"));
    }
}
