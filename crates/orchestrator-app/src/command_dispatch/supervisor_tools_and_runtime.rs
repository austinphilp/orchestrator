use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orchestrator_domain::{
    apply_workflow_transition, ArtifactId, ArtifactKind, ArtifactRecord, CoreError, EventStore,
    LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider, LlmResponseStream,
    LlmResponseSubscription, LlmRole, LlmStreamChunk, LlmTokenUsage, LlmTool, LlmToolCall,
    LlmToolCallOutput, LlmToolFunction, PullRequestRef, RepositoryRef, RetrievalScope,
    RuntimeMappingRecord, WorkItemId, WorkerSessionId, WorkflowGuardContext, WorkflowState,
    WorkflowTransitionReason,
};
use orchestrator_supervisor::{
    build_freeform_messages, build_template_messages_with_variables, SupervisorQueryEngine,
};
use orchestrator_ticketing::{
    AddTicketCommentRequest, GetTicketRequest, TicketAttachment, TicketId, TicketQuery,
    TicketingProvider, UpdateTicketDescriptionRequest, UpdateTicketStateRequest,
};
use orchestrator_vcs_repos::{default_system_url_opener, CodeHostProvider, UrlOpener};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use super::{
    open_event_store, open_owned_event_store, supervisor_chunk_event_flush_interval, AppEventStore,
    DatabaseRuntimeConfig, SupervisorRuntimeConfig,
};
use crate::controller::contracts::SupervisorCommandContext;
use crate::{
    commands::{
        ids as command_ids, resolve_supervisor_query_scope, Command, CommandRegistry,
        SupervisorQueryArgs, SupervisorQueryContextArgs,
    },
    events::{
        ArtifactCreatedPayload, NewEventEnvelope, OrchestrationEventPayload,
        SupervisorQueryCancellationSource, SupervisorQueryCancelledPayload,
        SupervisorQueryChunkPayload, SupervisorQueryFinishedPayload, SupervisorQueryKind,
        SupervisorQueryStartedPayload, WorkflowTransitionPayload,
    },
    normalization::DOMAIN_EVENT_SCHEMA_VERSION,
};

const MAX_TOOL_LOOP_ITERATIONS: usize = 8;
const TOOL_CALL_PROGRESS_PREFIX: &str = "[tool-call]";
const TOOL_RESULT_PREFIX: &str = "[tool-result]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum SupervisorTool {
    ListTickets,
    GetTicket,
    UpdateTicketState,
    UpdateTicketDescription,
    AddTicketComment,
}

