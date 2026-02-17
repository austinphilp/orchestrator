use std::sync::Arc;

use async_trait::async_trait;
use orchestrator_core::{
    CoreError, GithubClient, ProjectionState, SelectedTicketFlowResult, Supervisor, TicketQuery,
    TicketSummary, TicketingProvider, VcsProvider, WorkerBackend,
};
use orchestrator_ui::TicketPickerProvider;

use crate::App;

pub struct AppTicketPickerProvider<S, G, T, V, W>
where
    S: Supervisor,
    G: GithubClient,
    T: TicketingProvider,
    V: VcsProvider,
    W: WorkerBackend,
{
    app: Arc<App<S, G>>,
    ticketing: Arc<T>,
    vcs: Arc<V>,
    worker_backend: Arc<W>,
}

impl<S, G, T, V, W> AppTicketPickerProvider<S, G, T, V, W>
where
    S: Supervisor,
    G: GithubClient,
    T: TicketingProvider,
    V: VcsProvider,
    W: WorkerBackend,
{
    pub fn new(
        app: Arc<App<S, G>>,
        ticketing: Arc<T>,
        vcs: Arc<V>,
        worker_backend: Arc<W>,
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
impl<S, G, T, V, W> TicketPickerProvider for AppTicketPickerProvider<S, G, T, V, W>
where
    S: Supervisor + Send + Sync,
    G: GithubClient + Send + Sync,
    T: TicketingProvider + Send + Sync,
    V: VcsProvider + Send + Sync,
    W: WorkerBackend + Send + Sync,
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
    ) -> Result<SelectedTicketFlowResult, CoreError> {
        self.app
            .start_or_resume_selected_ticket(
                &ticket,
                self.vcs.as_ref(),
                self.worker_backend.as_ref(),
            )
            .await
    }

    async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
        self.app.startup_state().await.map(|state| state.projection)
    }
}

fn is_unfinished_ticket_state(state: &str) -> bool {
    let normalized = state.trim().to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "done" | "completed" | "canceled" | "cancelled"
    )
}
