use anyhow::Result;
use orchestrator_app::{App, AppConfig};
use orchestrator_github::{GhCliClient, ProcessCommandRunner};
use orchestrator_supervisor::OpenRouterSupervisor;
use orchestrator_ui::Ui;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = AppConfig::from_env()?;
    let supervisor = OpenRouterSupervisor::from_env()?;
    let github = GhCliClient::new(ProcessCommandRunner)?;

    let app = App {
        config,
        supervisor,
        github,
    };

    let state = app.startup_state().await?;
    let mut ui = Ui::init()?;
    ui.run(&state.status, &state.projection)?;

    Ok(())
}
