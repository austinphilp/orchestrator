use async_trait::async_trait;
use orchestrator_core::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, TicketAttachment, TicketId,
    TicketProvider, TicketQuery, TicketSummary, TicketingProvider, UpdateTicketStateRequest,
};
use reqwest::{header, Client};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::warn;

const DEFAULT_SHORTCUT_API_URL: &str = "https://api.app.shortcut.com/api/v3";
const DEFAULT_SHORTCUT_FETCH_LIMIT: u32 = 100;
const DEFAULT_SHORTCUT_REQUEST_TIMEOUT_SECS: u64 = 20;
const ENV_SHORTCUT_API_KEY: &str = "ORCHESTRATOR_SHORTCUT_API_KEY";
const ENV_SHORTCUT_API_URL: &str = "ORCHESTRATOR_SHORTCUT_API_URL";
const ENV_SHORTCUT_FETCH_LIMIT: &str = "ORCHESTRATOR_SHORTCUT_FETCH_LIMIT";

#[derive(Debug, Clone)]
pub struct ShortcutConfig {
    pub api_url: String,
    pub api_key: String,
    pub fetch_limit: u32,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_SHORTCUT_API_URL.to_owned(),
            api_key: String::new(),
            fetch_limit: DEFAULT_SHORTCUT_FETCH_LIMIT,
        }
    }
}

impl ShortcutConfig {
    pub fn from_env() -> Result<Self, CoreError> {
        let api_key = std::env::var(ENV_SHORTCUT_API_KEY).map_err(|_| {
            CoreError::Configuration(
                "ORCHESTRATOR_SHORTCUT_API_KEY is not set. Export a valid API key before using integration-shortcut."
                    .to_owned(),
            )
        })?;
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            return Err(CoreError::Configuration(
                "ORCHESTRATOR_SHORTCUT_API_KEY is empty. Provide a non-empty API key."
                    .to_owned(),
            ));
        }

        let api_url = std::env::var(ENV_SHORTCUT_API_URL)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_SHORTCUT_API_URL.to_owned());

        let fetch_limit = std::env::var(ENV_SHORTCUT_FETCH_LIMIT)
            .ok()
            .and_then(|raw| {
                let value = raw.trim();
                if value.is_empty() {
                    return None;
                }
                Some(value.to_owned())
            })
            .map(|raw| {
                let value = raw.parse::<u32>().map_err(|_| {
                    CoreError::Configuration(
                        "ORCHESTRATOR_SHORTCUT_FETCH_LIMIT must be a non-zero integer."
                            .to_owned(),
                    )
                })?;
                if value == 0 {
                    return Err(CoreError::Configuration(
                        "ORCHESTRATOR_SHORTCUT_FETCH_LIMIT must be greater than zero.".to_owned(),
                    ));
                }
                Ok(value)
            })
            .transpose()?
            .unwrap_or(DEFAULT_SHORTCUT_FETCH_LIMIT);

        Ok(Self {
            api_url,
            api_key,
            fetch_limit,
        })
    }
}

#[derive(Clone)]
pub struct ShortcutTicketingProvider {
    config: ShortcutConfig,
    client: Client,
}

