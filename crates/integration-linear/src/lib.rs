use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use orchestrator_core::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, TicketAttachment, TicketId,
    TicketProvider, TicketQuery, TicketSummary, TicketingProvider, UpdateTicketStateRequest,
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
      assignee {
        id
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

const COMMENT_CREATE_MUTATION: &str = r#"
mutation CommentCreate($issueId: String!, $body: String!) {
  commentCreate(input: { issueId: $issueId, body: $body }) {
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

pub struct LinearTicketingProvider {
    config: LinearConfig,
    transport: Arc<dyn GraphqlTransport>,
    cache: Arc<RwLock<TicketCache>>,
    poller: Mutex<Option<PollerState>>,
}

#[derive(Debug)]
struct PollerState {
    stop_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone)]
pub struct TicketCacheSnapshot {
    pub tickets: Vec<TicketSummary>,
    pub last_synced_at: Option<SystemTime>,
    pub last_sync_error: Option<String>,
}

#[derive(Debug, Default)]
struct TicketCache {
    tickets: BTreeMap<String, CachedTicket>,
    viewer_id: Option<String>,
    last_synced_at: Option<SystemTime>,
    last_sync_error: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedTicket {
    summary: TicketSummary,
    assignee_id: Option<String>,
}

impl LinearTicketingProvider {
    pub fn from_env() -> Result<Self, CoreError> {
        let config = LinearConfig::from_env()?;
        Self::new(config)
    }

    pub fn new(config: LinearConfig) -> Result<Self, CoreError> {
        let transport = ReqwestGraphqlTransport::new(&config.api_url, &config.api_key)?;
        Ok(Self::with_transport(config, Arc::new(transport)))
    }

    pub fn with_transport(config: LinearConfig, transport: Arc<dyn GraphqlTransport>) -> Self {
        Self {
            config,
            transport,
            cache: Arc::new(RwLock::new(TicketCache::default())),
            poller: Mutex::new(None),
        }
    }

    pub fn sync_query(&self) -> TicketQuery {
        self.config.sync_query.clone()
    }

    pub fn workflow_sync_config(&self) -> LinearWorkflowSyncConfig {
        self.config.workflow_sync.clone()
    }

    pub fn cache_snapshot(&self) -> TicketCacheSnapshot {
        let cache = self.cache.read().expect("linear ticket cache read lock");
        TicketCacheSnapshot {
            tickets: cache
                .tickets
                .values()
                .map(|item| item.summary.clone())
                .collect(),
            last_synced_at: cache.last_synced_at,
            last_sync_error: cache.last_sync_error.clone(),
        }
    }

    pub async fn sync_once(&self) -> Result<Vec<TicketSummary>, CoreError> {
        self.sync_with_query(self.sync_query()).await
    }

    pub async fn start_polling(&self) -> Result<(), CoreError> {
        {
            let guard = self.poller.lock().await;
            if guard.is_some() {
                return Ok(());
            }
        }

        self.sync_once().await?;

        let mut guard = self.poller.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        let cache = Arc::clone(&self.cache);
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.poll_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = interval.tick() => {
                        if let Err(error) = sync_with_query_with_error_tracking(
                            &transport,
                            &cache,
                            config.fetch_limit,
                            config.sync_query.clone(),
                        ).await {
                            warn!(error = %error, "linear polling sync failed");
                        }
                    }
                }
            }
        });

        *guard = Some(PollerState {
            stop_tx: Some(stop_tx),
            task,
        });
        Ok(())
    }

    pub async fn stop_polling(&self) -> Result<(), CoreError> {
        let state = {
            let mut guard = self.poller.lock().await;
            guard.take()
        };

        if let Some(mut state) = state {
            if let Some(stop_tx) = state.stop_tx.take() {
                let _ = stop_tx.send(());
            }
            state.task.await.map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "linear polling task join failed: {error}"
                ))
            })?;
        }

        Ok(())
    }

    async fn sync_with_query(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
        sync_with_query_with_error_tracking(
            &self.transport,
            &self.cache,
            self.config.fetch_limit,
            query,
        )
        .await
    }

    pub async fn sync_workflow_transition(
        &self,
        request: LinearWorkflowTransitionSyncRequest,
    ) -> Result<(), CoreError> {
        if let Some(linear_state) = self.config.workflow_sync.linear_state_for(&request.to) {
            self.update_ticket_state(UpdateTicketStateRequest {
                ticket_id: request.ticket_id.clone(),
                state: linear_state.to_owned(),
            })
            .await?;
        } else {
            warn!(
                ticket_id = %request.ticket_id.as_str(),
                target_workflow_state = workflow_state_label(&request.to),
                "no Linear workflow-state mapping configured; skipping Linear state update"
            );
        }

        let mut comment = String::new();
        if self.config.workflow_sync.comment_summaries {
            comment = request
                .summary
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "Workflow transitioned from `{}` to `{}`.",
                        workflow_state_label(&request.from),
                        workflow_state_label(&request.to)
                    )
                });
        }

        let mut attachments = Vec::new();
        if self.config.workflow_sync.attach_pr_links {
            if let Some(url) = request
                .pull_request_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
            {
                attachments.push(TicketAttachment {
                    label: "Pull Request".to_owned(),
                    url: url.to_owned(),
                });
            }
        }

        if !comment.trim().is_empty() || !attachments.is_empty() {
            self.add_comment(AddTicketCommentRequest {
                ticket_id: request.ticket_id,
                comment,
                attachments,
            })
            .await?;
        }

        Ok(())
    }
}