impl SupervisorTool {
    fn name(&self) -> &'static str {
        match self {
            Self::ListTickets => "list_tickets",
            Self::GetTicket => "get_ticket",
            Self::UpdateTicketState => "update_ticket_state",
            Self::UpdateTicketDescription => "update_ticket_description",
            Self::AddTicketComment => "add_ticket_comment",
        }
    }

    fn from_name(raw: &str) -> Option<Self> {
        match raw {
            "list_tickets" => Some(Self::ListTickets),
            "get_ticket" => Some(Self::GetTicket),
            "update_ticket_state" => Some(Self::UpdateTicketState),
            "update_ticket_description" => Some(Self::UpdateTicketDescription),
            "add_ticket_comment" => Some(Self::AddTicketComment),
            _ => None,
        }
    }

    fn definition(&self) -> LlmTool {
        let function = match self {
            Self::ListTickets => LlmToolFunction {
                name: self.name().to_owned(),
                description: Some("List support tickets with optional filters".to_owned()),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "assigned_to_me": { "type": "boolean" },
                        "states": {
                            "type": "array",
                            "items": { "type": "string" },
                        },
                        "search": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1 },
                    },
                    "additionalProperties": false,
                }),
            },
            Self::GetTicket => LlmToolFunction {
                name: self.name().to_owned(),
                description: Some("Fetch a specific ticket by id".to_owned()),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "ticket_id": { "type": "string" },
                    },
                    "required": ["ticket_id"],
                    "additionalProperties": false,
                }),
            },
            Self::UpdateTicketState => LlmToolFunction {
                name: self.name().to_owned(),
                description: Some("Update a ticket's workflow/state value".to_owned()),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "ticket_id": { "type": "string" },
                        "state": { "type": "string" },
                    },
                    "required": ["ticket_id", "state"],
                    "additionalProperties": false,
                }),
            },
            Self::UpdateTicketDescription => LlmToolFunction {
                name: self.name().to_owned(),
                description: Some("Update a ticket description".to_owned()),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "ticket_id": { "type": "string" },
                        "description": { "type": "string" },
                    },
                    "required": ["ticket_id", "description"],
                    "additionalProperties": false,
                }),
            },
            Self::AddTicketComment => LlmToolFunction {
                name: self.name().to_owned(),
                description: Some(
                    "Attach a comment and optional attachments to a ticket".to_owned(),
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "ticket_id": { "type": "string" },
                        "comment": { "type": "string" },
                        "attachments": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": { "type": "string" },
                                    "url": { "type": "string" },
                                },
                                "required": ["url"],
                                "additionalProperties": false,
                            },
                        },
                    },
                    "required": ["ticket_id", "comment"],
                    "additionalProperties": false,
                }),
            },
        };

        LlmTool {
            tool_type: "function".to_owned(),
            function,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ListTicketsToolArgs {
    #[serde(default)]
    assigned_to_me: bool,
    #[serde(default)]
    states: Vec<String>,
    #[serde(default)]
    search: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GetTicketToolArgs {
    ticket_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateTicketStateToolArgs {
    ticket_id: String,
    state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateTicketDescriptionToolArgs {
    ticket_id: String,
    description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AddTicketCommentToolArgs {
    ticket_id: String,
    comment: String,
    #[serde(default)]
    attachments: Vec<ToolAttachmentPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolAttachmentPayload {
    #[serde(default)]
    label: Option<String>,
    url: String,
}

#[derive(Debug, Clone)]
struct ToolingStream {
    chunks: VecDeque<LlmStreamChunk>,
}

impl ToolingStream {
    fn new(chunks: Vec<LlmStreamChunk>) -> Self {
        Self {
            chunks: VecDeque::from(chunks),
        }
    }
}

#[async_trait::async_trait]
impl LlmResponseSubscription for ToolingStream {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
        Ok(self.chunks.pop_front())
    }
}

fn supervisor_tools() -> Vec<LlmTool> {
    vec![
        SupervisorTool::ListTickets.definition(),
        SupervisorTool::GetTicket.definition(),
        SupervisorTool::UpdateTicketState.definition(),
        SupervisorTool::UpdateTicketDescription.definition(),
        SupervisorTool::AddTicketComment.definition(),
    ]
}

fn tool_progress_chunk(call: &LlmToolCall) -> LlmStreamChunk {
    let arguments = if call.arguments.trim().is_empty() {
        "{}".to_owned()
    } else {
        call.arguments.to_owned()
    };

    LlmStreamChunk {
        delta: format!("{TOOL_CALL_PROGRESS_PREFIX} {} {}", call.name, arguments),
        tool_calls: Vec::new(),
        finish_reason: None,
        usage: None,
        rate_limit: None,
    }
}

fn tool_result_payload<T>(value: T) -> String
where
    T: Serialize,
{
    serde_json::to_string_pretty(&value).unwrap_or_else(|error| {
        serde_json::json!({
            "error": format!("failed to serialize tool output: {error}")
        })
        .to_string()
    })
}

struct SupervisorTurnState {
    chunks: Vec<LlmStreamChunk>,
    assistant_message: LlmMessage,
    finish_reason: LlmFinishReason,
}

async fn run_supervisor_turn<P>(
    supervisor: &P,
    supervisor_model: &str,
    messages: Vec<LlmMessage>,
) -> Result<SupervisorTurnState, CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let (stream_id, mut stream) = supervisor
        .stream_chat(LlmChatRequest {
            model: supervisor_model.to_owned(),
            messages,
            tools: supervisor_tools(),
            temperature: Some(0.2),
            tool_choice: None,
            max_output_tokens: Some(700),
        })
        .await?;

    drop(stream_id);

    let mut chunks = Vec::new();
    let mut assistant_content = String::new();
    let mut tool_calls: std::collections::HashMap<String, (String, String)> = HashMap::new();
    let mut call_order = Vec::new();
    let mut finish_reason = None;

    while let Some(chunk) = stream.next_chunk().await? {
        let mut chunk = chunk;

        if !chunk.delta.is_empty() {
            assistant_content.push_str(chunk.delta.as_str());
        }

        if !chunk.tool_calls.is_empty() {
            for call in &chunk.tool_calls {
                let entry = tool_calls.entry(call.id.clone()).or_insert_with(|| {
                    call_order.push(call.id.clone());
                    (call.name.clone(), String::new())
                });
                entry.0 = call.name.clone();
                entry.1.push_str(call.arguments.as_str());
            }
        }

        if matches!(chunk.finish_reason, Some(LlmFinishReason::ToolCall)) {
            chunk.finish_reason = None;
            chunks.push(chunk);
            finish_reason = Some(LlmFinishReason::ToolCall);
            break;
        }

        chunks.push(chunk.clone());
        if chunk.finish_reason.is_some() {
            finish_reason = chunk.finish_reason.clone();
            break;
        }
    }

    let finish_reason = finish_reason.unwrap_or(LlmFinishReason::Stop);
    let tool_calls = call_order
        .into_iter()
        .filter_map(|call_id| {
            let (name, arguments) = tool_calls.remove(&call_id)?;
            Some(LlmToolCall {
                id: call_id,
                name,
                arguments,
            })
        })
        .collect::<Vec<_>>();

    Ok(SupervisorTurnState {
        chunks,
        assistant_message: LlmMessage {
            role: LlmRole::Assistant,
            content: assistant_content,
            name: None,
            tool_calls,
            tool_call_id: None,
        },
        finish_reason,
    })
}

fn parse_tool_args<T>(tool_name: &str, raw: &str) -> Result<T, CoreError>
where
    T: DeserializeOwned,
{
    let payload = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(raw).map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "tool '{tool_name}' arguments are not valid JSON: {error}"
            ))
        })?
    };

    serde_json::from_value(payload).map_err(|error| {
        CoreError::DependencyUnavailable(format!(
            "tool '{tool_name}' arguments are malformed for expected schema: {error}"
        ))
    })
}

