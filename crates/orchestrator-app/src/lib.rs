use orchestrator_core::{
    rebuild_projection, CoreError, EventStore, GithubClient, ProjectionState, SqliteEventStore,
    Supervisor,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub workspace: String,
    #[serde(default = "default_event_store_path")]
    pub event_store_path: String,
}

fn default_event_store_path() -> String {
    "./orchestrator-events.db".to_owned()
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
            event_store_path: default_event_store_path(),
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
    use orchestrator_core::test_support::{with_env_var, TestDbPath};

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
        with_env_var("ORCHESTRATOR_CONFIG", None, || {
            let config = AppConfig::from_env().expect("default config");
            assert_eq!(config, AppConfig::default());
        });
    }

    #[test]
    fn config_parses_from_toml_env() {
        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some("workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'"),
            || {
                let config = AppConfig::from_env().expect("parse config");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(config.event_store_path, "/tmp/events.db");
            },
        );
    }

    #[test]
    fn config_defaults_event_store_path_when_missing_in_toml_env() {
        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some("workspace = '/tmp/work'"),
            || {
                let config = AppConfig::from_env().expect("parse config");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(config.event_store_path, default_event_store_path());
            },
        );
    }

    #[tokio::test]
    async fn startup_composition_succeeds_with_mock_adapters() {
        let temp_db = TestDbPath::new("app-startup-test");

        let app = App {
            config: AppConfig {
                workspace: "./".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: Healthy,
            github: Healthy,
        };

        let state = app.startup_state().await.expect("startup state");
        assert!(state.status.contains("ready"));
        assert!(state.projection.events.is_empty());
    }
}