impl ShortcutTicketingProvider {
    pub fn from_env() -> Result<Self, CoreError> {
        let config = ShortcutConfig::from_env()?;

        let mut headers = header::HeaderMap::new();
        let token = header::HeaderValue::from_str(&config.api_key).map_err(|error| {
            CoreError::Configuration(format!("ORCHESTRATOR_SHORTCUT_API_KEY is invalid: {error}"))
        })?;
        headers.insert("Shortcut-Token", token);
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_SHORTCUT_REQUEST_TIMEOUT_SECS))
            .default_headers(headers)
            .build()
            .map_err(|error| {
                CoreError::Configuration(format!("failed to build Shortcut HTTP client: {error}"))
            })?;

        Ok(Self { config, client })
    }

    fn endpoint(&self, path: &str) -> String {
        let base = self.config.api_url.trim_end_matches('/');
        let suffix = path.trim_start_matches('/');
        format!("{base}/{suffix}")
    }

    async fn request_json<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T, CoreError> {
        let response = request.send().await.map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Shortcut API request failed: {error}"
            ))
        })?;

        let status = response.status();
        let body = response.text().await.map_err(|error| {
            CoreError::DependencyUnavailable(format!("Shortcut API response read failed: {error}"))
        })?;

        if !status.is_success() {
            return Err(CoreError::DependencyUnavailable(format!(
                "Shortcut API request failed with status {status}: {body}"
            )));
        }

        serde_json::from_str(&body).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Shortcut API response was malformed JSON: {error}"
            ))
        })
    }

    async fn request_status_only(&self, request: reqwest::RequestBuilder) -> Result<(), CoreError> {
        let response = request.send().await.map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Shortcut API request failed: {error}"
            ))
        })?;

        let status = response.status();
        let body = response.text().await.map_err(|error| {
            CoreError::DependencyUnavailable(format!("Shortcut API response read failed: {error}"))
        })?;

        if status.is_success() {
            Ok(())
        } else {
            Err(CoreError::DependencyUnavailable(format!(
                "Shortcut API request failed with status {status}: {body}"
            )))
        }
    }

    async fn fetch_stories(&self, query_limit: Option<u32>) -> Result<Vec<ShortcutStory>, CoreError> {
        let limit = query_limit.unwrap_or(self.config.fetch_limit).max(1);
        let request = self
            .client
            .get(self.endpoint("stories"))
            .query(&[("page_size", limit.to_string())]);
        let payload = self.request_json::<Value>(request).await?;
        extract_list(payload, "stories")
    }

    async fn fetch_workflow_states(&self) -> Result<Vec<ShortcutWorkflowState>, CoreError> {
        let request = self.client.get(self.endpoint("workflow-states"));
        let payload = self.request_json::<Value>(request).await?;
        extract_list(payload, "workflowStates")
    }

    async fn resolve_workflow_state_id(&self, state_name: &str) -> Result<String, CoreError> {
        let states = self.fetch_workflow_states().await?;
        let target = state_name.trim();
        if target.is_empty() {
            return Err(CoreError::Configuration(
                "Shortcut workflow state name cannot be empty.".to_owned(),
            ));
        }

        let target = target.to_ascii_lowercase();
        let mut available = Vec::new();
        for state in &states {
            let state_label = normalize_workflow_state_name(state);
            available.push(state_label.clone());
            if state_label.to_ascii_lowercase() == target {
                return Ok(state.id.clone());
            }
        }

        Err(CoreError::Configuration(format!(
            "Shortcut workflow state '{target}' not found. Available states: {}",
            available.join(", ")
        )))
    }

    async fn add_comment_request(
        &self,
        request: AddTicketCommentRequest,
    ) -> Result<(), CoreError> {
        let story_id = shortcut_provider_ticket_id(&request.ticket_id)?;
        let body = compose_shortcut_comment_body(request.comment.as_str(), &request.attachments)?;
        let response = self
            .client
            .post(self.endpoint(&format!("stories/{story_id}/comments")))
            .json(&json!({
                "text": body,
            }));
        self.request_status_only(response).await
    }
}

#[async_trait]
impl TicketingProvider for ShortcutTicketingProvider {
    fn provider(&self) -> TicketProvider {
        TicketProvider::Shortcut
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        let request = self.client.get(self.endpoint("workflow-states")).query(&[("page_size", "1")]);
        let payload = self.request_json::<Value>(request).await?;
        let states = extract_list::<ShortcutWorkflowState>(payload, "workflowStates")?;
        if states.is_empty() {
            return Err(CoreError::DependencyUnavailable(
                "Shortcut health check succeeded but returned no workflow states.".to_owned(),
            ));
        }
        Ok(())
    }

