use std::sync::Arc;

use async_trait::async_trait;
use orchestrator_core::{
    ArchiveTicketRequest, CoreError, CreateTicketRequest, GithubClient, LlmChatRequest,
    LlmMessage, LlmProvider, LlmRole, ProjectionState, SelectedTicketFlowResult, Supervisor,
    TicketQuery, TicketSummary, TicketingProvider, VcsProvider, WorkerBackend, WorkerSessionId,
};
use orchestrator_ui::{
    CreateTicketFromPickerRequest, SessionWorktreeDiff, SessionWorkflowAdvanceOutcome,
    TicketPickerProvider,
};
use std::path::PathBuf;

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
    S: Supervisor + LlmProvider + Send + Sync + 'static,
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

    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        self.ticketing.list_projects().await
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
        .map_err(|error| CoreError::Configuration(format!("ticket picker task failed: {error}")))?
    }

    async fn reload_projection(&self) -> Result<ProjectionState, CoreError> {
        self.app.startup_state().await.map(|state| state.projection)
    }

    async fn create_ticket_from_brief(
        &self,
        request: CreateTicketFromPickerRequest,
    ) -> Result<TicketSummary, CoreError> {
        let brief = request.brief.trim();
        if brief.is_empty() {
            return Err(CoreError::InvalidCommandArgs {
                command_id: "ui.ticket_picker.create".to_owned(),
                reason: "ticket brief cannot be empty".to_owned(),
            });
        }

        let (title, description) = draft_ticket_from_brief(&self.app.supervisor, brief).await?;
        let project = request
            .selected_project
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("No Project"));
        let ticket = self
            .ticketing
            .create_ticket(CreateTicketRequest {
                title,
                description: Some(description),
                project,
                state: None,
                priority: None,
                labels: Vec::new(),
            })
            .await?;

        Ok(ticket)
    }

    async fn archive_ticket(&self, ticket: TicketSummary) -> Result<(), CoreError> {
        self.ticketing
            .archive_ticket(ArchiveTicketRequest {
                ticket_id: ticket.ticket_id,
            })
            .await
    }

    async fn archive_session(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<Option<String>, CoreError> {
        let app = self.app.clone();
        let worker_backend = self.worker_backend.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    CoreError::Configuration(format!(
                        "failed to initialize session-archive runtime: {error}"
                    ))
                })?;

            runtime.block_on(async {
                app.archive_session(&session_id, worker_backend.as_ref())
                    .await
            })
        })
        .await
        .map_err(|error| {
            CoreError::Configuration(format!(
                "ticket picker task failed while archiving session: {error}"
            ))
        })?
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

    async fn session_worktree_diff(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<SessionWorktreeDiff, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.session_worktree_diff(&session_id))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while loading session diff: {error}"
                ))
            })?
    }

    async fn advance_session_workflow(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<SessionWorkflowAdvanceOutcome, CoreError> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.advance_session_workflow(&session_id))
            .await
            .map_err(|error| {
                CoreError::Configuration(format!(
                    "ticket picker task failed while advancing session workflow: {error}"
                ))
            })?
    }

    async fn complete_session_after_merge(
        &self,
        session_id: WorkerSessionId,
    ) -> Result<(), CoreError> {
        let app = self.app.clone();
        let worker_backend = self.worker_backend.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    CoreError::Configuration(format!(
                        "failed to initialize merge-finalization runtime: {error}"
                    ))
                })?;

            runtime.block_on(async {
                app.complete_session_after_merge(&session_id, worker_backend.as_ref())
                    .await
            })
        })
        .await
        .map_err(|error| {
            CoreError::Configuration(format!(
                "ticket picker task failed while finalizing merged session: {error}"
            ))
        })?
    }
}

const DEFAULT_SUPERVISOR_MODEL: &str = "openai/gpt-4o-mini";
const MAX_GENERATED_TITLE_LEN: usize = 180;

