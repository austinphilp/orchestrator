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
        project: issue.project.as_ref().map(|project| project.name.clone()),
        state: issue
            .state
            .as_ref()
            .map(|state| state.name.clone())
            .unwrap_or_else(|| "Unknown".to_owned()),
        url: issue.url.clone(),
        assignee: issue
            .assignee
            .as_ref()
            .and_then(|assignee| assignee.name.clone().or_else(|| assignee.id.clone())),
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

#[derive(Debug, Clone)]
struct CreateTicketStateSpec {
    team_token: Option<String>,
    state_name: Option<String>,
}

fn parse_create_ticket_state_spec(raw_state: &str) -> CreateTicketStateSpec {
    let raw = raw_state.trim();
    if raw.is_empty() {
        return CreateTicketStateSpec {
            team_token: None,
            state_name: None,
        };
    }

    for separator in [":", "/"] {
        if let Some((team, state)) = raw.split_once(separator) {
            let team_token = team.trim();
            let state_name = state.trim();
            return CreateTicketStateSpec {
                team_token: (!team_token.is_empty()).then_some(team_token.to_owned()),
                state_name: (!state_name.is_empty()).then_some(state_name.to_owned()),
            };
        }
    }

    CreateTicketStateSpec {
        team_token: None,
        state_name: Some(raw.to_owned()),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ResolvedLinearState {
    id: String,
    name: String,
}

fn linear_api_error_is_auth(message: &str) -> bool {
    let value = message.to_ascii_lowercase();
    value.contains("unauthorized")
        || value.contains("forbidden")
        || value.contains("invalid api key")
        || value.contains("authentication")
        || value.contains("authentication failed")
        || value.contains("token")
        || value.contains("access denied")
}

fn normalize_linear_api_error(error: CoreError, action: &str) -> CoreError {
    match error {
        CoreError::DependencyUnavailable(message) if linear_api_error_is_auth(&message) => {
            CoreError::Configuration(format!("Linear {action}: {message}"))
        }
        _ => error,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LinearTeamNode {
    id: String,
    key: Option<String>,
    name: String,
}

#[derive(Debug)]
struct LinearCreateIssueContext {
    team_id: String,
    team_state: Option<ResolvedLinearState>,
}

async fn resolve_linear_teams(
    transport: &Arc<dyn GraphqlTransport>,
) -> Result<Vec<LinearTeamNode>, CoreError> {
    let data = transport
        .execute(GraphqlRequest::new(TEAMS_QUERY, json!({})))
        .await
        .map_err(|error| normalize_linear_api_error(error, "could not resolve Linear teams"))?;
    let payload: TeamListResponse = serde_json::from_value(data).map_err(|error| {
        CoreError::DependencyUnavailable(format!("failed to decode Linear teams payload: {error}"))
    })?;

    if payload.teams.nodes.is_empty() {
        return Err(CoreError::DependencyUnavailable(
            "Linear did not return any teams for the authenticated user.".to_owned(),
        ));
    }

    Ok(payload.teams.nodes)
}

fn team_matches_token(team: &LinearTeamNode, token: &str) -> bool {
    let token = token.trim();
    if team.id.eq(token) {
        return true;
    }
    if let Some(key) = team.key.as_deref() {
        if key.eq_ignore_ascii_case(token) {
            return true;
        }
    }

    team.name.eq_ignore_ascii_case(token)
}

fn linear_team_list_hint(teams: &[LinearTeamNode]) -> String {
    teams
        .iter()
        .map(|team| {
            if let Some(key) = &team.key {
                format!("{} ({})", key, team.name)
            } else {
                team.name.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn resolve_linear_team_state_id(
    transport: &Arc<dyn GraphqlTransport>,
    team_id: &str,
    target_state: &str,
) -> Result<Option<ResolvedLinearState>, CoreError> {
    let data = transport
        .execute(GraphqlRequest::new(
            TEAM_STATES_QUERY,
            json!({ "id": team_id }),
        ))
        .await
        .map_err(|error| {
            normalize_linear_api_error(
                error,
                "could not resolve Linear state for team while creating issue",
            )
        })?;
    let payload: TeamStatesResponse = serde_json::from_value(data).map_err(|error| {
        CoreError::DependencyUnavailable(format!(
            "failed to decode Linear team states payload: {error}"
        ))
    })?;
    let team = payload.team.ok_or_else(|| {
        CoreError::DependencyUnavailable(format!(
            "Linear team lookup returned no team for id `{team_id}`."
        ))
    })?;

    Ok(team
        .states
        .nodes
        .into_iter()
        .find(|state| state.name.eq_ignore_ascii_case(target_state.trim()))
        .map(|state| ResolvedLinearState {
            id: state.id,
            name: state.name,
        }))
}

async fn resolve_linear_create_context(
    transport: &Arc<dyn GraphqlTransport>,
    state: Option<&str>,
) -> Result<LinearCreateIssueContext, CoreError> {
    let teams = resolve_linear_teams(transport).await?;
    let CreateTicketStateSpec {
        team_token,
        state_name,
    } = state
        .filter(|value| !value.trim().is_empty())
        .map(parse_create_ticket_state_spec)
        .unwrap_or(CreateTicketStateSpec {
            team_token: None,
            state_name: None,
        });

    let team = if let Some(token) = team_token.as_deref() {
        teams
            .iter()
            .find(|team| team_matches_token(team, token))
            .cloned()
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "No Linear team matches `{token}`. Available teams: {}",
                    linear_team_list_hint(&teams)
                ))
            })?
    } else if teams.len() == 1 {
        teams[0].clone()
    } else if state_name.is_none() {
        return Err(CoreError::Configuration(
            "Linear issueCreate requires a team when multiple teams are available. Provide the team in `state` as `<team-id|team-key>:<state>` or configure a default team.".to_owned(),
        ));
    } else {
        let target_state = state_name.as_deref().expect("state present");
        let mut matches = Vec::new();
        for team in &teams {
            if let Some(state) =
                resolve_linear_team_state_id(transport, &team.id, target_state).await?
            {
                matches.push((team.clone(), state));
            }
        }

        if matches.is_empty() {
            return Err(CoreError::Configuration(format!(
                "Linear teams do not expose a state named `{target_state}`. Available teams: {}",
                linear_team_list_hint(&teams)
            )));
        }
        if matches.len() > 1 {
            return Err(CoreError::Configuration(format!(
                "State `{target_state}` exists in multiple teams. Provide team explicitly in `state` as `<team-id|team-key>:<state>`. Available teams: {}",
                linear_team_list_hint(&teams)
            )));
        }
        let (team, state) = matches.pop().expect("single match found");
        return Ok(LinearCreateIssueContext {
            team_id: team.id,
            team_state: Some(state),
        });
    };

    let team_state = if let Some(target_state) = state_name.as_deref() {
        let state = resolve_linear_team_state_id(transport, &team.id, target_state)
            .await?
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "Linear team `{}` has no state named `{target_state}`.",
                    team.id
                ))
            })?;
        Some(state)
    } else {
        None
    };

    Ok(LinearCreateIssueContext {
        team_id: team.id,
        team_state,
    })
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

