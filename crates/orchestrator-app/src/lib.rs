use orchestrator_core::{
    rebuild_projection, CoreError, EventStore, GithubClient, ProjectionState, SqliteEventStore,
    Supervisor,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub workspace: String,
    pub event_store_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupState {
    pub status: String,
    pub projection: ProjectionState,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            workspace: "./".to_owned(),
            event_store_path: "./orchestrator-events.db".to_owned(),
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
    pub async fn startup_state(&self) -> Result<StartupState, CoreError> {
        self.supervisor.health_check().await?;
        self.github.health_check().await?;

        let store = SqliteEventStore::open(&self.config.event_store_path)?;
        let events = store.read_ordered()?;
        let projection = rebuild_projection(&events);

        Ok(StartupState {
            status: format!("ready ({})", self.config.workspace),
            projection,
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
        let original = std::env::var("ORCHESTRATOR_CONFIG").ok();
        unsafe {
            std::env::remove_var("ORCHESTRATOR_CONFIG");
        }
        let config = AppConfig::from_env().expect("default config");
        assert_eq!(config, AppConfig::default());
        if let Some(value) = original {
            unsafe {
                std::env::set_var("ORCHESTRATOR_CONFIG", value);
            }
        }
    }

    #[test]
    fn config_parses_from_toml_env() {
        let original = std::env::var("ORCHESTRATOR_CONFIG").ok();
        unsafe {
            std::env::set_var(
                "ORCHESTRATOR_CONFIG",
                "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'",
            );
        }
        let config = AppConfig::from_env().expect("parse config");
        assert_eq!(config.workspace, "/tmp/work");
        assert_eq!(config.event_store_path, "/tmp/events.db");
        match original {
            Some(value) => unsafe {
                std::env::set_var("ORCHESTRATOR_CONFIG", value);
            },
            None => unsafe {
                std::env::remove_var("ORCHESTRATOR_CONFIG");
            },
        }
    }

    #[tokio::test]
    async fn startup_composition_succeeds_with_mock_adapters() {
        let temp_path = std::env::temp_dir().join("orchestrator-app-startup-test.db");
        let _ = std::fs::remove_file(&temp_path);

        let app = App {
            config: AppConfig {
                workspace: "./".to_owned(),
                event_store_path: temp_path.to_string_lossy().to_string(),
            },
            supervisor: Healthy,
            github: Healthy,
        };

        let state = app.startup_state().await.expect("startup state");
        assert!(state.status.contains("ready"));
        assert!(state.projection.events.is_empty());
        let _ = std::fs::remove_file(temp_path);
    }
}
