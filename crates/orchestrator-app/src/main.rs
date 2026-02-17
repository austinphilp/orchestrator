use anyhow::Result;
use backend_opencode::{OpenCodeBackend, OpenCodeBackendConfig};
use integration_git::{GitCliVcsProvider, ProcessCommandRunner as GitProcessCommandRunner};
use integration_linear::LinearTicketingProvider;
use orchestrator_app::{App, AppConfig, AppTicketPickerProvider};
use orchestrator_github::{GhCliClient, ProcessCommandRunner as GhProcessCommandRunner};
use orchestrator_supervisor::OpenRouterSupervisor;
use orchestrator_ui::Ui;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = AppConfig::from_env()?;
    let supervisor = OpenRouterSupervisor::from_env()?;
    let github = GhCliClient::new(GhProcessCommandRunner)?;

    let app = Arc::new(App {
        config,
        supervisor,
        github,
    });

    let ticketing = Arc::new(LinearTicketingProvider::from_env()?);
    let vcs = Arc::new(GitCliVcsProvider::new(GitProcessCommandRunner)?);
    let worker_backend = Arc::new(OpenCodeBackend::new(OpenCodeBackendConfig::default()));
    let ticket_picker_provider = Arc::new(AppTicketPickerProvider::new(
        Arc::clone(&app),
        ticketing,
        vcs,
        worker_backend,
    ));

    let state = app.startup_state().await?;
    let supervisor_dispatcher: Arc<dyn orchestrator_ui::SupervisorCommandDispatcher> = app.clone();
    let mut ui = Ui::init()?
        .with_supervisor_command_dispatcher(supervisor_dispatcher)
        .with_ticket_picker_provider(ticket_picker_provider);
    ui.run(&state.status, &state.projection)?;

    Ok(())
}
