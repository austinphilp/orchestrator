use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use orchestrator_core::{
    AddTicketCommentRequest, ArchiveTicketRequest, CoreError, CreateTicketRequest,
    GetTicketRequest, TicketAttachment, TicketDetails, TicketId, TicketProvider, TicketQuery,
    TicketSummary, TicketingProvider, UpdateTicketDescriptionRequest, UpdateTicketStateRequest,
    WorkflowState,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::warn;

const DEFAULT_LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const DEFAULT_SYNC_INTERVAL_SECS: u64 = 60;
const DEFAULT_FETCH_LIMIT: u32 = 100;
const DEFAULT_WORKFLOW_COMMENT_SUMMARIES: bool = false;
const DEFAULT_WORKFLOW_ATTACH_PR_LINKS: bool = true;

const LIST_OPEN_ISSUES_QUERY: &str = r#"
query ListOpenIssues($first: Int!) {
  viewer {
    id
  }
  issues(
    first: $first
    filter: {
      archivedAt: { null: true }
      state: {
        type: {
          neq: "completed"
        }
      }
    }
  ) {
    nodes {
      id
      identifier
      title
      url
      priority
      updatedAt
      project {
        name
      }
      assignee {
        id
        name
      }
      state {
        name
      }
      labels {
        nodes {
          name
        }
      }
    }
  }
}
"#;

const VIEWER_HEALTH_QUERY: &str = r#"
query ViewerHealth {
  viewer {
    id
  }
}
"#;

const ISSUE_TEAM_STATES_QUERY: &str = r#"
query IssueTeamStates($id: String!) {
  issue(id: $id) {
    id
    team {
      states {
        nodes {
          id
          name
        }
      }
    }
  }
}
"#;

const UPDATE_ISSUE_STATE_MUTATION: &str = r#"
mutation UpdateIssueState($id: String!, $stateId: String!) {
  issueUpdate(id: $id, input: { stateId: $stateId }) {
    success
  }
}
"#;

const ISSUE_ARCHIVE_MUTATION: &str = r#"
mutation ArchiveIssue($id: String!) {
  issueArchive(id: $id) {
    success
  }
}
"#;

const COMMENT_CREATE_MUTATION: &str = r#"
mutation CommentCreate($issueId: String!, $body: String!) {
  commentCreate(input: { issueId: $issueId, body: $body }) {
    success
  }
}
"#;

const TEAMS_QUERY: &str = r#"
query Teams {
  teams {
    nodes {
      id
      key
      name
    }
  }
}
"#;

const TEAM_STATES_QUERY: &str = r#"
query TeamStates($id: String!) {
  team(id: $id) {
    states {
      nodes {
        id
        name
      }
    }
  }
}
"#;

const ISSUE_CREATE_MUTATION: &str = r#"
mutation CreateIssue($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue {
      id
      identifier
      title
      url
      priority
      updatedAt
      project {
        name
      }
      assignee {
        id
        name
      }
      state {
        name
      }
      labels {
        nodes {
          name
        }
      }
    }
  }
}
"#;

const ISSUE_DETAILS_QUERY: &str = r#"
query IssueDetails($id: String!) {
  issue(id: $id) {
    id
    identifier
    title
    description
    url
    priority
    updatedAt
    project {
      name
    }
    assignee {
      id
      name
    }
    state {
      name
    }
    labels {
      nodes {
        name
      }
    }
  }
}
"#;