async fn execute_tool_call(
    ticketing: &dyn TicketingProvider,
    call: &LlmToolCall,
) -> Result<LlmToolCallOutput, CoreError> {
    let Some(tool) = SupervisorTool::from_name(call.name.as_str()) else {
        return Ok(LlmToolCallOutput {
            tool_call_id: call.id.clone(),
            output: tool_result_payload(serde_json::json!({
                "error": format!("unsupported tool '{}'; available: list_tickets, get_ticket, update_ticket_state, update_ticket_description, add_ticket_comment", call.name)
            })),
        });
    };

    let output = match tool {
        SupervisorTool::ListTickets => {
            let args: ListTicketsToolArgs =
                parse_tool_args("list_tickets", call.arguments.as_str())?;
            let tickets = ticketing
                .list_tickets(TicketQuery {
                    assigned_to_me: args.assigned_to_me,
                    states: args.states,
                    search: args.search,
                    limit: args.limit,
                })
                .await?;
            tool_result_payload(tickets)
        }
        SupervisorTool::GetTicket => {
            let args: GetTicketToolArgs = parse_tool_args("get_ticket", call.arguments.as_str())?;
            let ticket = ticketing
                .get_ticket(GetTicketRequest {
                    ticket_id: TicketId::from(args.ticket_id),
                })
                .await?;
            tool_result_payload(ticket)
        }
        SupervisorTool::UpdateTicketState => {
            let args: UpdateTicketStateToolArgs =
                parse_tool_args("update_ticket_state", call.arguments.as_str())?;
            ticketing
                .update_ticket_state(UpdateTicketStateRequest {
                    ticket_id: TicketId::from(args.ticket_id),
                    state: args.state,
                })
                .await?;
            tool_result_payload(serde_json::json!({ "ok": true }))
        }
        SupervisorTool::UpdateTicketDescription => {
            let args: UpdateTicketDescriptionToolArgs =
                parse_tool_args("update_ticket_description", call.arguments.as_str())?;
            ticketing
                .update_ticket_description(UpdateTicketDescriptionRequest {
                    ticket_id: TicketId::from(args.ticket_id),
                    description: args.description,
                })
                .await?;
            tool_result_payload(serde_json::json!({ "ok": true }))
        }
        SupervisorTool::AddTicketComment => {
            let args: AddTicketCommentToolArgs =
                parse_tool_args("add_ticket_comment", call.arguments.as_str())?;
            let attachments = args
                .attachments
                .into_iter()
                .map(|attachment| TicketAttachment {
                    label: attachment.label.unwrap_or_default(),
                    url: attachment.url,
                })
                .collect();

            ticketing
                .add_comment(AddTicketCommentRequest {
                    ticket_id: TicketId::from(args.ticket_id),
                    comment: args.comment,
                    attachments,
                })
                .await?;
            tool_result_payload(serde_json::json!({ "ok": true }))
        }
    };

    Ok(LlmToolCallOutput {
        tool_call_id: call.id.clone(),
        output,
    })
}