    async fn list_tickets(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
        let limit = query.limit.unwrap_or(self.config.fetch_limit);
        let mut stories = self.fetch_stories(Some(limit)).await?;

        if query.assigned_to_me {
            warn!(
                "Shortcut integration currently does not support strict assigned-to-me filtering; returning all matching stories."
            );
        }

        if let Some(raw_search) = query.search {
            let search = raw_search.trim().to_ascii_lowercase();
            if !search.is_empty() {
                stories.retain(|story| {
                    let combined = format!(
                        "{} {} {}",
                        story_identifier(story),
                        story.name.as_deref().unwrap_or_default(),
                        normalize_story_state_name(story)
                    )
                    .to_ascii_lowercase();
                    combined.contains(&search)
                });
            }
        }

        if !query.states.is_empty() {
            let expected: Vec<String> = query
                .states
                .into_iter()
                .map(|state| state.trim().to_ascii_lowercase())
                .filter(|state| !state.is_empty())
                .collect();

            stories.retain(|story| {
                let normalized = normalize_story_state_name(story).to_ascii_lowercase();
                expected.iter().any(|state| normalized == *state)
            });
        }

        stories.sort_by(|left, right| {
            right
                .updated_at
                .as_deref()
                .unwrap_or("")
                .cmp(left.updated_at.as_deref().unwrap_or(""))
                .then_with(|| story_identifier(left).cmp(&story_identifier(right)))
        });

        stories.truncate(limit as usize);
        Ok(stories
            .into_iter()
            .map(ticket_summary_from_story)
            .collect())
    }

    async fn create_ticket(
        &self,
        request: CreateTicketRequest,
    ) -> Result<TicketSummary, CoreError> {
        let title = request.title.trim();
        if title.is_empty() {
            return Err(CoreError::Configuration(
                "Shortcut ticket title cannot be empty.".to_owned(),
            ));
        }

        let workflow_state_id = request
            .state
            .as_deref()
            .filter(|state| !state.trim().is_empty())
            .map(|state| self.resolve_workflow_state_id(state));
        let workflow_state_id = if let Some(future) = workflow_state_id {
            Some(future.await?)
        } else {
            None
        };

        let mut payload = json!({
            "name": title,
            "description": request.description.unwrap_or_default(),
            "labels": request.labels,
            "priority": request.priority.unwrap_or(0),
        });
        if let Some(state_id) = workflow_state_id {
            payload["workflowStateId"] = json!(state_id);
        }

        let response = self.client.post(self.endpoint("stories")).json(&payload);
        let created: ShortcutStory = self.request_json(response).await?;
        Ok(ticket_summary_from_story(created))
    }

    async fn update_ticket_state(
        &self,
        request: UpdateTicketStateRequest,
    ) -> Result<(), CoreError> {
        let issue_id = shortcut_provider_ticket_id(&request.ticket_id)?;
        let target_state = request.state.trim();
        if target_state.is_empty() {
            return Err(CoreError::Configuration(
                "Shortcut ticket state updates require a non-empty target state.".to_owned(),
            ));
        }

        let workflow_state_id = self.resolve_workflow_state_id(target_state).await?;
        let request = self
            .client
            .put(self.endpoint(&format!("stories/{issue_id}")))
            .json(&json!({ "workflowStateId": workflow_state_id }));
        self.request_status_only(request).await
    }

    async fn add_comment(&self, request: AddTicketCommentRequest) -> Result<(), CoreError> {
        self.add_comment_request(request).await
    }
}

fn extract_list<T: DeserializeOwned>(payload: Value, key: &str) -> Result<Vec<T>, CoreError> {
    if let Some(values) = payload.get(key).and_then(Value::as_array) {
        return values
            .iter()
            .map(|raw| serde_json::from_value(raw.clone()).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Shortcut payload decode failed: {error}"
                ))
            }))
            .collect();
    }

    if let Ok(stories) = serde_json::from_value::<Vec<T>>(payload.clone()) {
        return Ok(stories);
    }

    if let Some(values) = payload.get("data").and_then(Value::as_array) {
        return values
            .iter()
            .map(|raw| serde_json::from_value(raw.clone()).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Shortcut payload decode failed: {error}"
                ))
            }))
            .collect();
    }

    if let Some(values) = payload.get("items").and_then(Value::as_array) {
        return values
            .iter()
            .map(|raw| serde_json::from_value(raw.clone()).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Shortcut payload decode failed: {error}"
                ))
            }))
            .collect();
    }

    Err(CoreError::DependencyUnavailable(format!(
        "Shortcut response does not contain a list for '{key}'."
    )))
}

