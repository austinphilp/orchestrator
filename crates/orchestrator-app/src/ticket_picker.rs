use std::sync::Arc;

use async_trait::async_trait;
use std::path::PathBuf;
use orchestrator_core::{
    CoreError, GithubClient, ProjectionState, SelectedTicketFlowResult, Supervisor, TicketQuery,
    TicketSummary, TicketingProvider, VcsProvider, WorkerBackend, WorkerSessionId,
};
use orchestrator_ui::TicketPickerProvider;

use crate::App;

pub struct AppTicketPickerProvider<S, G, V>
where
    S: Supervisor + 'static,
    G: GithubClient + 'static,
    V: VcsProvider + 'static,
{
    app: Arc<App<S, G>>,
    ticketing: Arc<dyn TicketingProvider + Send + Sync>,
    vcs: Arc<V>,
    worker_backend: Arc<dyn WorkerBackend + Send + Sync>,
}

impl<S, G, V> AppTicketPickerProvider<S, G, V>
where
    S: Supervisor + 'static,
    G: GithubClient + 'static,
    V: VcsProvider + 'static,
{
    pub fn new(
        app: Arc<App<S, G>>,
        ticketing: Arc<dyn TicketingProvider + Send + Sync>,
        vcs: Arc<V>,
        worker_backend: Arc<dyn WorkerBackend + Send + Sync>,
    ) -> Self {
        Self {
            app,
            ticketing,
            vcs,
            worker_backend,
        }
    }
}

#[async_trait]
impl<S, G, V> TicketPickerProvider for AppTicketPickerProvider<S, G, V>
where
    S: Supervisor + Send + Sync + 'static,
    G: GithubClient + Send + Sync + 'static,
    V: VcsProvider + Send + Sync + 'static,
{
    async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
        let mut tickets = self
            .ticketing
            .list_tickets(TicketQuery {
                assigned_to_me: true,
                states: Vec::new(),
                search: None,
                limit: None,
            })
            .await?;

        tickets.retain(|ticket| is_unfinished_ticket_state(ticket.state.as_str()));

        Ok(tickets)
    }

    async fn start_or_resume_ticket(
        &self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) -> Result<SelectedTicketFlowResult, CoreError> {
        let app = self.app.clone();
        let ticket = ticket.clone();
        let vcs = self.vcs.clone();
        let worker_backend = self.worker_backend.clone();

        tokio::task::spawn_blocking(move || -> Result<SelectedTicketFlowResult, CoreError> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    CoreError::Configuration(format!(
                        "failed to initialize ticket picker runtime: {error}"
                    ))
                })?;

            runtime.block_on(async {
                app.start_or_resume_selected_ticket(
                    &ticket,
                    repository_override,
                    vcs.as_ref(),
                    worker_backend.as_ref(),
                )
                .await
            })
        })
        .await
        .map_err(|error| {
            CoreError::Configuration(format!(
                "ticket picker task failed: {error}"
            ))
        })?
    }

    async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
        self.app.startup_state().await.map(|state| state.projection)
    }

    async fn mark_session_crashed(
        &self,
        session_id: WorkerSessionId,
        reason: String,
    ) -> Result<(), CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.mark_session_crashed(&session_id, reason.as_str()))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while marking session crashed: {error}"
                ))
            })?
    }
}

fn is_unfinished_ticket_state(state: &str) -> bool {
    let normalized = state.trim().to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "done" | "completed" | "canceled" | "cancelled"
    )
}
