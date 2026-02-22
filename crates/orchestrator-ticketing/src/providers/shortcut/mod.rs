use crate::interface::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, GetTicketRequest, TicketAttachment,
    TicketDetails, TicketId, TicketProvider, TicketQuery, TicketSummary, TicketingProvider,
    UpdateTicketDescriptionRequest, UpdateTicketStateRequest,
};
use async_trait::async_trait;
use reqwest::{header, Client};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::warn;

const DEFAULT_SHORTCUT_API_URL: &str = "https://api.app.shortcut.com/api/v3";
const DEFAULT_SHORTCUT_FETCH_LIMIT: u32 = 100;
const DEFAULT_SHORTCUT_REQUEST_TIMEOUT_SECS: u64 = 20;

#[derive(Clone, PartialEq, Eq)]
pub struct ShortcutConfig {
    pub api_url: String,
    pub api_key: String,
    pub fetch_limit: u32,
}

impl fmt::Debug for ShortcutConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShortcutConfig")
            .field("api_url", &self.api_url)
            .field("api_key", &"<redacted>")
            .field("fetch_limit", &self.fetch_limit)
            .finish()
    }
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
    pub fn from_settings(
        api_key: impl Into<String>,
        api_url: impl Into<String>,
        fetch_limit: u32,
    ) -> Result<Self, CoreError> {
        let api_key = api_key.into().trim().to_owned();
        if api_key.is_empty() {
            return Err(CoreError::Configuration(
                "ORCHESTRATOR_SHORTCUT_API_KEY is empty. Provide a non-empty API key.".to_owned(),
            ));
        }

        let api_url = api_url.into().trim().to_owned();
        let api_url = if api_url.is_empty() {
            DEFAULT_SHORTCUT_API_URL.to_owned()
        } else {
            api_url
        };
        if fetch_limit == 0 {
            return Err(CoreError::Configuration(
                "ORCHESTRATOR_SHORTCUT_FETCH_LIMIT must be greater than zero.".to_owned(),
            ));
        }

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
    member_id_cache: Arc<RwLock<Option<String>>>,
}

impl ShortcutTicketingProvider {
    pub fn new(mut config: ShortcutConfig) -> Result<Self, CoreError> {
        if config.api_key.trim().is_empty() {
            return Err(CoreError::Configuration(
                "ORCHESTRATOR_SHORTCUT_API_KEY is empty. Provide a non-empty API key.".to_owned(),
            ));
        }
        if config.fetch_limit == 0 {
            return Err(CoreError::Configuration(
                "ORCHESTRATOR_SHORTCUT_FETCH_LIMIT must be greater than zero.".to_owned(),
            ));
        }
        if config.api_url.trim().is_empty() {
            config.api_url = DEFAULT_SHORTCUT_API_URL.to_owned();
        }

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

        Ok(Self {
            config,
            client,
            member_id_cache: Arc::new(RwLock::new(None)),
        })
    }

    pub fn scaffold_default() -> Self {
        let mut config = ShortcutConfig::default();
        if config.api_key.trim().is_empty() {
            config.api_key = "scaffold-token".to_owned();
        }
        Self::new(config).expect("construct default shortcut provider")
    }

    pub fn config(&self) -> &ShortcutConfig {
        &self.config
    }

