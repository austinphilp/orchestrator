use orchestrator_app::{App, AppConfig};
use orchestrator_core::{CoreError, GithubClient, Supervisor};

struct MockAdapter {
    pass: bool,
}

#[async_trait::async_trait]
impl Supervisor for MockAdapter {
    async fn health_check(&self) -> Result<(), CoreError> {
        if self.pass {
            Ok(())
        } else {
            Err(CoreError::DependencyUnavailable(
                "supervisor unavailable".to_owned(),
            ))
        }
    }
}

#[async_trait::async_trait]
impl GithubClient for MockAdapter {
    async fn health_check(&self) -> Result<(), CoreError> {
        if self.pass {
            Ok(())
        } else {
            Err(CoreError::DependencyUnavailable(
                "github unavailable".to_owned(),
            ))
        }
    }
}

#[tokio::test]
async fn startup_path_cleanly_initializes() {
    let temp_path = std::env::temp_dir().join("orchestrator-startup-integration.db");
    let _ = std::fs::remove_file(&temp_path);

    let app = App {
        config: AppConfig {
            workspace: "./".to_owned(),
            event_store_path: temp_path.to_string_lossy().to_string(),
        },
        supervisor: MockAdapter { pass: true },
        github: MockAdapter { pass: true },
    };

    let state = app.startup_state().await.expect("startup should succeed");
    assert!(state.status.starts_with("ready"));
    assert_eq!(state.projection.events.len(), 0);

    let _ = std::fs::remove_file(temp_path);
}