const UPDATE_ISSUE_DESCRIPTION_MUTATION: &str = r#"
mutation UpdateIssueDescription($id: String!, $description: String!) {
  issueUpdate(id: $id, input: { description: $description }) {
    success
  }
}
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStateMapping {
    pub workflow_state: WorkflowState,
    pub linear_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearWorkflowSyncConfig {
    pub state_mappings: Vec<WorkflowStateMapping>,
    pub comment_summaries: bool,
    pub attach_pr_links: bool,
}

impl Default for LinearWorkflowSyncConfig {
    fn default() -> Self {
        Self {
            state_mappings: vec![
                WorkflowStateMapping {
                    workflow_state: WorkflowState::Implementing,
                    linear_state: "In Progress".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::Testing,
                    linear_state: "In Progress".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::PRDrafted,
                    linear_state: "In Review".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::AwaitingYourReview,
                    linear_state: "In Review".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::ReadyForReview,
                    linear_state: "In Review".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::InReview,
                    linear_state: "In Review".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::Done,
                    linear_state: "Done".to_owned(),
                },
                WorkflowStateMapping {
                    workflow_state: WorkflowState::Abandoned,
                    linear_state: "Canceled".to_owned(),
                },
            ],
            comment_summaries: DEFAULT_WORKFLOW_COMMENT_SUMMARIES,
            attach_pr_links: DEFAULT_WORKFLOW_ATTACH_PR_LINKS,
        }
    }
}

impl LinearWorkflowSyncConfig {
    fn linear_state_for(&self, workflow_state: &WorkflowState) -> Option<&str> {
        self.state_mappings
            .iter()
            .find(|entry| &entry.workflow_state == workflow_state)
            .map(|entry| entry.linear_state.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearWorkflowTransitionSyncRequest {
    pub ticket_id: TicketId,
    pub from: WorkflowState,
    pub to: WorkflowState,
    pub summary: Option<String>,
    pub pull_request_url: Option<String>,
}

#[derive(Clone)]
pub struct LinearConfig {
    pub api_url: String,
    pub api_key: String,
    pub poll_interval: Duration,
    pub sync_query: TicketQuery,
    pub fetch_limit: u32,
    pub workflow_sync: LinearWorkflowSyncConfig,
}

impl fmt::Debug for LinearConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinearConfig")
            .field("api_url", &self.api_url)
            .field("api_key", &"<redacted>")
            .field("poll_interval", &self.poll_interval)
            .field("sync_query", &self.sync_query)
            .field("fetch_limit", &self.fetch_limit)
            .field("workflow_sync", &self.workflow_sync)
            .finish()
    }
}

impl Default for LinearConfig {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_LINEAR_API_URL.to_owned(),
            api_key: String::new(),
            poll_interval: Duration::from_secs(DEFAULT_SYNC_INTERVAL_SECS),
            sync_query: TicketQuery {
                assigned_to_me: true,
                states: Vec::new(),
                search: None,
                limit: Some(DEFAULT_FETCH_LIMIT),
            },
            fetch_limit: DEFAULT_FETCH_LIMIT,
            workflow_sync: LinearWorkflowSyncConfig::default(),
        }
    }
}

impl LinearConfig {
    pub fn from_env() -> Result<Self, CoreError> {
        let api_key = std::env::var("LINEAR_API_KEY").map_err(|_| {
            CoreError::Configuration(
                "LINEAR_API_KEY is not set. Export a valid key before using integration-linear."
                    .to_owned(),
            )
        })?;
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(CoreError::Configuration(
                "LINEAR_API_KEY is empty. Provide a non-empty API key.".to_owned(),
            ));
        }