async fn sync_with_query_with_error_tracking(
    transport: &Arc<dyn GraphqlTransport>,
    cache: &Arc<RwLock<TicketCache>>,
    fetch_limit: u32,
    query: TicketQuery,
) -> Result<Vec<TicketSummary>, CoreError> {
    match sync_with_query_components(transport, cache, fetch_limit, query).await {
        Ok(summaries) => Ok(summaries),
        Err(error) => {
            let mut mutable_cache = cache.write().expect("linear ticket cache write lock");
            mutable_cache.last_sync_error = Some(error.to_string());
            Err(error)
        }
    }
}

async fn sync_with_query_components(
    transport: &Arc<dyn GraphqlTransport>,
    cache: &Arc<RwLock<TicketCache>>,
    fetch_limit: u32,
    query: TicketQuery,
) -> Result<Vec<TicketSummary>, CoreError> {
    let data = transport
        .execute(GraphqlRequest::new(
            LIST_OPEN_ISSUES_QUERY,
            json!({ "first": fetch_limit }),
        ))
        .await?;

    let payload: ListOpenIssuesData = serde_json::from_value(data).map_err(|error| {
        CoreError::DependencyUnavailable(format!("failed to decode Linear issues payload: {error}"))
    })?;

    let viewer_id = Some(payload.viewer.id.as_str());
    let filtered = payload
        .issues
        .nodes
        .into_iter()
        .filter(|issue| issue_matches_query(issue, &query, viewer_id))
        .collect::<Vec<_>>();

    let mut ordered = filtered;
    ordered.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.identifier.cmp(&right.identifier))
    });
    if let Some(limit) = query.limit {
        ordered.truncate(limit as usize);
    }

    let cache_entries = ordered
        .into_iter()
        .map(|issue| {
            let summary = issue_to_summary(&issue);
            (
                issue.id,
                CachedTicket {
                    summary,
                    assignee_id: issue.assignee.and_then(|assignee| assignee.id),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let summaries = cache_entries
        .values()
        .map(|entry| entry.summary.clone())
        .collect::<Vec<_>>();

    {
        let mut mutable_cache = cache.write().expect("linear ticket cache write lock");
        mutable_cache.tickets = cache_entries;
        mutable_cache.viewer_id = payload.viewer.id.into();
        mutable_cache.last_synced_at = Some(SystemTime::now());
        mutable_cache.last_sync_error = None;
    }

    Ok(summaries)
}

fn issue_matches_query(
    issue: &LinearIssueNode,
    query: &TicketQuery,
    viewer_id: Option<&str>,
) -> bool {
    if query.assigned_to_me {
        let matches_assignee = issue
            .assignee
            .as_ref()
            .and_then(|assignee| assignee.id.as_deref())
            .zip(viewer_id)
            .map(|(assignee_id, viewer_id)| assignee_id == viewer_id)
            .unwrap_or(false);
        if !matches_assignee {
            return false;
        }
    }

    if !query.states.is_empty() {
        let state_name = issue
            .state
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or_default();
        let state_matches = query
            .states
            .iter()
            .any(|target| target.eq_ignore_ascii_case(state_name));
        if !state_matches {
            return false;
        }
    }

    if let Some(search) = query.search.as_deref() {
        let needle = search.trim().to_ascii_lowercase();
        if !needle.is_empty() {
            let haystack = format!(
                "{} {} {}",
                issue.identifier,
                issue.title,
                issue
                    .state
                    .as_ref()
                    .map(|state| state.name.as_str())
                    .unwrap_or_default()
            )
            .to_ascii_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }
    }

    true
}

fn issue_to_summary(issue: &LinearIssueNode) -> TicketSummary {
    TicketSummary {
        ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, &issue.id),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        state: issue
            .state
            .as_ref()
            .map(|state| state.name.clone())
            .unwrap_or_else(|| "Unknown".to_owned()),
        url: issue.url.clone(),
        priority: issue.priority,
        labels: issue
            .labels
            .as_ref()
            .map(|labels| {
                labels
                    .nodes
                    .iter()
                    .map(|label| label.name.clone())
                    .collect()
            })
            .unwrap_or_default(),
        updated_at: issue.updated_at.clone(),
    }
}

async fn resolve_linear_state_id(
    transport: &Arc<dyn GraphqlTransport>,
    issue_id: &str,
    target_state: &str,
) -> Result<ResolvedLinearState, CoreError> {
    let data = transport
        .execute(GraphqlRequest::new(
            ISSUE_TEAM_STATES_QUERY,
            json!({ "id": issue_id }),
        ))
        .await?;
    let payload: IssueTeamStatesResponse = serde_json::from_value(data).map_err(|error| {
        CoreError::DependencyUnavailable(format!(
            "failed to decode Linear issue/team states payload: {error}"
        ))
    })?;
    let issue = payload.issue.ok_or_else(|| {
        CoreError::DependencyUnavailable(format!(
            "Linear issue lookup returned no issue for id `{issue_id}`."
        ))
    })?;
    let team = issue.team.ok_or_else(|| {
        CoreError::DependencyUnavailable(format!(
            "Linear issue `{issue_id}` does not include team state metadata."
        ))
    })?;

    let target_state = target_state.trim();
    let matched_state = team
        .states
        .nodes
        .iter()
        .find(|state| state.name.eq_ignore_ascii_case(target_state))
        .ok_or_else(|| {
            let available = team
                .states
                .nodes
                .iter()
                .map(|state| state.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            CoreError::Configuration(format!(
                "Linear issue `{issue_id}` has no state named `{target_state}`. Available states: {available}"
            ))
        })?;

    Ok(ResolvedLinearState {
        id: matched_state.id.clone(),
        name: matched_state.name.clone(),
    })
}

#[derive(Debug, Clone)]
struct ResolvedLinearState {
    id: String,
    name: String,
}

fn linear_provider_ticket_id(ticket_id: &TicketId) -> Result<&str, CoreError> {
    let raw = ticket_id.as_str();
    let (provider, provider_ticket_id) = raw.split_once(':').ok_or_else(|| {
        CoreError::Configuration(format!(
            "Ticket id `{raw}` is invalid; expected `linear:<issue-id>`."
        ))
    })?;

    if provider != "linear" {
        return Err(CoreError::Configuration(format!(
            "Ticket id `{raw}` is not a Linear ticket id."
        )));
    }
    let provider_ticket_id = provider_ticket_id.trim();
    if provider_ticket_id.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Ticket id `{raw}` is invalid; provider issue id is empty."
        )));
    }

    Ok(provider_ticket_id)
}