    fn endpoint(&self, path: &str) -> String {
        let base = self.config.api_url.trim_end_matches('/');
        let suffix = path.trim_start_matches('/');
        format!("{base}/{suffix}")
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, CoreError> {
        let response = request.send().await.map_err(|error| {
            CoreError::DependencyUnavailable(format!("Shortcut API request failed: {error}"))
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
            CoreError::DependencyUnavailable(format!("Shortcut API request failed: {error}"))
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

    async fn fetch_stories(
        &self,
        query_limit: Option<u32>,
    ) -> Result<Vec<ShortcutStory>, CoreError> {
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

    async fn fetch_projects(&self) -> Result<Vec<ShortcutProjectRecord>, CoreError> {
        let request = self
            .client
            .get(self.endpoint("projects"))
            .query(&[("page_size", self.config.fetch_limit.to_string())]);
        let payload = self.request_json::<Value>(request).await?;
        extract_list(payload, "projects")
    }

    async fn resolve_project_id(&self, project_name: &str) -> Result<String, CoreError> {
        let target = project_name.trim();
        if target.is_empty() {
            return Err(CoreError::Configuration(
                "Shortcut project name cannot be empty.".to_owned(),
            ));
        }

        let mut matches = self
            .fetch_projects()
            .await?
            .into_iter()
            .filter(|project| project.name.eq_ignore_ascii_case(target))
            .collect::<Vec<_>>();

        if matches.is_empty() {
            return Err(CoreError::Configuration(format!(
                "Shortcut project `{target}` was not found."
            )));
        }
        if matches.len() > 1 {
            return Err(CoreError::Configuration(format!(
                "Shortcut project `{target}` is ambiguous; multiple projects share that name."
            )));
        }
        Ok(matches.pop().expect("single project match").id)
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

    async fn add_comment_request(&self, request: AddTicketCommentRequest) -> Result<(), CoreError> {
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

    async fn resolve_api_key_member_id(&self) -> Result<String, CoreError> {
        {
            let cache = self
                .member_id_cache
                .read()
                .expect("shortcut member cache read lock");
            if let Some(member_id) = cache
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Ok(member_id.to_owned());
            }
        }

        let request = self.client.get(self.endpoint("member"));
        let payload = self.request_json::<Value>(request).await?;
        let member_id = parse_shortcut_member_id(&payload).ok_or_else(|| {
            CoreError::DependencyUnavailable(
                "Shortcut member lookup returned no member id.".to_owned(),
            )
        })?;

        {
            let mut cache = self
                .member_id_cache
                .write()
                .expect("shortcut member cache write lock");
            *cache = Some(member_id.clone());
        }

        Ok(member_id)
    }
}

#[async_trait]
impl TicketingProvider for ShortcutTicketingProvider {
    fn provider(&self) -> TicketProvider {
        TicketProvider::Shortcut
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        let request = self
            .client
            .get(self.endpoint("workflow-states"))
            .query(&[("page_size", "1")]);
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
            let member_id = self.resolve_api_key_member_id().await?;
            stories.retain(|story| story_is_assigned_to_member(story, member_id.as_str()));
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
        Ok(stories.into_iter().map(ticket_summary_from_story).collect())
    }

    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        let projects = self
            .fetch_projects()
            .await?
            .into_iter()
            .map(|project| project.name)
            .filter(|name| !name.trim().is_empty())
            .collect::<Vec<_>>();

        let mut deduped_projects = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for project in projects {
            let project = project.trim().to_owned();
            if project.is_empty() {
                continue;
            }
            let normalized = project.to_ascii_lowercase();
            if !seen.insert(normalized) {
                continue;
            }
            deduped_projects.push(project);
        }

        deduped_projects
            .sort_by(|left, right| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()));
        Ok(deduped_projects)
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
        if let Some(project) = request.project.as_deref() {
            let project = project.trim();
            if !project.is_empty() && !project.eq_ignore_ascii_case("No Project") {
                let project_id = self.resolve_project_id(project).await?;
                payload["project_id"] = json!(project_id);
            }
        }
        if request.assign_to_api_key_user {
            match self.resolve_api_key_member_id().await {
                Ok(member_id) => {
                    payload["owner_ids"] = json!([member_id]);
                }
                Err(error) => {
                    warn!(error = %error, "Shortcut member lookup failed; creating ticket unassigned");
                }
            }
        }

        let response = self.client.post(self.endpoint("stories")).json(&payload);
        let created = parse_shortcut_story(self.request_json(response).await?)?;
        Ok(ticket_summary_from_story(created))
    }

    async fn get_ticket(&self, request: GetTicketRequest) -> Result<TicketDetails, CoreError> {
        let issue_id = shortcut_provider_ticket_id(&request.ticket_id)?;
        let response = self
            .client
            .get(self.endpoint(&format!("stories/{issue_id}")));
        let story = parse_shortcut_story(self.request_json(response).await?)?;
        Ok(TicketDetails {
            summary: ticket_summary_from_story(story.clone()),
            description: story.description,
        })
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

    async fn update_ticket_description(
        &self,
        request: UpdateTicketDescriptionRequest,
    ) -> Result<(), CoreError> {
        let issue_id = shortcut_provider_ticket_id(&request.ticket_id)?;
        let description = request.description.trim();
        if description.is_empty() {
            return Err(CoreError::Configuration(
                "Shortcut ticket description updates require non-empty text.".to_owned(),
            ));
        }
        let request = self
            .client
            .put(self.endpoint(&format!("stories/{issue_id}")))
            .json(&json!({ "description": description }));
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
            .map(|raw| {
                serde_json::from_value(raw.clone()).map_err(|error| {
                    CoreError::DependencyUnavailable(format!(
                        "Shortcut payload decode failed: {error}"
                    ))
                })
            })
            .collect();
    }

    if let Ok(stories) = serde_json::from_value::<Vec<T>>(payload.clone()) {
        return Ok(stories);
    }

    if let Some(values) = payload.get("data").and_then(Value::as_array) {
        return values
            .iter()
            .map(|raw| {
                serde_json::from_value(raw.clone()).map_err(|error| {
                    CoreError::DependencyUnavailable(format!(
                        "Shortcut payload decode failed: {error}"
                    ))
                })
            })
            .collect();
    }

    if let Some(values) = payload.get("items").and_then(Value::as_array) {
        return values
            .iter()
            .map(|raw| {
                serde_json::from_value(raw.clone()).map_err(|error| {
                    CoreError::DependencyUnavailable(format!(
                        "Shortcut payload decode failed: {error}"
                    ))
                })
            })
            .collect();
    }

    Err(CoreError::DependencyUnavailable(format!(
        "Shortcut response does not contain a list for '{key}'."
    )))
}

fn parse_shortcut_story(payload: Value) -> Result<ShortcutStory, CoreError> {
    if let Some(story) = payload.get("story") {
        serde_json::from_value::<ShortcutStory>(story.clone()).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Shortcut story payload decode failed: {error}"
            ))
        })
    } else if let Some(story) = payload.get("data").and_then(|value| value.get("story")) {
        serde_json::from_value::<ShortcutStory>(story.clone()).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Shortcut story payload decode failed: {error}"
            ))
        })
    } else {
        serde_json::from_value::<ShortcutStory>(payload).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Shortcut response does not contain a story payload: {error}"
            ))
        })
    }
}

