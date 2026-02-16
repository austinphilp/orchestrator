use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use orchestrator_core::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, TicketId, TicketProvider, TicketQuery,
    TicketSummary, TicketingProvider, UpdateTicketStateRequest,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::warn;

const DEFAULT_LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const DEFAULT_SYNC_INTERVAL_SECS: u64 = 60;
const DEFAULT_FETCH_LIMIT: u32 = 100;

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

#[derive(Clone)]
pub struct LinearConfig {
    pub api_url: String,
    pub api_key: String,
    pub poll_interval: Duration,
    pub sync_query: TicketQuery,
    pub fetch_limit: u32,
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
        _request: UpdateTicketStateRequest,
    ) -> Result<(), CoreError> {
        Err(CoreError::Configuration(
            "Linear update_ticket_state is out of scope for AP-119; this adapter currently supports list/search and polling sync."
                .to_owned(),
        ))
    }

    async fn add_comment(&self, _request: AddTicketCommentRequest) -> Result<(), CoreError> {
        Err(CoreError::Configuration(
            "Linear add_comment is out of scope for AP-119; this adapter currently supports list/search and polling sync."
                .to_owned(),
        ))
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
    async fn mutating_methods_are_explicitly_out_of_scope() {
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

        let update_error = provider
            .update_ticket_state(UpdateTicketStateRequest {
                ticket_id: TicketId::from("linear:x"),
                state: "Done".to_owned(),
            })
            .await
            .expect_err("update is out of scope");
        assert!(update_error.to_string().contains("out of scope"));

        let comment_error = provider
            .add_comment(AddTicketCommentRequest {
                ticket_id: TicketId::from("linear:x"),
                comment: "hello".to_owned(),
                attachments: Vec::new(),
            })
            .await
            .expect_err("comment is out of scope");
        assert!(comment_error.to_string().contains("out of scope"));
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