async fn execute_tool_calls(
    ticketing: &dyn TicketingProvider,
    calls: Vec<LlmToolCall>,
) -> Result<Vec<LlmToolCallOutput>, CoreError> {
    let mut outputs = Vec::new();
    for call in calls {
        match execute_tool_call(ticketing, &call).await {
            Ok(result) => outputs.push(result),
            Err(error) => {
                outputs.push(LlmToolCallOutput {
                    tool_call_id: call.id,
                    output: tool_result_payload(serde_json::json!({
                        "error": error.to_string()
                    })),
                });
            }
        }
    }
    Ok(outputs)
}

static SUPERVISOR_QUERY_EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);
static SUPERVISOR_QUERY_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static RUNTIME_COMMAND_STREAM_COUNTER: AtomicU64 = AtomicU64::new(1);

const SUPPORTED_RUNTIME_COMMAND_IDS: [&str; 5] = [
    command_ids::SUPERVISOR_QUERY,
    command_ids::WORKFLOW_APPROVE_PR_READY,
    command_ids::WORKFLOW_RECONCILE_PR_MERGE,
    command_ids::WORKFLOW_MERGE_PR,
    command_ids::GITHUB_OPEN_REVIEW_TABS,
];

const GITHUB_OPEN_REVIEW_TABS_ACK: &str =
    "github.open_review_tabs command accepted. This action opens the PR review URL and is idempotent when re-run.";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ActiveSupervisorQueryKey {
    event_store_path: String,
    stream_id: String,
}

#[derive(Debug, Clone)]
struct ActiveSupervisorQueryState {
    query_id: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    cancellation_requested_by_user: Arc<AtomicBool>,
}

fn active_supervisor_queries(
) -> &'static Mutex<HashMap<ActiveSupervisorQueryKey, ActiveSupervisorQueryState>> {
    static ACTIVE: OnceLock<Mutex<HashMap<ActiveSupervisorQueryKey, ActiveSupervisorQueryState>>> =
        OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn active_query_key(event_store_path: &str, stream_id: &str) -> ActiveSupervisorQueryKey {
    ActiveSupervisorQueryKey {
        event_store_path: event_store_path.to_owned(),
        stream_id: stream_id.to_owned(),
    }
}