fn parse_shortcut_member_id(payload: &Value) -> Option<String> {
    payload
        .get("id")
        .and_then(json_value_to_non_empty_string)
        .or_else(|| {
            payload
                .get("member")
                .and_then(|value| value.get("id"))
                .and_then(json_value_to_non_empty_string)
        })
        .or_else(|| {
            payload
                .get("data")
                .and_then(|value| value.get("id"))
                .and_then(json_value_to_non_empty_string)
        })
        .or_else(|| {
            payload
                .get("data")
                .and_then(|value| value.get("member"))
                .and_then(|value| value.get("id"))
                .and_then(json_value_to_non_empty_string)
        })
}

fn json_value_to_non_empty_string(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => {
            let normalized = raw.trim();
            if normalized.is_empty() {
                None
            } else {
                Some(normalized.to_owned())
            }
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn story_is_assigned_to_member(story: &ShortcutStory, member_id: &str) -> bool {
    let member_id = member_id.trim();
    if member_id.is_empty() {
        return false;
    }

    if story.owner_ids.iter().any(|owner_id| owner_id == member_id) {
        return true;
    }

    story
        .owner
        .as_ref()
        .and_then(|owner| owner.id.as_deref())
        .map(|owner_id| owner_id == member_id)
        .unwrap_or(false)
}

fn deserialize_optional_stringish<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|entry| json_value_to_non_empty_string(&entry)))
}

fn deserialize_vec_stringish<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|entry| json_value_to_non_empty_string(&entry))
        .collect())
}