fn compose_linear_comment_body(
    comment: &str,
    attachments: &[TicketAttachment],
) -> Result<String, CoreError> {
    let mut body = comment.trim().to_owned();
    if !attachments.is_empty() {
        let mut attachment_lines = Vec::new();
        for attachment in attachments {
            let url = attachment.url.trim();
            if url.is_empty() {
                return Err(CoreError::Configuration(
                    "Linear comment attachment URLs cannot be empty.".to_owned(),
                ));
            }
            let label = attachment.label.trim();
            if label.is_empty() {
                attachment_lines.push(format!("- {url}"));
            } else {
                attachment_lines.push(format!("- [{label}]({url})"));
            }
        }
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("Attachments:\n");
        body.push_str(&attachment_lines.join("\n"));
    }

    if body.trim().is_empty() {
        return Err(CoreError::Configuration(
            "Linear comments require non-empty content or at least one attachment.".to_owned(),
        ));
    }

    Ok(body)
}

fn workflow_state_label(state: &WorkflowState) -> &'static str {
    match state {
        WorkflowState::New => "New",
        WorkflowState::Planning => "Planning",
        WorkflowState::Implementing => "Implementing",
        WorkflowState::Testing => "Testing",
        WorkflowState::PRDrafted => "PRDrafted",
        WorkflowState::AwaitingYourReview => "AwaitingYourReview",
        WorkflowState::ReadyForReview => "ReadyForReview",
        WorkflowState::InReview => "InReview",
        WorkflowState::Done => "Done",
        WorkflowState::Abandoned => "Abandoned",
    }
}