async fn draft_ticket_from_brief(
    supervisor: &dyn LlmProvider,
    brief: &str,
) -> Result<(String, String), CoreError> {
    let (_stream_id, mut stream) = supervisor
        .stream_chat(LlmChatRequest {
            model: supervisor_model_from_env(),
            tools: Vec::new(),
            messages: vec![
                LlmMessage {
                    role: LlmRole::System,
                    content: concat!(
                        "You draft software engineering tickets.\n",
                        "Return plain text with exactly this shape:\n",
                        "Title: <one concise line>\n",
                        "Description:\n",
                        "<multi-line markdown description with sections for Summary, Scope, and Acceptance Criteria>\n",
                        "Do not include code fences."
                    )
                    .to_owned(),
                    name: None,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
                LlmMessage {
                    role: LlmRole::User,
                    content: format!("Brief:\n{brief}"),
                    name: None,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            ],
            temperature: Some(0.2),
            tool_choice: None,
            max_output_tokens: Some(700),
        })
        .await?;

    let mut draft = String::new();
    while let Some(chunk) = stream.next_chunk().await? {
        if !chunk.delta.is_empty() {
            draft.push_str(chunk.delta.as_str());
        }
        if chunk.finish_reason.is_some() {
            break;
        }
    }

    parse_ticket_draft(draft.as_str(), brief)
}

fn parse_ticket_draft(draft: &str, fallback_brief: &str) -> Result<(String, String), CoreError> {
    let mut title = String::new();
    let mut description_lines = Vec::new();
    let mut in_description = false;

    for raw_line in draft.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() && !in_description {
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("title:") && title.is_empty() {
            title = trimmed[6..].trim().to_owned();
            continue;
        }

        if lower.starts_with("description:") {
            in_description = true;
            let first = trimmed[12..].trim();
            if !first.is_empty() {
                description_lines.push(first.to_owned());
            }
            continue;
        }

        if in_description {
            description_lines.push(line.to_owned());
        } else if title.is_empty() {
            title = trimmed.to_owned();
        }
    }

    let title = title.trim();
    if title.is_empty() {
        return Err(CoreError::DependencyUnavailable(
            "supervisor ticket draft did not include a title".to_owned(),
        ));
    }
    let title = truncate_to_char_boundary(title, MAX_GENERATED_TITLE_LEN).to_owned();

    let description = description_lines.join("\n").trim().to_owned();
    let description = if description.is_empty() {
        fallback_brief.trim().to_owned()
    } else {
        description
    };

    Ok((title, description))
}

fn truncate_to_char_boundary(value: &str, max_chars: usize) -> &str {
    if value.chars().count() <= max_chars {
        return value;
    }

    let mut end = value.len();
    for (count, (idx, _)) in value.char_indices().enumerate() {
        if count == max_chars {
            end = idx;
            break;
        }
    }
    &value[..end]
}

fn supervisor_model_from_env() -> String {
    std::env::var("ORCHESTRATOR_SUPERVISOR_MODEL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SUPERVISOR_MODEL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ticket_draft_extracts_title_and_description_sections() {
        let draft = "\
Title: Add create ticket shortcut in picker
Description:
## Summary
Allow creating tickets with n.

## Scope
- add key handling
- create and start
";

        let (title, description) =
            parse_ticket_draft(draft, "fallback brief").expect("parse ticket draft");
        assert_eq!(title, "Add create ticket shortcut in picker");
        assert!(description.contains("## Summary"));
        assert!(description.contains("## Scope"));
    }

    #[test]
    fn parse_ticket_draft_uses_fallback_when_description_missing() {
        let (title, description) =
            parse_ticket_draft("Title: New work item", "fallback brief text")
                .expect("parse ticket draft");
        assert_eq!(title, "New work item");
        assert_eq!(description, "fallback brief text");
    }

    #[test]
    fn parse_ticket_draft_rejects_missing_title() {
        let error = parse_ticket_draft("Description:\nNo title", "fallback")
            .expect_err("missing title should fail");
        assert!(error.to_string().contains("did not include a title"));
    }
}

fn is_unfinished_ticket_state(state: &str) -> bool {
    let normalized = state.trim().to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "done" | "completed" | "canceled" | "cancelled"
    )
}