fn register_active_supervisor_query(
    event_store_path: &str,
    stream_id: &str,
    state: ActiveSupervisorQueryState,
) {
    let key = active_query_key(event_store_path, stream_id);
    let mut active = active_supervisor_queries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    active.insert(key, state);
}

fn remove_active_supervisor_query_for_query(
    event_store_path: &str,
    stream_id: &str,
    query_id: &str,
) -> Option<ActiveSupervisorQueryState> {
    let key = active_query_key(event_store_path, stream_id);
    let mut active = active_supervisor_queries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let is_matching_query = active
        .get(&key)
        .is_some_and(|state| state.query_id.as_str() == query_id);
    if !is_matching_query {
        return None;
    }
    active.remove(&key)
}

fn mark_user_cancellation_requested(
    event_store_path: &str,
    stream_id: &str,
) -> Option<ActiveSupervisorQueryState> {
    let key = active_query_key(event_store_path, stream_id);
    let active = active_supervisor_queries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = active.get(&key)?.clone();
    if state
        .cancellation_requested_by_user
        .swap(true, Ordering::Relaxed)
    {
        return None;
    }

    Some(state)
}

fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", now.as_secs(), now.subsec_nanos())
}

fn next_event_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = SUPERVISOR_QUERY_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("evt-{prefix}-{now}-{count}")
}

fn next_supervisor_query_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = SUPERVISOR_QUERY_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("supq-{now}-{count}")
}

fn next_runtime_stream_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = RUNTIME_COMMAND_STREAM_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("runtime-{now}-{count}")
}

fn invalid_supervisor_runtime_usage(command_id: &str) -> CoreError {
    let supported = SUPPORTED_RUNTIME_COMMAND_IDS
        .iter()
        .map(|id| format!("'{}'", id))
        .collect::<Vec<_>>()
        .join(", ");

    CoreError::InvalidCommandArgs {
        command_id: command_id.to_owned(),
        reason: format!(
            "unsupported command '{command_id}' for supervisor runtime dispatch; expected one of {supported}"
        ),
    }
}

fn merge_supervisor_query_context(
    fallback_context: SupervisorCommandContext,
    invocation_context: Option<SupervisorQueryContextArgs>,
) -> SupervisorCommandContext {
    let Some(invocation_context) = invocation_context else {
        return fallback_context;
    };

    let mut merged = fallback_context;

    let invocation_scope = invocation_context.scope;
    let has_explicit_scope = invocation_scope.is_some();
    let invocation_session_id = invocation_context.selected_session_id;
    let has_invocation_session = invocation_session_id.is_some();
    let invocation_work_item_id = invocation_context.selected_work_item_id;

    if let Some(scope) = invocation_scope {
        if scope == "global" {
            merged.selected_work_item_id = None;
            merged.selected_session_id = None;
        }
        merged.scope = Some(scope);
    }

    if let Some(session_id) = invocation_session_id {
        if !has_explicit_scope {
            merged.scope = Some(format!("session:{session_id}"));
        }
        merged.selected_session_id = Some(session_id);
    }

    if let Some(work_item_id) = invocation_work_item_id {
        if !has_explicit_scope && !has_invocation_session {
            merged.scope = Some(format!("work_item:{work_item_id}"));
            merged.selected_session_id = None;
        }
        merged.selected_work_item_id = Some(work_item_id);
    }

    merged
}

fn args_context(args: &SupervisorQueryArgs) -> Option<SupervisorQueryContextArgs> {
    match args {
        SupervisorQueryArgs::Template { context, .. } => context.clone(),
        SupervisorQueryArgs::Freeform { context, .. } => context.clone(),
    }
}

fn scope_label(scope: &RetrievalScope) -> String {
    match scope {
        RetrievalScope::Global => "global".to_owned(),
        RetrievalScope::WorkItem(work_item_id) => format!("work_item:{}", work_item_id.as_str()),
        RetrievalScope::Session(session_id) => format!("session:{}", session_id.as_str()),
    }
}