fn deserialize_required_stringish<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    json_value_to_non_empty_string(&value)
        .ok_or_else(|| serde::de::Error::custom("expected non-empty string or numeric id"))
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
        title: story.name.as_deref().unwrap_or("Untitled").to_owned(),
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
        updated_at: story
            .updated_at
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutStory {
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "app_url")]
    pub app_url: Option<String>,
    #[serde(default, alias = "updated_at")]
    pub updated_at: Option<String>,
    #[serde(
        default,
        alias = "short_id",
        deserialize_with = "deserialize_optional_stringish"
    )]
    pub short_id: Option<String>,
    #[serde(rename = "workflowState", alias = "workflow_state", default)]
    pub workflow_state: Option<ShortcutWorkflowState>,
    #[serde(default)]
    pub project: Option<ShortcutProject>,
    #[serde(default)]
    pub labels: Vec<ShortcutLabel>,
    #[serde(default)]
    pub owner: Option<ShortcutOwner>,
    #[serde(
        default,
        alias = "owner_ids",
        deserialize_with = "deserialize_vec_stringish"
    )]
    pub owner_ids: Vec<String>,
    #[serde(default)]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutProject {
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutProjectRecord {
    #[serde(deserialize_with = "deserialize_required_stringish")]
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutOwner {
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    pub id: Option<String>,
    pub name: Option<String>,
    #[serde(default, alias = "profile_url")]
    pub profile_url: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutLabel {
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShortcutWorkflowState {
    #[serde(deserialize_with = "deserialize_required_stringish")]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{TicketingProvider, TicketingProviderKind};

    #[test]
    fn scaffold_default_uses_shortcut_kind() {
        let provider = ShortcutTicketingProvider::scaffold_default();
        assert_eq!(provider.provider(), TicketProvider::Shortcut);
        assert_eq!(provider.kind(), TicketingProviderKind::Shortcut);
        assert_eq!(provider.provider_key(), "ticketing.shortcut");
    }

    #[test]
    fn parse_shortcut_member_id_reads_top_level_id() {
        let payload = json!({ "id": "member-1" });
        assert_eq!(
            parse_shortcut_member_id(&payload),
            Some("member-1".to_owned())
        );
    }

    #[test]
    fn parse_shortcut_member_id_reads_nested_member_id() {
        let payload = json!({ "member": { "id": "member-2" } });
        assert_eq!(
            parse_shortcut_member_id(&payload),
            Some("member-2".to_owned())
        );
    }

    #[test]
    fn parse_shortcut_member_id_reads_data_member_id_and_numeric_values() {
        let payload = json!({ "data": { "member": { "id": 42 } } });
        assert_eq!(parse_shortcut_member_id(&payload), Some("42".to_owned()));
    }

    #[test]
    fn parse_shortcut_member_id_returns_none_for_missing_id() {
        let payload = json!({ "member": { "name": "Austin" } });
        assert_eq!(parse_shortcut_member_id(&payload), None);
    }

    #[test]
    fn parse_shortcut_story_accepts_wrapped_payload_and_stringifies_ids() {
        let payload = json!({
            "story": {
                "id": 123,
                "short_id": 456,
                "name": "Fix filter behavior",
                "app_url": "https://app.shortcut.com/example/story/456",
                "updated_at": "2026-02-22T12:34:56Z",
                "owner_ids": [7]
            }
        });

        let story = parse_shortcut_story(payload).expect("parse wrapped story payload");
        assert_eq!(story.id.as_deref(), Some("123"));
        assert_eq!(story.short_id.as_deref(), Some("456"));
        assert_eq!(
            story.app_url.as_deref(),
            Some("https://app.shortcut.com/example/story/456")
        );
        assert_eq!(story.owner_ids, vec!["7".to_owned()]);
    }

    #[test]
    fn story_assignment_matching_uses_owner_ids_and_owner_fallback() {
        let mut story = ShortcutStory {
            id: Some("1".to_owned()),
            description: None,
            name: Some("Story".to_owned()),
            app_url: None,
            updated_at: None,
            short_id: Some("SC-1".to_owned()),
            workflow_state: None,
            project: None,
            labels: Vec::new(),
            owner: Some(ShortcutOwner {
                id: Some("member-2".to_owned()),
                name: None,
                profile_url: None,
            }),
            owner_ids: vec!["member-1".to_owned()],
            priority: None,
        };

        assert!(story_is_assigned_to_member(&story, "member-1"));
        assert!(story_is_assigned_to_member(&story, "member-2"));
        assert!(!story_is_assigned_to_member(&story, "member-3"));

        story.owner_ids.clear();
        assert!(story_is_assigned_to_member(&story, "member-2"));
    }

    #[test]
    fn shortcut_config_debug_redacts_api_key() {
        let config = ShortcutConfig {
            api_url: "https://api.app.shortcut.com/api/v3".to_owned(),
            api_key: "super-secret-token".to_owned(),
            fetch_limit: 100,
        };

        let debug = format!("{config:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("super-secret-token"));
    }
}