#[async_trait]
impl TicketingProvider for LinearTicketingProvider {
    fn provider(&self) -> TicketProvider {
        TicketProvider::Linear
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        let response = self
            .transport
            .execute(GraphqlRequest::new(VIEWER_HEALTH_QUERY, json!({})))
            .await?;
        let payload: ViewerHealthData = serde_json::from_value(response).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "failed to decode Linear health-check payload: {error}"
            ))
        })?;
        if payload.viewer.id.trim().is_empty() {
            return Err(CoreError::DependencyUnavailable(
                "Linear health check succeeded but returned an empty viewer id.".to_owned(),
            ));
        }
        Ok(())
    }

    async fn list_tickets(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError> {
        let cache_empty = {
            let cache = self.cache.read().expect("linear ticket cache read lock");
            cache.tickets.is_empty()
        };
        if cache_empty {
            self.sync_with_query(query.clone()).await?;
        }

        let cache = self.cache.read().expect("linear ticket cache read lock");
        let viewer_id = cache.viewer_id.as_deref();

        let mut tickets = cache
            .tickets
            .values()
            .filter(|entry| cached_ticket_matches_query(entry, &query, viewer_id))
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        tickets.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.identifier.cmp(&right.identifier))
        });
        if let Some(limit) = query.limit {
            tickets.truncate(limit as usize);
        }

        Ok(tickets)
    }

    async fn create_ticket(
        &self,
        _request: CreateTicketRequest,
    ) -> Result<TicketSummary, CoreError> {
        Err(CoreError::Configuration(
            "Linear create_ticket is out of scope for AP-119; this adapter currently supports list/search and polling sync."
                .to_owned(),
        ))
    }

    async fn update_ticket_state(
        &self,
        request: UpdateTicketStateRequest,
    ) -> Result<(), CoreError> {
        let issue_id = linear_provider_ticket_id(&request.ticket_id)?;
        let target_state = request.state.trim();
        if target_state.is_empty() {
            return Err(CoreError::Configuration(
                "Linear ticket state updates require a non-empty target state.".to_owned(),
            ));
        }
        let state = resolve_linear_state_id(&self.transport, issue_id, target_state).await?;

        let response = self
            .transport
            .execute(GraphqlRequest::new(
                UPDATE_ISSUE_STATE_MUTATION,
                json!({
                    "id": issue_id,
                    "stateId": state.id,
                }),
            ))
            .await?;
        let payload: UpdateIssueStateResponse =
            serde_json::from_value(response).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "failed to decode Linear update-state payload: {error}"
                ))
            })?;
        if !payload.issue_update.success {
            return Err(CoreError::DependencyUnavailable(
                "Linear issueUpdate mutation returned success=false.".to_owned(),
            ));
        }

        {
            let mut cache = self.cache.write().expect("linear ticket cache write lock");
            if let Some(entry) = cache.tickets.get_mut(issue_id) {
                entry.summary.state = state.name;
            }
        }

        Ok(())
    }

    async fn add_comment(&self, request: AddTicketCommentRequest) -> Result<(), CoreError> {
        let issue_id = linear_provider_ticket_id(&request.ticket_id)?;
        let body = compose_linear_comment_body(request.comment.as_str(), &request.attachments)?;

        let response = self
            .transport
            .execute(GraphqlRequest::new(
                COMMENT_CREATE_MUTATION,
                json!({
                    "issueId": issue_id,
                    "body": body,
                }),
            ))
            .await?;
        let payload: CommentCreateResponse = serde_json::from_value(response).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "failed to decode Linear commentCreate payload: {error}"
            ))
        })?;
        if !payload.comment_create.success {
            return Err(CoreError::DependencyUnavailable(
                "Linear commentCreate mutation returned success=false.".to_owned(),
            ));
        }

        Ok(())
    }
}

fn cached_ticket_matches_query(
    entry: &CachedTicket,
    query: &TicketQuery,
    viewer_id: Option<&str>,
) -> bool {
    if query.assigned_to_me {
        let matches_assignee = entry
            .assignee_id
            .as_deref()
            .zip(viewer_id)
            .map(|(assignee_id, viewer_id)| assignee_id == viewer_id)
            .unwrap_or(false);
        if !matches_assignee {
            return false;
        }
    }

    if !query.states.is_empty() {
        let state_matches = query
            .states
            .iter()
            .any(|state| state.eq_ignore_ascii_case(&entry.summary.state));
        if !state_matches {
            return false;
        }
    }

    if let Some(search) = query.search.as_deref() {
        let needle = search.trim().to_ascii_lowercase();
        if !needle.is_empty() {
            let haystack = format!(
                "{} {} {}",
                entry.summary.identifier, entry.summary.title, entry.summary.state
            )
            .to_ascii_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }
    }

    true
}