        let mut config = Self {
            api_key: api_key.to_owned(),
            ..Self::default()
        };
        if let Ok(api_url) = std::env::var("ORCHESTRATOR_LINEAR_API_URL") {
            let api_url = api_url.trim();
            if !api_url.is_empty() {
                config.api_url = api_url.to_owned();
            }
        }
        if let Ok(raw) = std::env::var("ORCHESTRATOR_LINEAR_SYNC_INTERVAL_SECS") {
            config.poll_interval = parse_sync_interval_secs(&raw)?;
        }
        if let Ok(raw) = std::env::var("ORCHESTRATOR_LINEAR_FETCH_LIMIT") {
            let value = raw.parse::<u32>().map_err(|_| {
                CoreError::Configuration(
                    "ORCHESTRATOR_LINEAR_FETCH_LIMIT must be an unsigned integer.".to_owned(),
                )
            })?;
            if value == 0 {
                return Err(CoreError::Configuration(
                    "ORCHESTRATOR_LINEAR_FETCH_LIMIT must be greater than zero.".to_owned(),
                ));
            }
            config.fetch_limit = value;
            config.sync_query.limit = Some(value);
        }
        if let Ok(raw) = std::env::var("ORCHESTRATOR_LINEAR_SYNC_ASSIGNED_TO_ME") {
            config.sync_query.assigned_to_me =
                parse_bool_env("ORCHESTRATOR_LINEAR_SYNC_ASSIGNED_TO_ME", &raw)?;
        }
        if let Ok(raw_states) = std::env::var("ORCHESTRATOR_LINEAR_SYNC_STATES") {
            config.sync_query.states = raw_states
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }
        if let Ok(raw_mapping) = std::env::var("ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP") {
            config.workflow_sync.state_mappings = parse_workflow_state_map_env(&raw_mapping)?;
        }
        if let Ok(raw) = std::env::var("ORCHESTRATOR_LINEAR_WORKFLOW_COMMENT_SUMMARIES") {
            config.workflow_sync.comment_summaries =
                parse_bool_env("ORCHESTRATOR_LINEAR_WORKFLOW_COMMENT_SUMMARIES", &raw)?;
        }
        if let Ok(raw) = std::env::var("ORCHESTRATOR_LINEAR_WORKFLOW_ATTACH_PR_LINKS") {
            config.workflow_sync.attach_pr_links =
                parse_bool_env("ORCHESTRATOR_LINEAR_WORKFLOW_ATTACH_PR_LINKS", &raw)?;
        }

        Ok(config)
    }
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool, CoreError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(CoreError::Configuration(format!(
            "{name} must be a boolean (true/false)."
        ))),
    }
}

fn parse_sync_interval_secs(value: &str) -> Result<Duration, CoreError> {
    let seconds = value.parse::<u64>().map_err(|_| {
        CoreError::Configuration(
            "ORCHESTRATOR_LINEAR_SYNC_INTERVAL_SECS must be an unsigned integer.".to_owned(),
        )
    })?;
    if seconds == 0 {
        return Err(CoreError::Configuration(
            "ORCHESTRATOR_LINEAR_SYNC_INTERVAL_SECS must be greater than zero.".to_owned(),
        ));
    }

    Ok(Duration::from_secs(seconds))
}

fn parse_workflow_state_map_env(value: &str) -> Result<Vec<WorkflowStateMapping>, CoreError> {
    let mut mappings = Vec::new();
    let mut seen = BTreeSet::new();
    for raw_pair in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let (raw_workflow_state, raw_linear_state) = raw_pair.split_once('=').ok_or_else(|| {
            CoreError::Configuration(
                "ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP entries must use `WorkflowState=LinearState` format."
                    .to_owned(),
            )
        })?;
        let workflow_state = parse_workflow_state_name(raw_workflow_state.trim())?;
        let linear_state = raw_linear_state.trim();
        if linear_state.is_empty() {
            return Err(CoreError::Configuration(
                "ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP target Linear state cannot be empty."
                    .to_owned(),
            ));
        }
        let state_key = workflow_state_key(&workflow_state);
        if !seen.insert(state_key) {
            return Err(CoreError::Configuration(format!(
                "ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP contains duplicate mapping for workflow state `{}`.",
                workflow_state_label(&workflow_state)
            )));
        }
        mappings.push(WorkflowStateMapping {
            workflow_state,
            linear_state: linear_state.to_owned(),
        });
    }

    if mappings.is_empty() {
        return Err(CoreError::Configuration(
            "ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP did not contain any valid mappings.".to_owned(),
        ));
    }

    Ok(mappings)
}

