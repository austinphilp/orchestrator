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

    async fn list_projects(&self) -> Result<Vec<String>, CoreError> {
        let data = self
            .transport
            .execute(GraphqlRequest::new(TEAMS_QUERY, json!({})))
            .await
            .map_err(|error| normalize_linear_api_error(error, "could not list Linear projects"))?;
        let payload: TeamListResponse = serde_json::from_value(data).map_err(|error| {
            CoreError::DependencyUnavailable(format!("failed to decode Linear teams payload: {error}"))
        })?;

        let mut projects = payload
            .teams
            .nodes
            .into_iter()
            .flat_map(|team| {
                team.projects
                    .nodes
                    .into_iter()
                    .map(|project| project.name)
            })
            .filter(|name| !name.trim().is_empty())
            .collect::<Vec<_>>();

        if projects.is_empty() {
            let cache_projects = {
                let cache = self.cache.read().expect("linear ticket cache read lock");
                cache
                    .tickets
                    .values()
                    .filter_map(|ticket| ticket.summary.project.clone())
                    .collect::<Vec<_>>()
            };
            projects.extend(cache_projects);
        }

        let mut deduped_projects = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for mut project in projects {
            project = project.trim().to_owned();
            let normalized = project.to_ascii_lowercase();
            if project.is_empty() || project == "No Project" || !seen.insert(normalized) {
                continue;
            }
            deduped_projects.push(project);
        }

        deduped_projects.sort_by(|left, right| {
            left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase())
        });
        Ok(deduped_projects)
    }

    async fn create_ticket(
        &self,
        request: CreateTicketRequest,
    ) -> Result<TicketSummary, CoreError> {
        let title = request.title.trim().to_owned();
        if title.is_empty() {
            return Err(CoreError::Configuration(
                "Linear ticket title cannot be empty.".to_owned(),
            ));
        }
        if !request.labels.is_empty() {
            return Err(CoreError::Configuration(
                "Linear create_ticket does not currently support label creation.".to_owned(),
            ));
        }

        let context =
            resolve_linear_create_context(&self.transport, request.state.as_deref()).await?;
        let mut input = json!({
            "teamId": context.team_id,
            "title": title,
        });

        if let Some(description) = request.description.as_deref() {
            let description = description.trim();
            if !description.is_empty() {
                input["description"] = json!(description);
            }
        }
        if let Some(priority) = request.priority {
            input["priority"] = json!(priority);
        }
        if let Some(team_state) = context.team_state {
            input["stateId"] = json!(team_state.id);
        }

        let response = self
            .transport
            .execute(GraphqlRequest::new(
                ISSUE_CREATE_MUTATION,
                json!({ "input": input }),
            ))
            .await
            .map_err(|error| normalize_linear_api_error(error, "issue create failed"))?;
        let payload: IssueCreateResponse = serde_json::from_value(response).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "failed to decode Linear issueCreate payload: {error}"
            ))
        })?;
        if !payload.issue_create.success {
            return Err(CoreError::DependencyUnavailable(
                "Linear issueCreate mutation returned success=false.".to_owned(),
            ));
        }

        let issue = payload.issue_create.issue.ok_or_else(|| {
            CoreError::DependencyUnavailable(
                "Linear issueCreate mutation did not return an issue payload.".to_owned(),
            )
        })?;
        let summary = issue_to_summary(&issue);
        {
            let mut cache = self.cache.write().expect("linear ticket cache write lock");
            cache.tickets.insert(
                issue.id.clone(),
                CachedTicket {
                    summary: summary.clone(),
                    assignee_id: issue.assignee.and_then(|assignee| assignee.id),
                },
            );
        }

        Ok(summary)
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

    async fn get_ticket(&self, request: GetTicketRequest) -> Result<TicketDetails, CoreError> {
        let issue_id = linear_provider_ticket_id(&request.ticket_id)?;
        let response = self
            .transport
            .execute(GraphqlRequest::new(
                ISSUE_DETAILS_QUERY,
                json!({ "id": issue_id }),
            ))
            .await?;

        let payload: IssueDetailsResponse = serde_json::from_value(response).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "failed to decode Linear issue payload: {error}"
            ))
        })?;
        let issue = payload.issue.ok_or_else(|| {
            CoreError::DependencyUnavailable(format!(
                "Linear issue lookup returned no issue for id `{issue_id}`."
            ))
        })?;
        let summary = issue_to_summary(&issue);

        {
            let mut cache = self.cache.write().expect("linear ticket cache write lock");
            cache.tickets.insert(
                issue.id.clone(),
                CachedTicket {
                    summary: summary.clone(),
                    assignee_id: issue
                        .assignee
                        .as_ref()
                        .and_then(|assignee| assignee.id.clone()),
                },
            );
        }

        Ok(TicketDetails {
            summary,
            description: issue.description,
        })
    }

    async fn update_ticket_description(
        &self,
        request: UpdateTicketDescriptionRequest,
    ) -> Result<(), CoreError> {
        let issue_id = linear_provider_ticket_id(&request.ticket_id)?;
        let description = request.description.trim();
        if description.is_empty() {
            return Err(CoreError::Configuration(
                "Linear ticket description updates require non-empty text.".to_owned(),
            ));
        }

        let response = self
            .transport
            .execute(GraphqlRequest::new(
                UPDATE_ISSUE_DESCRIPTION_MUTATION,
                json!({
                    "id": issue_id,
                    "description": description,
                }),
            ))
            .await?;
        let payload: UpdateIssueDescriptionResponse =
            serde_json::from_value(response).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "failed to decode Linear update-description payload: {error}"
                ))
            })?;
        if !payload.issue_update.success {
            return Err(CoreError::DependencyUnavailable(
                "Linear issueUpdate mutation returned success=false for description update."
                    .to_owned(),
            ));
        }

        Ok(())
    }

    async fn archive_ticket(&self, request: ArchiveTicketRequest) -> Result<(), CoreError> {
        let issue_id = linear_provider_ticket_id(&request.ticket_id)?;
        let response = self
            .transport
            .execute(GraphqlRequest::new(
                ISSUE_ARCHIVE_MUTATION,
                json!({
                    "id": issue_id,
                }),
            ))
            .await?;
        let payload: IssueArchiveResponse = serde_json::from_value(response).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "failed to decode Linear issueArchive payload: {error}"
            ))
        })?;
        if !payload.issue_archive.success {
            return Err(CoreError::DependencyUnavailable(
                "Linear issueArchive mutation returned success=false.".to_owned(),
            ));
        }

        {
            let mut cache = self.cache.write().expect("linear ticket cache write lock");
            cache.tickets.remove(issue_id);
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
struct TeamListResponse {
    teams: TeamListConnection,
}

#[derive(Debug, Deserialize)]
struct TeamListConnection {
    nodes: Vec<LinearTeamNode>,
}

#[derive(Debug, Deserialize)]
struct TeamStatesResponse {
    team: Option<LinearTeamStateNode>,
}

#[derive(Debug, Deserialize)]
struct LinearTeamStateNode {
    states: IssueTeamStateConnection,
}

#[derive(Debug, Deserialize)]
struct IssueCreateResponse {
    #[serde(rename = "issueCreate")]
    issue_create: IssueCreateResult,
}

#[derive(Debug, Deserialize)]
struct IssueDetailsResponse {
    issue: Option<LinearIssueNode>,
}

#[derive(Debug, Deserialize)]
struct IssueCreateResult {
    success: bool,
    issue: Option<LinearIssueNode>,
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
struct IssueArchiveResponse {
    #[serde(rename = "issueArchive")]
    issue_archive: MutationResult,
}

#[derive(Debug, Deserialize)]
struct UpdateIssueDescriptionResponse {
    #[serde(rename = "issueUpdate")]
    issue_update: MutationResult,
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
    #[serde(default)]
    description: Option<String>,
    url: String,
    priority: Option<i32>,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    project: Option<LinearProjectNode>,
    assignee: Option<LinearAssigneeNode>,
    state: Option<LinearStateNode>,
    labels: Option<LinearLabelConnection>,
}

#[derive(Debug, Deserialize)]
struct LinearProjectNode {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearAssigneeNode {
    id: Option<String>,
    name: Option<String>,
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