#[derive(Debug, Deserialize)]
struct GraphqlResponseEnvelope {
    data: Option<serde_json::Value>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct ViewerHealthData {
    viewer: ViewerNode,
}

#[derive(Debug, Deserialize)]
struct IssueTeamStatesResponse {
    issue: Option<IssueTeamStateIssueNode>,
}

#[derive(Debug, Deserialize)]
struct IssueTeamStateIssueNode {
    team: Option<IssueTeamStateTeamNode>,
}

#[derive(Debug, Deserialize)]
struct IssueTeamStateTeamNode {
    states: IssueTeamStateConnection,
}

#[derive(Debug, Deserialize)]
struct IssueTeamStateConnection {
    nodes: Vec<IssueTeamStateNode>,
}

#[derive(Debug, Deserialize)]
struct IssueTeamStateNode {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct UpdateIssueStateResponse {
    #[serde(rename = "issueUpdate")]
    issue_update: MutationResult,
}

#[derive(Debug, Deserialize)]
struct CommentCreateResponse {
    #[serde(rename = "commentCreate")]
    comment_create: MutationResult,
}

#[derive(Debug, Deserialize)]
struct MutationResult {
    success: bool,
}

#[derive(Debug, Deserialize)]
struct ListOpenIssuesData {
    viewer: ViewerNode,
    issues: LinearIssueConnection,
}

#[derive(Debug, Deserialize)]
struct ViewerNode {
    id: String,
}

#[derive(Debug, Deserialize)]
struct LinearIssueConnection {
    nodes: Vec<LinearIssueNode>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueNode {
    id: String,
    identifier: String,
    title: String,
    url: String,
    priority: Option<i32>,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    assignee: Option<LinearAssigneeNode>,
    state: Option<LinearStateNode>,
    labels: Option<LinearLabelConnection>,
}

#[derive(Debug, Deserialize)]
struct LinearAssigneeNode {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearStateNode {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearLabelConnection {
    nodes: Vec<LinearLabelNode>,
}

#[derive(Debug, Deserialize)]
struct LinearLabelNode {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tokio::time::{sleep, timeout};

    #[derive(Debug, Default)]
    struct StubTransport {
        requests: Mutex<Vec<GraphqlRequest>>,
        responses: Mutex<VecDeque<serde_json::Value>>,
    }

    impl StubTransport {
        async fn push_response(&self, value: serde_json::Value) {
            self.responses.lock().await.push_back(value);
        }

        async fn request_count(&self) -> usize {
            self.requests.lock().await.len()
        }

        async fn requests(&self) -> Vec<GraphqlRequest> {
            self.requests.lock().await.clone()
        }
    }

    #[async_trait]
    impl GraphqlTransport for StubTransport {
        async fn execute(&self, request: GraphqlRequest) -> Result<serde_json::Value, CoreError> {
            self.requests.lock().await.push(request);
            let mut responses = self.responses.lock().await;
            if let Some(response) = responses.pop_front() {
                return Ok(response);
            }

            Err(CoreError::DependencyUnavailable(
                "stub transport has no more queued responses".to_owned(),
            ))
        }
    }

    fn config_with(interval: Duration, query: TicketQuery) -> LinearConfig {
        LinearConfig {
            api_url: DEFAULT_LINEAR_API_URL.to_owned(),
            api_key: "token".to_owned(),
            poll_interval: interval,
            sync_query: query,
            fetch_limit: 50,
            workflow_sync: LinearWorkflowSyncConfig::default(),
        }
    }

    fn issue_json(
        id: &str,
        identifier: &str,
        title: &str,
        state: &str,
        updated_at: &str,
        assignee_id: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "id": id,
            "identifier": identifier,
            "title": title,
            "url": format!("https://linear.app/example/issue/{identifier}"),
            "priority": 2,
            "updatedAt": updated_at,
            "assignee": assignee_id.map(|value| json!({ "id": value })),
            "state": { "name": state },
            "labels": { "nodes": [{ "name": "orchestrator" }] }
        })
    }

    fn list_payload(viewer_id: &str, issues: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
            "viewer": { "id": viewer_id },
            "issues": { "nodes": issues }
        })
    }