fn parse_workflow_state_name(value: &str) -> Result<WorkflowState, CoreError> {
    match normalize_workflow_state_name(value).as_str() {
        "new" => Ok(WorkflowState::New),
        "planning" => Ok(WorkflowState::Planning),
        "implementing" => Ok(WorkflowState::Implementing),
        "testing" => Ok(WorkflowState::Testing),
        "prdrafted" => Ok(WorkflowState::PRDrafted),
        "awaitingyourreview" => Ok(WorkflowState::AwaitingYourReview),
        "readyforreview" => Ok(WorkflowState::ReadyForReview),
        "inreview" => Ok(WorkflowState::InReview),
        "done" => Ok(WorkflowState::Done),
        "abandoned" => Ok(WorkflowState::Abandoned),
        _ => Err(CoreError::Configuration(format!(
            "Unknown workflow state `{value}` in ORCHESTRATOR_LINEAR_WORKFLOW_STATE_MAP."
        ))),
    }
}

fn normalize_workflow_state_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn workflow_state_key(value: &WorkflowState) -> String {
    match value {
        WorkflowState::New => "new",
        WorkflowState::Planning => "planning",
        WorkflowState::Implementing => "implementing",
        WorkflowState::Testing => "testing",
        WorkflowState::PRDrafted => "prdrafted",
        WorkflowState::AwaitingYourReview => "awaitingyourreview",
        WorkflowState::ReadyForReview => "readyforreview",
        WorkflowState::InReview => "inreview",
        WorkflowState::Done => "done",
        WorkflowState::Abandoned => "abandoned",
    }
    .to_owned()
}

#[derive(Debug, Clone)]
pub struct GraphqlRequest {
    pub query: String,
    pub variables: serde_json::Value,
}

impl GraphqlRequest {
    pub fn new(query: impl Into<String>, variables: serde_json::Value) -> Self {
        Self {
            query: query.into(),
            variables,
        }
    }
}

#[async_trait]
pub trait GraphqlTransport: Send + Sync {
    async fn execute(&self, request: GraphqlRequest) -> Result<serde_json::Value, CoreError>;
}

#[derive(Clone)]
pub struct ReqwestGraphqlTransport {
    endpoint: String,
    api_key: String,
    client: reqwest::Client,
}

impl fmt::Debug for ReqwestGraphqlTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestGraphqlTransport")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .field("client", &self.client)
            .finish()
    }
}

impl ReqwestGraphqlTransport {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Result<Self, CoreError> {
        let client = reqwest::Client::builder()
            .user_agent("orchestrator/integration-linear")
            .build()
            .map_err(|err| {
                CoreError::DependencyUnavailable(format!(
                    "failed to initialize Linear HTTP client: {err}"
                ))
            })?;

        Ok(Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            client,
        })
    }
}

#[async_trait]
impl GraphqlTransport for ReqwestGraphqlTransport {
    async fn execute(&self, request: GraphqlRequest) -> Result<serde_json::Value, CoreError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .json(&json!({
                "query": request.query,
                "variables": request.variables,
            }))
            .send()
            .await
            .map_err(|err| {
                CoreError::DependencyUnavailable(format!(
                    "failed to call Linear GraphQL API: {err}"
                ))
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|err| {
            CoreError::DependencyUnavailable(format!(
                "failed to read response from Linear GraphQL API: {err}"
            ))
        })?;

        if !status.is_success() {
            return Err(CoreError::DependencyUnavailable(format!(
                "Linear GraphQL API returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            )));
        }

        let envelope: GraphqlResponseEnvelope = serde_json::from_str(&body).map_err(|err| {
            CoreError::DependencyUnavailable(format!(
                "failed to parse Linear GraphQL response JSON: {err}"
            ))
        })?;

        if let Some(errors) = envelope.errors {
            let message = errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(CoreError::DependencyUnavailable(format!(
                "Linear GraphQL query failed: {message}"
            )));
        }

        envelope.data.ok_or_else(|| {
            CoreError::DependencyUnavailable(
                "Linear GraphQL response did not include a data payload.".to_owned(),
            )
        })
    }
}

fn truncate_for_error(body: &str) -> String {
    const MAX_LEN: usize = 200;
    if body.chars().count() <= MAX_LEN {
        body.to_owned()
    } else {
        format!("{}...", body.chars().take(MAX_LEN).collect::<String>())
    }
}