fn compose_shortcut_comment_body(
    comment: &str,
    attachments: &[TicketAttachment],
) -> Result<String, CoreError> {
    let mut body = comment.trim().to_owned();
    if !attachments.is_empty() {
        let mut lines = Vec::new();
        for attachment in attachments {
            let url = attachment.url.trim();
            if url.is_empty() {
                return Err(CoreError::Configuration(
                    "Shortcut attachment URL cannot be empty.".to_owned(),
                ));
            }
            let label = attachment.label.trim();
            if label.is_empty() {
                lines.push(format!("- {url}"));
            } else {
                lines.push(format!("- [{label}]({url})"));
            }
        }

        if !body.is_empty() {
            body.push('\n');
            body.push('\n');
        }
        body.push_str("Attachments:\n");
        body.push_str(&lines.join("\n"));
    }

    if body.trim().is_empty() {
        return Err(CoreError::Configuration(
            "Shortcut comments require non-empty content or at least one attachment.".to_owned(),
        ));
    }

    Ok(body)
}

fn shortcut_provider_ticket_id(ticket_id: &TicketId) -> Result<String, CoreError> {
    let raw = ticket_id.as_str();
    let (provider, provider_ticket_id) = raw.split_once(':').ok_or_else(|| {
        CoreError::Configuration(format!(
            "Ticket id '{raw}' is invalid; expected '<provider>:<id>'."
        ))
    })?;
    if provider != "shortcut" {
        return Err(CoreError::Configuration(format!(
            "Ticket id '{raw}' is not a Shortcut ticket id."
        )));
    }
    let id = provider_ticket_id.trim();
    if id.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Ticket id '{raw}' is invalid; empty Shortcut issue id."
        )));
    }
    Ok(id.to_owned())
}

fn normalize_workflow_state_name(state: &ShortcutWorkflowState) -> String {
    state.name.clone().unwrap_or_else(|| "Unknown".to_owned())
}

fn normalize_story_state_name(story: &ShortcutStory) -> String {
    story
        .workflow_state
        .as_ref()
        .map(normalize_workflow_state_name)
        .unwrap_or_else(|| "Unknown".to_owned())
}

fn story_identifier(story: &ShortcutStory) -> String {
    story
        .short_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| story.id.clone())
        .unwrap_or_else(|| "story".to_owned())
}

fn ticket_summary_from_story(story: ShortcutStory) -> TicketSummary {
    let identifier = story_identifier(&story);
    let provider_ticket_id = story
        .id
        .as_deref()
        .or(story.short_id.as_deref())
        .unwrap_or_else(|| identifier.as_str());
    TicketSummary {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Shortcut, provider_ticket_id),
        identifier,
        title: story
            .name
            .as_deref()
            .unwrap_or("Untitled")
            .to_owned(),
        project: story.project.as_ref().map(|project| project.name.clone()),
        state: normalize_story_state_name(&story),
        url: story.app_url.unwrap_or_else(|| "about:blank".to_owned()),
        assignee: story
            .owner
            .as_ref()
            .and_then(|owner| owner.name.clone().or_else(|| owner.profile_url.clone())),
        priority: story.priority,
        labels: story
            .labels
            .iter()
            .map(|label| label.name.clone())
            .collect(),
        updated_at: story.updated_at.unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutStory {
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub app_url: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub short_id: Option<String>,
    #[serde(rename = "workflowState", default)]
    pub workflow_state: Option<ShortcutWorkflowState>,
    #[serde(default)]
    pub project: Option<ShortcutProject>,
    #[serde(default)]
    pub labels: Vec<ShortcutLabel>,
    #[serde(default)]
    pub owner: Option<ShortcutOwner>,
    #[serde(default)]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutProject {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutOwner {
    pub name: Option<String>,
    #[serde(default)]
    pub profile_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutLabel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutWorkflowState {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}