fn event_scope_identifiers(
    scope: &RetrievalScope,
    context: &SupervisorCommandContext,
) -> (Option<WorkItemId>, Option<WorkerSessionId>) {
    let context_work_item_id = context
        .selected_work_item_id
        .as_ref()
        .map(|id| WorkItemId::from(id.clone()));
    let context_session_id = context
        .selected_session_id
        .as_ref()
        .map(|id| WorkerSessionId::from(id.clone()));

    match scope {
        RetrievalScope::Global => (None, None),
        RetrievalScope::WorkItem(work_item_id) => (Some(work_item_id.clone()), context_session_id),
        RetrievalScope::Session(session_id) => (context_work_item_id, Some(session_id.clone())),
    }
}

fn query_kind(args: &SupervisorQueryArgs) -> SupervisorQueryKind {
    match args {
        SupervisorQueryArgs::Template { .. } => SupervisorQueryKind::Template,
        SupervisorQueryArgs::Freeform { .. } => SupervisorQueryKind::Freeform,
    }
}

fn query_template(args: &SupervisorQueryArgs) -> Option<String> {
    match args {
        SupervisorQueryArgs::Template { template, .. } => Some(template.clone()),
        SupervisorQueryArgs::Freeform { .. } => None,
    }
}

struct RuntimeCommandResultStream {
    chunks: VecDeque<LlmStreamChunk>,
}

impl RuntimeCommandResultStream {
    fn new(message: String) -> Self {
        Self {
            chunks: VecDeque::from([LlmStreamChunk {
                delta: message,
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }]),
        }
    }
}

#[async_trait::async_trait]
impl LlmResponseSubscription for RuntimeCommandResultStream {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
        Ok(self.chunks.pop_front())
    }
}

fn runtime_command_response(message: &str) -> Result<(String, LlmResponseStream), CoreError> {
    Ok((
        next_runtime_stream_id(),
        Box::new(RuntimeCommandResultStream::new(message.to_owned())),
    ))
}

fn query_text(args: &SupervisorQueryArgs) -> Option<String> {
    match args {
        SupervisorQueryArgs::Template { .. } => None,
        SupervisorQueryArgs::Freeform { query, .. } => Some(query.clone()),
    }
}

fn parse_pull_request_number(url: &str) -> Result<u64, CoreError> {
    let normalized = normalize_pull_request_url(url);
    let segment = normalized.split("/pull/").nth(1).ok_or_else(|| {
        CoreError::Configuration(format!("Could not resolve PR number from URL `{url}`"))
    })?;

    let digits = segment
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Pull request URL `{url}` does not include a numeric PR number"
        )));
    }

    digits.parse::<u64>().map_err(|error| {
        CoreError::Configuration(format!(
            "Failed to parse PR number from URL `{url}`: {error}"
        ))
    })
}

fn parse_pull_request_repository(url: &str) -> Result<(String, String), CoreError> {
    let normalized = normalize_pull_request_url(url);
    let path = normalized
        .split_once("/pull/")
        .map(|(before, _)| before)
        .ok_or_else(|| CoreError::Configuration(format!("Invalid PR URL `{url}`")))?
        .split_once("://")
        .map(|(_, path)| path)
        .unwrap_or(normalized)
        .split_once('/')
        .ok_or_else(|| CoreError::Configuration(format!("Invalid PR URL `{url}`")))?
        .1
        .to_owned();

    let mut parts = path.split('/').filter(|value| !value.is_empty());
    let owner = parts
        .next()
        .ok_or_else(|| CoreError::Configuration(format!("Invalid PR URL `{url}`")))?;
    let name = parts
        .next()
        .ok_or_else(|| CoreError::Configuration(format!("Invalid PR URL `{url}`")))?;

    let repository = format!("{owner}/{name}");
    Ok((repository.clone(), repository))
}
