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
            "project": { "name": "Orchestrator" },
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

    fn teams_payload(teams: &[(&str, Option<&str>, &str)]) -> serde_json::Value {
        json!({
            "teams": {
                "nodes": teams
                    .iter()
                    .map(|(id, key, name)| {
                        if let Some(key) = key {
                            json!({ "id": id, "key": key, "name": name })
                        } else {
                            json!({ "id": id, "name": name })
                        }
                    })
                    .collect::<Vec<_>>()
            }
        })
    }

    fn team_states_payload(team_id: &str, states: &[(&str, &str)]) -> serde_json::Value {
        json!({
            "team": {
                "id": team_id,
                "states": {
                    "nodes": states
                        .iter()
                        .map(|(id, name)| json!({ "id": id, "name": name }))
                        .collect::<Vec<_>>()
                }
            }
        })
    }

    fn issue_create_payload(issue: serde_json::Value) -> serde_json::Value {
        json!({
            "issueCreate": {
                "success": true,
                "issue": issue
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

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
        let synced = provider.sync_once().await.expect("sync succeeds");

        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].identifier, "AP-100");

        let snapshot = provider.cache_snapshot();
        assert_eq!(snapshot.tickets.len(), 1);
        assert!(snapshot.last_synced_at.is_some());
        assert!(snapshot.last_sync_error.is_none());
    }

    #[tokio::test]
    async fn list_tickets_syncs_latest_payload_and_applies_search_and_state_filters() {
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
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![
                    issue_json(
                        "issue-11",
                        "AP-111",
                        "Parser fails for escaped slash",
                        "In Progress",
                        "2026-02-16T12:20:00.000Z",
                        Some("viewer-1"),
                    ),
                    issue_json(
                        "issue-13",
                        "AP-113",
                        "Escaped slash follow-up",
                        "Todo",
                        "2026-02-16T12:15:00.000Z",
                        Some("viewer-1"),
                    ),
                ],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
        provider.sync_once().await.expect("seed cache");

        let result = provider
            .list_tickets(TicketQuery {
                assigned_to_me: true,
                states: vec!["In Progress".to_owned()],
                search: Some("escaped".to_owned()),
                limit: Some(5),
            })
            .await
            .expect("list after sync");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].identifier, "AP-111");
        assert_eq!(result[0].updated_at, "2026-02-16T12:20:00.000Z");
    }

    #[tokio::test]
    async fn list_tickets_returns_cached_results_when_refresh_fails() {
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
                    "issue-14",
                    "AP-114",
                    "Fallback cache issue",
                    "In Progress",
                    "2026-02-16T12:30:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
        provider.sync_once().await.expect("seed cache");

        let result = provider
            .list_tickets(TicketQuery {
                assigned_to_me: true,
                states: vec!["In Progress".to_owned()],
                search: Some("fallback".to_owned()),
                limit: Some(5),
            })
            .await
            .expect("list should fall back to cache");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].identifier, "AP-114");
        assert_eq!(transport.request_count().await, 2);
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

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
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

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
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

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
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

        let provider = LinearTicketingProvider::with_transport(config, transport.clone());
        provider.health_check().await.expect("health check");
    }

    #[tokio::test]
    async fn create_ticket_succeeds_with_valid_input() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[("team-1", Some("ENG"), "Engineering")]))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-801",
                "AP-801",
                "Add linear ticket create test",
                "Todo",
                "2026-02-16T13:00:00.000Z",
                Some("viewer-1"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let summary = provider
            .create_ticket(CreateTicketRequest {
                title: "Add linear ticket create test".to_owned(),
                description: Some("Exercise create path".to_owned()),
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect("create succeeds");
        assert_eq!(
            summary.ticket_id,
            TicketId::from_provider_uuid(TicketProvider::Linear, "issue-801")
        );
        assert_eq!(summary.identifier, "AP-801");
        assert_eq!(summary.title, "Add linear ticket create test");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[0].query.contains("Teams"));
        assert_eq!(requests[1].query.contains("IssueCreate"), true);
        assert_eq!(requests[1].variables["input"]["teamId"], json!("team-1"));
        assert_eq!(
            requests[1].variables["input"]["title"],
            json!("Add linear ticket create test")
        );
        assert_eq!(
            requests[1].variables["input"]["description"],
            json!("Exercise create path")
        );
        assert!(!requests[1].variables["input"]
            .as_object()
            .expect("issueCreate input object")
            .contains_key("assigneeId"));
    }

    #[tokio::test]
    async fn create_ticket_assigns_to_cached_viewer_when_requested() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-77",
                vec![issue_json(
                    "issue-seed",
                    "AP-700",
                    "Seed",
                    "Todo",
                    "2026-02-16T12:00:00.000Z",
                    Some("viewer-77"),
                )],
            ))
            .await;
        transport
            .push_response(teams_payload(&[("team-1", Some("ENG"), "Engineering")]))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-805",
                "AP-805",
                "Assign from cache",
                "Todo",
                "2026-02-16T13:02:00.000Z",
                Some("viewer-77"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider.sync_once().await.expect("seed cache");
        provider
            .create_ticket(CreateTicketRequest {
                title: "Assign from cache".to_owned(),
                description: None,
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: true,
            })
            .await
            .expect("create succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 3);
        assert!(requests[2].query.contains("IssueCreate"));
        assert_eq!(
            requests[2].variables["input"]["assigneeId"],
            json!("viewer-77")
        );
    }

    #[tokio::test]
    async fn create_ticket_fetches_viewer_for_assignment_when_cache_empty() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[("team-1", Some("ENG"), "Engineering")]))
            .await;
        transport
            .push_response(json!({ "viewer": { "id": "viewer-88" } }))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-806",
                "AP-806",
                "Assign from viewer fetch",
                "Todo",
                "2026-02-16T13:03:00.000Z",
                Some("viewer-88"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .create_ticket(CreateTicketRequest {
                title: "Assign from viewer fetch".to_owned(),
                description: None,
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: true,
            })
            .await
            .expect("create succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 3);
        assert!(requests[1].query.contains("ViewerHealth"));
        assert_eq!(
            requests[2].variables["input"]["assigneeId"],
            json!("viewer-88")
        );
    }

    #[tokio::test]
    async fn create_ticket_continues_unassigned_when_viewer_lookup_fails() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[("team-1", Some("ENG"), "Engineering")]))
            .await;
        transport.push_response(json!({ "viewer": {} })).await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-807",
                "AP-807",
                "Assignment fallback",
                "Todo",
                "2026-02-16T13:04:00.000Z",
                None,
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let summary = provider
            .create_ticket(CreateTicketRequest {
                title: "Assignment fallback".to_owned(),
                description: None,
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: true,
            })
            .await
            .expect("create succeeds even when viewer lookup fails");
        assert!(summary.assignee.is_none());

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 3);
        assert!(requests[1].query.contains("ViewerHealth"));
        assert!(!requests[2].variables["input"]
            .as_object()
            .expect("issueCreate input object")
            .contains_key("assigneeId"));
    }

    #[tokio::test]
    async fn create_ticket_sets_project_id_when_project_is_selected() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "ENG",
                        "name": "Engineering",
                        "projects": {
                            "nodes": [
                                {"id": "project-1", "name": "Orchestrator"}
                            ]
                        }
                    }]
                }
            }))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-811",
                "AP-811",
                "Project selected",
                "Todo",
                "2026-02-16T13:05:00.000Z",
                Some("viewer-1"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .create_ticket(CreateTicketRequest {
                title: "Project selected".to_owned(),
                description: None,
                project: Some("orchestrator".to_owned()),
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect("create succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests[1].variables["input"]["projectId"], json!("project-1"));
    }

    #[tokio::test]
    async fn create_ticket_rejects_unknown_project() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "ENG",
                        "name": "Engineering",
                        "projects": {
                            "nodes": [
                                {"id": "project-1", "name": "Other Project"}
                            ]
                        }
                    }]
                }
            }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport);

        let create_error = provider
            .create_ticket(CreateTicketRequest {
                title: "Missing project".to_owned(),
                description: None,
                project: Some("Orchestrator".to_owned()),
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect_err("unknown project should fail");
        assert!(create_error
            .to_string()
            .contains("No Linear project matches"));
    }

    #[tokio::test]
    async fn create_ticket_resolves_state_and_team_from_state_field() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[
                ("team-1", Some("ENG"), "Engineering"),
                ("team-2", Some("OPS"), "Operations"),
            ]))
            .await;
        transport
            .push_response(team_states_payload(
                "team-1",
                &[("state-1", "Todo"), ("state-2", "Done")],
            ))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-802",
                "AP-802",
                "State-mapped issue",
                "Todo",
                "2026-02-16T13:10:00.000Z",
                Some("viewer-1"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let summary = provider
            .create_ticket(CreateTicketRequest {
                title: "State-mapped issue".to_owned(),
                description: None,
                project: None,
                state: Some("ENG:Todo".to_owned()),
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect("create succeeds");
        assert_eq!(
            summary.ticket_id,
            TicketId::from_provider_uuid(TicketProvider::Linear, "issue-802")
        );
        assert_eq!(summary.state, "Todo");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 3);
        assert!(requests[0].query.contains("Teams"));
        assert_eq!(requests[1].query.contains("TeamStates"), true);
        assert_eq!(requests[2].query.contains("IssueCreate"), true);
        assert_eq!(requests[1].variables["id"], json!("team-1"));
        assert_eq!(requests[2].variables["input"]["stateId"], json!("state-1"));
    }

    #[tokio::test]
    async fn create_ticket_rejects_empty_title_before_network_call() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let create_error = provider
            .create_ticket(CreateTicketRequest {
                title: "   ".to_owned(),
                description: None,
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect_err("empty title should fail");
        assert!(create_error.to_string().contains("title cannot be empty"));
        assert_eq!(transport.request_count().await, 0);
    }

    #[tokio::test]
    async fn create_ticket_requires_team_when_state_omitted_with_multiple_teams() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[
                ("team-1", Some("ENG"), "Engineering"),
                ("team-2", Some("OPS"), "Operations"),
            ]))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let create_error = provider
            .create_ticket(CreateTicketRequest {
                title: "Missing team".to_owned(),
                description: None,
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect_err("missing team should fail");
        assert!(create_error.to_string().contains("requires a team"));

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].query.contains("Teams"));
    }

    #[tokio::test]
    async fn create_ticket_rejects_unknown_state_name_for_team() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[
                ("team-1", Some("ENG"), "Engineering"),
                ("team-2", Some("OPS"), "Operations"),
            ]))
            .await;
        transport
            .push_response(team_states_payload(
                "team-1",
                &[("state-1", "Todo"), ("state-2", "Done")],
            ))
            .await;
        transport
            .push_response(team_states_payload(
                "team-2",
                &[("state-3", "In Progress"), ("state-4", "Done")],
            ))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport);

        let create_error = provider
            .create_ticket(CreateTicketRequest {
                title: "Missing state".to_owned(),
                description: None,
                project: None,
                state: Some("Unknown".to_owned()),
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect_err("missing state should fail");
        assert!(create_error
            .to_string()
            .contains("do not expose a state named"));
    }

    #[tokio::test]
    async fn create_ticket_resolves_team_by_name_as_token() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[
                ("team-1", Some("ENG"), "Engineering"),
                ("team-2", Some("OPS"), "Operations"),
            ]))
            .await;
        transport
            .push_response(team_states_payload("team-1", &[("state-1", "Todo")]))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-803",
                "AP-803",
                "Team name token",
                "Todo",
                "2026-02-16T13:20:00.000Z",
                Some("viewer-1"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let summary = provider
            .create_ticket(CreateTicketRequest {
                title: "Team name token".to_owned(),
                description: None,
                project: None,
                state: Some("Engineering:Todo".to_owned()),
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect("create succeeds");
        assert_eq!(
            summary.ticket_id,
            TicketId::from_provider_uuid(TicketProvider::Linear, "issue-803")
        );

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].variables["id"], json!("team-1"));
    }

    #[tokio::test]
    async fn create_ticket_omits_empty_description_from_issue_create_payload() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(teams_payload(&[("team-1", Some("ENG"), "Engineering")]))
            .await;
        transport
            .push_response(issue_create_payload(issue_json(
                "issue-804",
                "AP-804",
                "No description",
                "Todo",
                "2026-02-16T13:25:00.000Z",
                Some("viewer-1"),
            )))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .create_ticket(CreateTicketRequest {
                title: "No description".to_owned(),
                description: Some("   ".to_owned()),
                project: None,
                state: None,
                priority: None,
                labels: Vec::new(),
                assign_to_api_key_user: false,
            })
            .await
            .expect("create succeeds");

        let requests = transport.requests().await;
        assert!(!requests[1].variables["input"]
            .as_object()
            .unwrap()
            .contains_key("description"));
    }

    #[tokio::test]
    async fn create_ticket_rejects_labels_until_linear_support_is_implemented() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let create_error = provider
            .create_ticket(CreateTicketRequest {
                title: "Label test".to_owned(),
                description: None,
                project: None,
                state: None,
                priority: None,
                labels: vec!["orchestrator".to_owned()],
                assign_to_api_key_user: false,
            })
            .await
            .expect_err("labels not supported should fail");
        assert!(create_error
            .to_string()
            .contains("does not currently support label creation"));
        assert_eq!(transport.request_count().await, 0);
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
    async fn list_tickets_query_excludes_archived_issues() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query.clone());
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-121",
                    "AP-121",
                    "Visible issue",
                    "Todo",
                    "2026-02-16T12:10:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let _ = provider
            .list_tickets(sync_query)
            .await
            .expect("list tickets succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].query.contains("archivedAt: { null: true }"));
    }

    #[tokio::test]
    async fn archive_ticket_uses_issue_archive_mutation_and_evicts_cache() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query.clone());
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(list_payload(
                "viewer-1",
                vec![issue_json(
                    "issue-131",
                    "AP-131",
                    "Archive me",
                    "Todo",
                    "2026-02-16T12:10:00.000Z",
                    Some("viewer-1"),
                )],
            ))
            .await;
        transport
            .push_response(json!({ "issueArchive": { "success": true } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let tickets = provider
            .list_tickets(sync_query)
            .await
            .expect("list tickets succeeds");
        assert_eq!(tickets.len(), 1);

        provider
            .archive_ticket(ArchiveTicketRequest {
                ticket_id: tickets[0].ticket_id.clone(),
            })
            .await
            .expect("archive succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[1].query.contains("ArchiveIssue"));
        assert!(provider.cache_snapshot().tickets.is_empty());
    }

    #[tokio::test]
    async fn archive_ticket_returns_error_when_linear_archive_fails() {
        let config = config_with(Duration::from_secs(60), TicketQuery::default());
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({ "issueArchive": { "success": false } }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport);

        let error = provider
            .archive_ticket(ArchiveTicketRequest {
                ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "issue-141"),
            })
            .await
            .expect_err("archive should fail");
        assert!(error
            .to_string()
            .contains("issueArchive mutation returned success=false"));
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
    async fn get_ticket_fetches_summary_and_description() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({
                "issue": {
                    "id": "issue-1001",
                    "identifier": "AP-1001",
                    "title": "Fetch ticket details",
                    "description": "Tracks a high-priority bug",
                    "url": "https://linear.app/example/issue/AP-1001",
                    "priority": 3,
                    "updatedAt": "2026-02-17T14:00:00.000Z",
                    "project": { "name": "Orchestrator" },
                    "assignee": { "id": "viewer-1" },
                    "state": { "name": "In Progress" },
                    "labels": { "nodes": [{ "name": "orchestrator" }] }
                }
            }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        let details = provider
            .get_ticket(GetTicketRequest {
                ticket_id: TicketId::from("linear:issue-1001"),
            })
            .await
            .expect("get ticket succeeds");

        assert_eq!(details.summary.identifier, "AP-1001");
        assert_eq!(details.summary.title, "Fetch ticket details");
        assert_eq!(
            details.description,
            Some("Tracks a high-priority bug".to_owned())
        );

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].query.contains("IssueDetails"));
        assert_eq!(requests[0].variables["id"], json!("issue-1001"));
    }

    #[tokio::test]
    async fn update_ticket_description_updates_issue_description() {
        let sync_query = TicketQuery::default();
        let config = config_with(Duration::from_secs(60), sync_query);
        let transport = Arc::new(StubTransport::default());
        transport
            .push_response(json!({
                "issueUpdate": { "success": true }
            }))
            .await;
        let provider = LinearTicketingProvider::with_transport(config, transport.clone());

        provider
            .update_ticket_description(UpdateTicketDescriptionRequest {
                ticket_id: TicketId::from("linear:issue-1002"),
                description: "Updated by command".to_owned(),
            })
            .await
            .expect("description update succeeds");

        let requests = transport.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].query.contains("UpdateIssueDescription"));
        assert_eq!(requests[0].variables["id"], json!("issue-1002"));
        assert_eq!(
            requests[0].variables["description"],
            json!("Updated by command")
        );
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
                from: WorkflowState::Implementing,
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
        let mappings = parse_workflow_state_map_settings(&[
            WorkflowStateMapSetting {
                workflow_state: "implementing".to_owned(),
                linear_state: "In Progress".to_owned(),
            },
            WorkflowStateMapSetting {
                workflow_state: "ready_for_review".to_owned(),
                linear_state: "In Review".to_owned(),
            },
            WorkflowStateMapSetting {
                workflow_state: "pending_merge".to_owned(),
                linear_state: "In Review".to_owned(),
            },
            WorkflowStateMapSetting {
                workflow_state: "DONE".to_owned(),
                linear_state: "Done".to_owned(),
            },
        ])
        .expect("workflow state mapping should parse");
        assert_eq!(mappings.len(), 4);
        assert_eq!(mappings[0].workflow_state, WorkflowState::Implementing);
        assert_eq!(mappings[0].linear_state, "In Progress");
        assert_eq!(mappings[1].workflow_state, WorkflowState::ReadyForReview);
        assert_eq!(mappings[2].workflow_state, WorkflowState::PendingMerge);
        assert_eq!(mappings[3].workflow_state, WorkflowState::Done);
    }

    #[test]
    fn parse_workflow_state_map_normalizes_legacy_testing_to_implementing() {
        let mappings = parse_workflow_state_map_settings(&[WorkflowStateMapSetting {
            workflow_state: "testing".to_owned(),
            linear_state: "In Progress".to_owned(),
        }])
        .expect("legacy testing mapping should parse");
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].workflow_state, WorkflowState::Implementing);
    }

    #[test]
    fn parse_workflow_state_map_rejects_duplicate_workflow_states() {
        let error = parse_workflow_state_map_settings(&[
            WorkflowStateMapSetting {
                workflow_state: "pending_merge".to_owned(),
                linear_state: "In Review".to_owned(),
            },
            WorkflowStateMapSetting {
                workflow_state: "merging".to_owned(),
                linear_state: "Queued for Merge".to_owned(),
            },
        ])
        .expect_err("duplicate workflow states should fail");
        assert!(error.to_string().contains("duplicate mapping"));
    }

    #[test]
    fn parse_workflow_state_map_rejects_legacy_testing_and_implementing_duplicates() {
        let error = parse_workflow_state_map_settings(&[
            WorkflowStateMapSetting {
                workflow_state: "Implementing".to_owned(),
                linear_state: "In Progress".to_owned(),
            },
            WorkflowStateMapSetting {
                workflow_state: "testing".to_owned(),
                linear_state: "In Progress".to_owned(),
            },
        ])
        .expect_err("testing and implementing should be treated as duplicates");
        assert!(error.to_string().contains("duplicate mapping"));
    }

    #[test]
    fn parse_create_ticket_state_spec_handles_team_prefixes() {
        let colon_spec = parse_create_ticket_state_spec("ENG:Todo");
        assert_eq!(colon_spec.team_token.as_deref(), Some("ENG"));
        assert_eq!(colon_spec.state_name.as_deref(), Some("Todo"));

        let slash_spec = parse_create_ticket_state_spec("ops/Todo");
        assert_eq!(slash_spec.team_token.as_deref(), Some("ops"));
        assert_eq!(slash_spec.state_name.as_deref(), Some("Todo"));
    }

    #[test]
    fn parse_create_ticket_state_spec_treats_whitespace_only_input_as_empty() {
        let spec = parse_create_ticket_state_spec("   ");
        assert!(spec.team_token.is_none());
        assert!(spec.state_name.is_none());
    }

    #[test]
    fn parse_sync_interval_rejects_zero() {
        let error = parse_sync_interval_secs(0).expect_err("zero interval should fail");
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn truncate_for_error_handles_multibyte_utf8() {
        let body = "é".repeat(300);
        let truncated = truncate_for_error(&body);

        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), 203);
    }

    #[test]
    fn reqwest_transport_rejects_empty_endpoint() {
        let error = ReqwestGraphqlTransport::new("   ", "token")
            .expect_err("empty endpoint should fail");
        assert!(error
            .to_string()
            .contains("Linear GraphQL endpoint is empty"));
    }

    #[test]
    fn reqwest_transport_rejects_empty_api_key() {
        let error = ReqwestGraphqlTransport::new("https://api.linear.app/graphql", "   ")
            .expect_err("empty API key should fail");
        assert!(error.to_string().contains("LINEAR_API_KEY is empty"));
    }
}
