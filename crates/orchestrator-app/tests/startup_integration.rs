use orchestrator_app::{App, AppConfig};
use orchestrator_core::test_support::TestDbPath;
use orchestrator_core::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, GithubClient, GetTicketRequest,
    Supervisor, TicketDetails, TicketProvider, TicketQuery, TicketSummary,
    TicketingProvider, UpdateTicketDescriptionRequest, UpdateTicketStateRequest,
};
use std::sync::Arc;

struct MockAdapter {
    pass: bool,
}

#[derive(Debug)]
struct MockTicketingProvider;

impl MockTicketingProvider {
    fn service() -> Arc<dyn TicketingProvider + Send + Sync> {
        Arc::new(Self)
    }
}

#[async_trait::async_trait]
impl TicketingProvider for MockTicketingProvider {
    fn provider(&self) -> TicketProvider {
        TicketProvider::Linear
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn list_tickets(&self, _query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
        Ok(Vec::new())
    }

    async fn create_ticket(
        &self,
        _request: CreateTicketRequest,
    ) -> Result<TicketSummary, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "mock ticketing provider create_ticket".to_owned(),
        ))
    }

    async fn update_ticket_state(
        &self,
        _request: UpdateTicketStateRequest,
    ) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "mock ticketing provider update_ticket_state".to_owned(),
        ))
    }

    async fn get_ticket(&self, _request: GetTicketRequest) -> Result<TicketDetails, CoreError> {
        Err(CoreError::DependencyUnavailable(
            "mock ticketing provider get_ticket".to_owned(),
        ))
    }

    async fn update_ticket_description(
        &self,
        _request: UpdateTicketDescriptionRequest,
    ) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(
            "mock ticketing provider update_ticket_description".to_owned(),
        ))
    }

    async fn add_comment(&self, _request: AddTicketCommentRequest) -> Result<(), CoreError> {
        Ok(())
    }
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
    let temp_db = TestDbPath::new("startup-integration");

    let app = App {
        config: AppConfig {
            workspace: "./".to_owned(),
            event_store_path: temp_db.path().to_string_lossy().to_string(),
            ticketing_provider: "linear".to_owned(),
            harness_provider: "codex".to_owned(),
        },
        ticketing: MockTicketingProvider::service(),
        supervisor: MockAdapter { pass: true },
        github: MockAdapter { pass: true },
    };

    let state = app.startup_state().await.expect("startup should succeed");
    assert!(state.status.starts_with("ready"));
    assert_eq!(state.projection.events.len(), 0);
}