    fn issue_team_states_payload(states: &[(&str, &str)]) -> serde_json::Value {
        json!({
            "issue": {
                "team": {
                    "states": {
                        "nodes": states
                            .iter()
                            .map(|(id, name)| json!({ "id": id, "name": name }))
                            .collect::<Vec<_>>()
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn sync_once_updates_cache_with_filtered_relevant_issues() {
        let query = TicketQuery {
            assigned_to_me: true,
            states: vec!["In Progress".to_owned()],
            search: None,
            limit: Some(10),
        };
        let config = config_with(Duration::from_secs(60), query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![
                    issue_json(
                        "issue-1",
                        "AP-100",
                        "Implement parser",
                        "In Progress",
                        "2026-02-16T12:00:00.000Z",
                        Some("viewer-1"),
                    ),
                    issue_json(
                        "issue-2",
                        "AP-101",
                        "Write docs",
                        "Todo",
                        "2026-02-16T11:00:00.000Z",
                        Some("viewer-1"),
                    ),
                    issue_json(
                        "issue-3",
                        "AP-102",
                        "Peer review",
                        "In Progress",
                        "2026-02-16T10:00:00.000Z",
                        Some("someone-else"),
                    ),
                ],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport);
        let synced = provider.sync_once().await.expect("sync succeeds");

        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].identifier, "AP-100");

        let snapshot = provider.cache_snapshot();
        assert_eq!(snapshot.tickets.len(), 1);
        assert!(snapshot.last_synced_at.is_some());
        assert!(snapshot.last_sync_error.is_none());
    }

    #[tokio::test]
    async fn list_tickets_filters_from_cache_for_search_and_state() {
        let sync_query = TicketQuery {
            assigned_to_me: true,
            states: Vec::new(),
            search: None,
            limit: Some(20),
        };
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![
                    issue_json(
                        "issue-11",
                        "AP-111",
                        "Parser fails for escaped slash",
                        "In Progress",
                        "2026-02-16T12:10:00.000Z",
                        Some("viewer-1"),
                    ),
                    issue_json(
                        "issue-12",
                        "AP-112",
                        "UI copy polish",
                        "Todo",
                        "2026-02-16T12:05:00.000Z",
                        Some("viewer-1"),
                    ),
                ],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport);
        provider.sync_once().await.expect("seed cache");

        let result = provider
            .list_tickets(TicketQuery {
                assigned_to_me: true,
                states: vec!["In Progress".to_owned()],
                search: Some("escaped".to_owned()),
                limit: Some(5),
            })
            .await
            .expect("list from cache");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].identifier, "AP-111");
    }

    #[tokio::test]
    async fn list_tickets_triggers_initial_sync_when_cache_is_empty() {
        let sync_query = TicketQuery {
            assigned_to_me: true,
            states: Vec::new(),
            search: None,
            limit: Some(20),
        };
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-21",
                    "AP-121",
                    "Initial cache warmup",
                    "Todo",
                    "2026-02-16T12:15:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
        let result = provider
            .list_tickets(TicketQuery {
                assigned_to_me: true,
                states: Vec::new(),
                search: None,
                limit: Some(10),
            })
            .await
            .expect("list call warms cache");

        assert_eq!(result.len(), 1);
        assert_eq!(transport.request_count().await, 1);
    }

    #[tokio::test]
    async fn list_tickets_initial_sync_uses_requested_query() {
        let sync_query = TicketQuery {
            assigned_to_me: true,
            states: Vec::new(),
            search: None,
            limit: Some(20),
        };
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-51",
                    "AP-151",
                    "Unassigned issue should still load",
                    "Todo",
                    "2026-02-16T12:17:00.000Z",
                    None,
                )],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport);
        let result = provider
            .list_tickets(TicketQuery {
                assigned_to_me: false,
                states: Vec::new(),
                search: None,
                limit: Some(10),
            })
            .await
            .expect("list call warms cache with requested query");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].identifier, "AP-151");
    }

    #[tokio::test]
    async fn polling_refreshes_cache_with_latest_issue_set() {
        let sync_query = TicketQuery {
            assigned_to_me: true,
            states: Vec::new(),
            search: None,
            limit: Some(20),
        };
        let config = config_with(Duration::from_millis(30), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-31",
                    "AP-131",
                    "First sync payload",
                    "In Progress",
                    "2026-02-16T12:20:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-32",
                    "AP-132",
                    "Second sync payload",
                    "In Progress",
                    "2026-02-16T12:21:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-32",
                    "AP-132",
                    "Second sync payload",
                    "In Progress",
                    "2026-02-16T12:21:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport);
        provider.start_polling().await.expect("start polling");

        timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = provider.cache_snapshot();
                if snapshot
                    .tickets
                    .iter()
                    .any(|ticket| ticket.identifier == "AP-132")
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cache refresh should eventually include second payload");

        provider.stop_polling().await.expect("stop polling");
    }

    #[tokio::test]
    async fn start_polling_is_idempotent_when_already_running() {
        let sync_query = TicketQuery {
            assigned_to_me: true,
            states: Vec::new(),
            search: None,
            limit: Some(20),
        };
        let config = config_with(Duration::from_secs(3600), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-61",
                    "AP-161",
                    "First sync payload",
                    "In Progress",
                    "2026-02-16T12:25:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-62",
                    "AP-162",
                    "Poller immediate tick payload",
                    "In Progress",
                    "2026-02-16T12:26:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport);
        provider
            .start_polling()
            .await
            .expect("initial start succeeds");
        provider
            .start_polling()
            .await
            .expect("second start should be a no-op");
        provider.stop_polling().await.expect("stop polling");
    }

    #[tokio::test]
    async fn health_check_reads_viewer_identity() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({ "viewer": { "id": "viewer-1" } }))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport);
        provider.health_check().await.expect("health check");
    }

    #[tokio::test]
    async fn create_ticket_remains_explicitly_out_of_scope() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        let provider = LinearTicketingProvider::with_transport(config, transport);

        let create_error = provider
            .create_ticket(CreateTicketRequest {
                title: "x".to_owned(),
                description: None,
                state: None,
                priority: None,
                labels: Vec::new(),
            })
            .await
            .expect_err("create is out of scope");
        assert!(create_error.to_string().contains("out of scope"));
    }

    #[tokio::test]
    async fn update_ticket_state_resolves_target_state_id_and_mutates_issue() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(issue_team_states_payload(&[
                ("state-todo", "Todo"),
                ("state-done", "Done"),
            ]))
            .await;
        transport
            .push_response(json!({ "issueUpdate": { "success": true } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .update_ticket_state(UpdateTicketStateRequest {
                ticket_id: TicketId::from("linear:issue-777"),
                state: "Done".to_owned(),
            })
            .await
            .expect("update succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[0].query.contains("IssueTeamStates"));
        assert_eq!(requests[0].variables["id"], json!("issue-777"));
        assert!(requests[1].query.contains("UpdateIssueState"));
        assert_eq!(requests[1].variables["id"], json!("issue-777"));
        assert_eq!(requests[1].variables["stateId"], json!("state-done"));
    }

    #[tokio::test]
    async fn update_ticket_state_rejects_unknown_target_state_name() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(issue_team_states_payload(&[("state-todo", "Todo")]))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport);

        let error = provider
            .update_ticket_state(UpdateTicketStateRequest {
                ticket_id: TicketId::from("linear:issue-778"),
                state: "Done".to_owned(),
            })
            .await
            .expect_err("unknown state should fail");
        assert!(error.to_string().contains("no state named"));
    }

    #[tokio::test]
    async fn update_ticket_state_rejects_empty_target_state_before_network_call() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let error = provider
            .update_ticket_state(UpdateTicketStateRequest {
                ticket_id: TicketId::from("linear:issue-778"),
                state: "   ".to_owned(),
            })
            .await
            .expect_err("empty state should fail");

        assert!(error.to_string().contains("non-empty target state"));
        assert_eq!(transport.request_count().await, 0);
    }

    #[tokio::test]
    async fn update_ticket_state_updates_cache_with_linear_state_casing() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-779",
                    "AP-779",
                    "Cache state casing",
                    "Todo",
                    "2026-02-16T12:28:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;
        transport
            .push_response(issue_team_states_payload(&[
                ("state-todo", "Todo"),
                ("state-done", "Done"),
            ]))
            .await;
        transport
            .push_response(json!({ "issueUpdate": { "success": true } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport);
        provider.sync_once().await.expect("seed cache");

        provider
            .update_ticket_state(UpdateTicketStateRequest {
                ticket_id: TicketId::from("linear:issue-779"),
                state: "  done ".to_owned(),
            })
            .await
            .expect("update succeeds");

        let snapshot = provider.cache_snapshot();
        let updated = snapshot
            .tickets
            .iter()
            .find(|ticket| ticket.ticket_id == TicketId::from("linear:issue-779"))
            .expect("ticket should remain in cache");
        assert_eq!(updated.state, "Done");
    }

    #[tokio::test]
    async fn add_comment_posts_comment_with_attachment_markdown() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({ "commentCreate": { "success": true } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .add_comment(AddTicketCommentRequest {
                ticket_id: TicketId::from("linear:issue-901"),
                comment: "Ready for review.".to_owned(),
                attachments: vec![TicketAttachment {
                    label: "Draft PR".to_owned(),
                    url: "https://github.com/example/repo/pull/901".to_owned(),
                }],
            })
            .await
            .expect("add comment succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].query.contains("CommentCreate"));
        assert_eq!(requests[0].variables["issueId"], json!("issue-901"));
        let body = requests[0].variables["body"].as_str().expect("string body");
        assert!(body.contains("Ready for review."));
        assert!(body.contains("[Draft PR](https://github.com/example/repo/pull/901)"));
    }

    #[tokio::test]
    async fn add_comment_rejects_empty_content_and_attachments() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        let provider = LinearTicketingProvider::with_transport(config, transport);

        let error = provider
            .add_comment(AddTicketCommentRequest {
                ticket_id: TicketId::from("linear:issue-902"),
                comment: "   ".to_owned(),
                attachments: Vec::new(),
            })
            .await
            .expect_err("empty comment should fail");
        assert!(error.to_string().contains("non-empty"));
    }

    #[tokio::test]
    async fn sync_workflow_transition_updates_state_and_posts_summary_with_pr_link() {
        let sync_query = TicketQuery::default();
        let mut config = config_with(Duration::from_secs(60), sync_query);
        config.workflow_sync.comment_summaries = true;
        config.workflow_sync.attach_pr_links = true;

        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(issue_team_states_payload(&[
                ("state-progress", "In Progress"),
                ("state-review", "In Review"),
            ]))
            .await;
        transport
            .push_response(json!({ "issueUpdate": { "success": true } }))
            .await;
        transport
            .push_response(json!({ "commentCreate": { "success": true } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .sync_workflow_transition(LinearWorkflowTransitionSyncRequest {
                ticket_id: TicketId::from("linear:issue-903"),
                from: WorkflowState::Testing,
                to: WorkflowState::ReadyForReview,
                summary: Some("All tests are green; ready for reviewer handoff.".to_owned()),
                pull_request_url: Some("https://github.com/example/repo/pull/903".to_owned()),
            })
            .await
            .expect("transition sync succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 3);
        assert!(requests[0].query.contains("IssueTeamStates"));
        assert!(requests[1].query.contains("UpdateIssueState"));
        assert!(requests[2].query.contains("CommentCreate"));
        let comment = requests[2].variables["body"]
            .as_str()
            .expect("comment body");
        assert!(comment.contains("All tests are green"));
        assert!(comment.contains("[Pull Request](https://github.com/example/repo/pull/903)"));
    }

    #[tokio::test]
    async fn sync_workflow_transition_can_skip_comment_when_disabled() {
        let sync_query = TicketQuery::default();
        let mut config = config_with(Duration::from_secs(60), sync_query);
        config.workflow_sync.comment_summaries = false;
        config.workflow_sync.attach_pr_links = false;

        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(issue_team_states_payload(&[("state-done", "Done")]))
            .await;
        transport
            .push_response(json!({ "issueUpdate": { "success": true } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .sync_workflow_transition(LinearWorkflowTransitionSyncRequest {
                ticket_id: TicketId::from("linear:issue-904"),
                from: WorkflowState::InReview,
                to: WorkflowState::Done,
                summary: Some("Merged.".to_owned()),
                pull_request_url: Some("https://github.com/example/repo/pull/904".to_owned()),
            })
            .await
            .expect("transition sync succeeds");

        assert_eq!(transport.request_count().await, 2);
    }

    #[test]
    fn parse_workflow_state_map_parses_case_and_separator_variants() {
        let mappings = parse_workflow_state_map_env(
            "implementing=In Progress, ready_for_review=In Review, DONE=Done",
        )
        .expect("workflow state mapping should parse");
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0].workflow_state, WorkflowState::Implementing);
        assert_eq!(mappings[0].linear_state, "In Progress");
        assert_eq!(mappings[1].workflow_state, WorkflowState::ReadyForReview);
        assert_eq!(mappings[2].workflow_state, WorkflowState::Done);
    }

    #[test]
    fn parse_workflow_state_map_rejects_duplicate_workflow_states() {
        let error = parse_workflow_state_map_env("Done=Done, done=Canceled")
            .expect_err("duplicate workflow states should fail");
        assert!(error.to_string().contains("duplicate mapping"));
    }

    #[test]
    fn parse_sync_interval_rejects_zero() {
        let error = parse_sync_interval_secs("0").expect_err("zero interval should fail");
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn truncate_for_error_handles_multibyte_utf8() {
        let body = "é".repeat(300);
        let truncated = truncate_for_error(&body);

        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), 203);
    }
}
