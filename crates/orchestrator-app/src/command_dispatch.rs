use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use orchestrator_core::{
    apply_workflow_transition, command_ids, resolve_supervisor_query_scope,
    AddTicketCommentRequest, ArtifactCreatedPayload, ArtifactId, ArtifactKind, ArtifactRecord,
    CodeHostProvider, Command, CommandRegistry, CoreError, EventStore, GetTicketRequest,
    LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider, LlmResponseStream,
    LlmResponseSubscription, LlmRole, LlmStreamChunk, LlmTokenUsage, LlmTool, LlmToolCall,
    LlmToolCallOutput, LlmToolFunction, NewEventEnvelope, OrchestrationEventPayload,
    PullRequestRef, RepositoryRef, RetrievalScope, RuntimeMappingRecord, SqliteEventStore,
    SupervisorQueryArgs, SupervisorQueryCancellationSource, SupervisorQueryCancelledPayload,
    SupervisorQueryChunkPayload, SupervisorQueryContextArgs, SupervisorQueryFinishedPayload,
    SupervisorQueryKind, SupervisorQueryStartedPayload, TicketAttachment, TicketId, TicketQuery,
    TicketingProvider, UntypedCommandInvocation, UpdateTicketDescriptionRequest,
    UpdateTicketStateRequest, UrlOpener, WorkItemId, WorkerSessionId, WorkflowGuardContext,
    WorkflowState, WorkflowTransitionPayload, WorkflowTransitionReason,
    DOMAIN_EVENT_SCHEMA_VERSION,
};
use orchestrator_github::default_system_url_opener;
use orchestrator_supervisor::{
    build_freeform_messages, build_template_messages_with_variables, SupervisorQueryEngine,
};
use orchestrator_ui::SupervisorCommandContext;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::{open_event_store, supervisor_model_from_env};

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
    messages: Vec<LlmMessage>,
) -> Result<SupervisorTurnState, CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let (stream_id, mut stream) = supervisor
        .stream_chat(LlmChatRequest {
            model: supervisor_model_from_env(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::test_support::TestDbPath;
    use orchestrator_core::{
        ArtifactCreatedPayload, ArtifactId, ArtifactKind, ArtifactRecord, BackendKind,
        CodeHostKind, PullRequestMergeState, PullRequestRef, PullRequestSummary, RepositoryRef,
        ReviewerRequest, RuntimeMappingRecord, SessionRecord, TicketId, TicketProvider,
        TicketRecord, WorkItemId, WorkerSessionId, WorkerSessionStatus, WorktreeId, WorktreeRecord,
    };
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn parse_pull_request_number_supports_query_and_fragment_suffix() {
        let number =
            parse_pull_request_number("https://github.com/acme/repo/pull/123?draft=true#section")
                .expect("parse numeric suffix");
        assert_eq!(number, 123);
    }

    #[test]
    fn parse_pull_request_number_rejects_missing_pull_segment() {
        let err = parse_pull_request_number("https://github.com/acme/repo/issues/123")
            .expect_err("missing /pull");
        assert!(matches!(err, CoreError::Configuration { .. }));
    }

    #[test]
    fn parse_pull_request_repository_parses_without_scheme() {
        let (name, id) = parse_pull_request_repository("github.com/acme/repo/pull/777")
            .expect("parse repo without scheme");
        assert_eq!(name, "acme/repo");
        assert_eq!(id, "acme/repo");
    }

    #[test]
    fn parse_pull_request_repository_rejects_short_path() {
        let err = parse_pull_request_repository("https://github.com/pull/777")
            .expect_err("repository name missing");
        assert!(matches!(err, CoreError::Configuration { .. }));
    }

    #[test]
    fn parse_pull_request_repository_trims_surrounding_punctuation() {
        let (name, id) =
            parse_pull_request_repository("(\"https://github.com/acme/repo/pull/777\",)")
                .expect("parse url with punctuation");
        assert_eq!(name, "acme/repo");
        assert_eq!(id, "acme/repo");
    }

    #[test]
    fn sanitize_error_display_text_strips_control_characters() {
        let sanitized = sanitize_error_display_text("workflow merge\0 failed\u{2400}\r\n");
        assert_eq!(sanitized, "workflow merge failed\n\n");
    }

    struct MockUrlOpener {
        calls: Mutex<Vec<String>>,
    }

    impl MockUrlOpener {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock calls").clone()
        }
    }

    impl Default for MockUrlOpener {
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl UrlOpener for MockUrlOpener {
        async fn open_url(&self, url: &str) -> Result<(), CoreError> {
            self.calls.lock().expect("lock calls").push(url.to_owned());
            Ok(())
        }
    }

    struct FailingUrlOpener;

    #[async_trait::async_trait]
    impl UrlOpener for FailingUrlOpener {
        async fn open_url(&self, _url: &str) -> Result<(), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "open failed intentionally".to_owned(),
            ))
        }
    }

    struct MockCodeHost {
        fallback_pr: Option<PullRequestRef>,
        fallback_calls: Mutex<Vec<(String, String)>>,
    }

    impl MockCodeHost {
        fn new(fallback_pr: Option<PullRequestRef>) -> Self {
            Self {
                fallback_pr,
                fallback_calls: Mutex::new(Vec::new()),
            }
        }

        fn fallback_calls(&self) -> Vec<(String, String)> {
            self.fallback_calls
                .lock()
                .expect("fallback calls lock")
                .clone()
        }
    }

    #[async_trait::async_trait]
    impl CodeHostProvider for MockCodeHost {
        fn kind(&self) -> CodeHostKind {
            CodeHostKind::Github
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn create_draft_pull_request(
            &self,
            _request: orchestrator_core::CreatePullRequestRequest,
        ) -> Result<PullRequestSummary, CoreError> {
            Err(CoreError::DependencyUnavailable(
                "not implemented in test mock".to_owned(),
            ))
        }

        async fn mark_ready_for_review(&self, _pr: &PullRequestRef) -> Result<(), CoreError> {
            Ok(())
        }

        async fn request_reviewers(
            &self,
            _pr: &PullRequestRef,
            _reviewers: ReviewerRequest,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn list_waiting_for_my_review(&self) -> Result<Vec<PullRequestSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn get_pull_request_merge_state(
            &self,
            _pr: &PullRequestRef,
        ) -> Result<PullRequestMergeState, CoreError> {
            Ok(PullRequestMergeState {
                merged: false,
                is_draft: true,
                merge_conflict: false,
                base_branch: None,
                head_branch: None,
            })
        }

        async fn merge_pull_request(&self, _pr: &PullRequestRef) -> Result<(), CoreError> {
            Ok(())
        }

        async fn find_open_pull_request_for_branch(
            &self,
            repository: &RepositoryRef,
            head_branch: &str,
        ) -> Result<Option<PullRequestRef>, CoreError> {
            self.fallback_calls
                .lock()
                .expect("fallback calls lock")
                .push((
                    repository.root.to_string_lossy().to_string(),
                    head_branch.to_owned(),
                ));
            Ok(self.fallback_pr.clone())
        }
    }

    async fn execute_github_open_review_tabs_with_opener(
        event_store_path: &str,
        context: SupervisorCommandContext,
        url_opener: &impl UrlOpener,
    ) -> Result<(String, LlmResponseStream), CoreError> {
        let code_host = MockCodeHost::new(None);
        super::execute_github_open_review_tabs_with_opener(
            &code_host,
            event_store_path,
            context,
            url_opener,
        )
        .await
    }

    fn seed_runtime_mapping(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
    ) -> Result<(), CoreError> {
        let ticket = TicketRecord {
            ticket_id: TicketId::from_provider_uuid(
                TicketProvider::Linear,
                format!("provider-{}", work_item_id.as_str()),
            ),
            provider: TicketProvider::Linear,
            provider_ticket_id: format!("provider-{}", work_item_id.as_str()),
            identifier: format!("ORCH-{}", work_item_id.as_str()),
            title: "Open review test".to_owned(),
            state: "In Progress".to_owned(),
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        };

        let worktree_path =
            std::path::PathBuf::from(format!("/workspace/{}", work_item_id.as_str()));
        let runtime = RuntimeMappingRecord {
            ticket,
            work_item_id: work_item_id.clone(),
            worktree: WorktreeRecord {
                worktree_id: WorktreeId::new(format!("wt-{}", work_item_id.as_str())),
                work_item_id: work_item_id.clone(),
                path: worktree_path.to_string_lossy().to_string(),
                branch: "feature/open-review-tabs".to_owned(),
                base_branch: "main".to_owned(),
                created_at: "2026-02-16T11:00:00Z".to_owned(),
            },
            session: SessionRecord {
                session_id: WorkerSessionId::new(session_id),
                work_item_id: work_item_id.clone(),
                backend_kind: BackendKind::OpenCode,
                workdir: worktree_path.to_string_lossy().to_string(),
                model: Some("gpt-5".to_owned()),
                status: WorkerSessionStatus::Running,
                created_at: "2026-02-16T11:00:00Z".to_owned(),
                updated_at: "2026-02-16T11:01:00Z".to_owned(),
            },
        };
        store.upsert_runtime_mapping(&runtime)?;
        Ok(())
    }

    fn seed_runtime_mapping_with_artifact(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
        artifact_id: &str,
        kind: ArtifactKind,
        pr_url: &str,
    ) -> Result<(), CoreError> {
        seed_runtime_mapping(store, work_item_id, session_id)?;
        let artifact = ArtifactRecord {
            artifact_id: ArtifactId::new(artifact_id),
            work_item_id: work_item_id.clone(),
            kind: kind.clone(),
            metadata: json!({"type": "pull_request"}),
            storage_ref: pr_url.to_owned(),
            created_at: "2026-02-16T11:01:00Z".to_owned(),
        };
        store.create_artifact(&artifact)?;
        let event = OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
            artifact_id: artifact.artifact_id.clone(),
            work_item_id: work_item_id.clone(),
            kind,
            label: "Pull request".to_owned(),
            uri: pr_url.to_owned(),
        });
        store.append_event(
            NewEventEnvelope {
                event_id: format!("evt-pr-artifact-{}", artifact_id),
                occurred_at: "2026-02-16T11:02:00Z".to_owned(),
                work_item_id: Some(work_item_id.clone()),
                session_id: Some(WorkerSessionId::new(session_id)),
                payload: event,
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            },
            &[artifact.artifact_id.clone()],
        )?;
        Ok(())
    }

    fn seed_runtime_mapping_with_pr_artifact(
        store: &mut SqliteEventStore,
        work_item_id: &WorkItemId,
        session_id: &str,
        artifact_id: &str,
        pr_url: &str,
    ) -> Result<(), CoreError> {
        seed_runtime_mapping_with_artifact(
            store,
            work_item_id,
            session_id,
            artifact_id,
            ArtifactKind::PR,
            pr_url,
        )
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_opens_pr_url_for_session_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-session");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-session");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-session",
            "artifact-pr-review-session",
            "https://github.com/acme/repo/pull/123",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: None,
                selected_session_id: Some("sess-open-review-tabs-session".to_owned()),
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk.delta.contains("github.open_review_tabs"));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/123"));
        assert!(stream
            .next_chunk()
            .await
            .expect("read runtime close")
            .is_none());
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/123".to_owned()]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_opens_pr_url_for_work_item_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-work-item");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-work-item");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-work-item",
            "artifact-pr-review-work-item",
            "https://github.com/acme/repo/pull/124",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/124"));
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/124".to_owned()]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_accepts_pr_link_artifacts() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-link-artifact");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-link-artifact");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-link-artifact",
            "artifact-pr-link-review",
            ArtifactKind::Link,
            "https://github.com/acme/repo/pull/321",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/321"));
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/321".to_owned()]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_reopens_url_per_invocation() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-reopen");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-reopen");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-reopen",
            "artifact-pr-review-reopen",
            "https://github.com/acme/repo/pull/125",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let context = SupervisorCommandContext {
            selected_work_item_id: Some(work_item_id.as_str().to_owned()),
            selected_session_id: None,
            scope: None,
        };
        let event_store_path = temp_db.path().to_str().expect("path").to_owned();

        let _ = execute_github_open_review_tabs_with_opener(
            &event_store_path,
            context.clone(),
            &opener,
        )
        .await
        .expect("first invocation");
        let _ = execute_github_open_review_tabs_with_opener(
            &event_store_path,
            context.clone(),
            &opener,
        )
        .await
        .expect("second invocation");

        assert_eq!(
            opener.calls(),
            vec![
                "https://github.com/acme/repo/pull/125".to_owned(),
                "https://github.com/acme/repo/pull/125".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_rejects_missing_context() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-missing-context");
        let opener = MockUrlOpener::default();

        let err = match execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext::default(),
            &opener,
        )
        .await
        {
            Ok(_) => panic!("missing context should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
        assert!(
            message.contains("requires an active runtime session or selected work item context")
        );
        assert!(opener.calls().is_empty());
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_rejects_missing_pr_artifact() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-missing-pr");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-missing-pr");
        seed_runtime_mapping(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-missing-pr",
        )
        .expect("seed mapping");

        let opener = MockUrlOpener::default();
        let err = match execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        {
            Ok(_) => panic!("missing artifact should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
        assert!(message.contains("could not resolve a PR artifact"));
        assert!(opener.calls().is_empty());
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_propagates_opener_errors() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-opener-error");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-opener-error");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping_with_pr_artifact(
            &mut store,
            &work_item_id,
            "sess-open-review-tabs-opener-error",
            "artifact-pr-review-opener-error",
            "https://github.com/acme/repo/pull/126",
        )
        .expect("seed mapping");

        let opener = FailingUrlOpener;
        let err = match execute_github_open_review_tabs_with_opener(
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        {
            Ok(_) => panic!("opener failure should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains("open failed intentionally"));
        assert!(message.contains(command_ids::GITHUB_OPEN_REVIEW_TABS));
    }

    #[tokio::test]
    async fn execute_github_open_review_tabs_lazy_resolves_pr_via_code_host_fallback() {
        let temp_db = TestDbPath::new("app-runtime-command-open-review-tabs-fallback");
        let work_item_id = WorkItemId::new("wi-open-review-tabs-fallback");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, "sess-open-review-tabs-fallback")
            .expect("seed mapping");

        let code_host = MockCodeHost::new(Some(PullRequestRef {
            repository: RepositoryRef {
                id: "acme/repo".to_owned(),
                name: "acme/repo".to_owned(),
                root: std::path::PathBuf::from("/workspace/wi-open-review-tabs-fallback"),
            },
            number: 333,
            url: "https://github.com/acme/repo/pull/333".to_owned(),
        }));
        let opener = MockUrlOpener::default();
        let (stream_id, mut stream) = super::execute_github_open_review_tabs_with_opener(
            &code_host,
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            &opener,
        )
        .await
        .expect("open review tabs");

        assert!(stream_id.starts_with("runtime-"));
        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains("https://github.com/acme/repo/pull/333"));
        assert_eq!(
            opener.calls(),
            vec!["https://github.com/acme/repo/pull/333".to_owned()]
        );
        assert_eq!(code_host.fallback_calls().len(), 1);

        let persisted_store = SqliteEventStore::open(temp_db.path()).expect("reopen store");
        let runtime = resolve_runtime_mapping_for_context_with_command(
            &persisted_store,
            &SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            command_ids::GITHUB_OPEN_REVIEW_TABS,
        )
        .expect("runtime mapping");
        let resolved = resolve_pull_request_for_mapping_with_command(
            &persisted_store,
            &runtime,
            command_ids::GITHUB_OPEN_REVIEW_TABS,
        )
        .expect("fallback should persist PR artifact");
        assert_eq!(resolved.number, 333);
    }

    #[tokio::test]
    async fn execute_workflow_reconcile_pr_merge_lazy_resolves_pr_via_code_host_fallback() {
        let temp_db = TestDbPath::new("app-runtime-command-reconcile-pr-fallback");
        let work_item_id = WorkItemId::new("wi-reconcile-pr-fallback");
        let mut store = SqliteEventStore::open(temp_db.path()).expect("open store");
        seed_runtime_mapping(&mut store, &work_item_id, "sess-reconcile-pr-fallback")
            .expect("seed mapping");

        let code_host = MockCodeHost::new(Some(PullRequestRef {
            repository: RepositoryRef {
                id: "acme/repo".to_owned(),
                name: "acme/repo".to_owned(),
                root: std::path::PathBuf::from("/workspace/wi-reconcile-pr-fallback"),
            },
            number: 444,
            url: "https://github.com/acme/repo/pull/444".to_owned(),
        }));

        let (_stream_id, mut stream) = execute_workflow_reconcile_pr_merge(
            &code_host,
            temp_db.path().to_str().expect("path"),
            SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
        )
        .await
        .expect("reconcile should succeed with fallback");

        let first_chunk = stream
            .next_chunk()
            .await
            .expect("read runtime chunk")
            .expect("expected runtime message");
        assert_eq!(first_chunk.finish_reason, Some(LlmFinishReason::Stop));
        assert!(first_chunk
            .delta
            .contains(command_ids::WORKFLOW_RECONCILE_PR_MERGE));
        assert_eq!(code_host.fallback_calls().len(), 1);

        let persisted_store = SqliteEventStore::open(temp_db.path()).expect("reopen store");
        let runtime = resolve_runtime_mapping_for_context_with_command(
            &persisted_store,
            &SupervisorCommandContext {
                selected_work_item_id: Some(work_item_id.as_str().to_owned()),
                selected_session_id: None,
                scope: None,
            },
            command_ids::WORKFLOW_RECONCILE_PR_MERGE,
        )
        .expect("runtime mapping");
        let resolved = resolve_pull_request_for_mapping_with_command(
            &persisted_store,
            &runtime,
            command_ids::WORKFLOW_RECONCILE_PR_MERGE,
        )
        .expect("fallback should persist PR artifact");
        assert_eq!(resolved.number, 444);
    }
}

fn normalize_pull_request_url(url: &str) -> &str {
    url.trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '(' || ch == ')')
        .trim_end_matches(|ch: char| ch == ',' || ch == ';' || ch == '.')
}

fn sanitize_error_display_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\t' => output.push(ch),
            '\r' => output.push('\n'),
            '\u{2400}' => {}
            _ if ch.is_control() => {}
            _ => output.push(ch),
        }
    }
    output
}

fn resolve_runtime_mapping_for_context(
    store: &SqliteEventStore,
    context: &SupervisorCommandContext,
) -> Result<RuntimeMappingRecord, CoreError> {
    resolve_runtime_mapping_for_context_with_command(
        store,
        context,
        command_ids::WORKFLOW_APPROVE_PR_READY,
    )
}

fn resolve_runtime_mapping_for_context_with_command(
    store: &SqliteEventStore,
    context: &SupervisorCommandContext,
    command_id: &str,
) -> Result<RuntimeMappingRecord, CoreError> {
    let mappings = store.list_runtime_mappings()?;

    if let Some(session_id) = context.selected_session_id.as_deref() {
        if let Some(mapping) = mappings
            .iter()
            .find(|mapping| mapping.session.session_id.as_str() == session_id)
        {
            return Ok(mapping.clone());
        }

        return Err(CoreError::Configuration(format!(
            "{command_id} could not resolve runtime mapping for selected session '{session_id}'"
        )));
    }

    if let Some(work_item_id) = context.selected_work_item_id.as_deref() {
        if let Some(mapping) = mappings
            .iter()
            .find(|mapping| mapping.work_item_id.as_str() == work_item_id)
        {
            return Ok(mapping.clone());
        }

        return Err(CoreError::Configuration(format!(
            "{command_id} could not resolve runtime mapping for selected work item '{work_item_id}'"
        )));
    }

    Err(CoreError::Configuration(
        format!("{command_id} requires an active runtime session or selected work item context")
            .to_owned(),
    ))
}

fn resolve_pull_request_for_mapping_with_command(
    store: &SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    command_id: &str,
) -> Result<PullRequestRef, CoreError> {
    let events = store.read_event_with_artifacts(&mapping.work_item_id)?;
    let mut candidate_parse_error: Option<CoreError> = None;

    for stored in events.iter().rev() {
        for artifact_id in stored.artifact_ids.iter().rev() {
            let Some(artifact) = store.get_artifact(artifact_id)? else {
                continue;
            };

            if !is_pull_request_artifact_candidate(&artifact.kind, artifact.storage_ref.as_str()) {
                continue;
            }

            let url = artifact.storage_ref.trim();
            if url.is_empty() {
                continue;
            }

            let number = match parse_pull_request_number(url) {
                Ok(number) => number,
                Err(error) => {
                    candidate_parse_error = Some(error);
                    continue;
                }
            };
            let (repository_name, repository_id) = match parse_pull_request_repository(url) {
                Ok(repository) => repository,
                Err(error) => {
                    candidate_parse_error = Some(error);
                    continue;
                }
            };
            return Ok(PullRequestRef {
                repository: RepositoryRef {
                    id: repository_id,
                    name: repository_name,
                    root: PathBuf::from(mapping.worktree.path.clone()),
                },
                number,
                url: url.to_owned(),
            });
        }
    }

    if let Some(candidate_error) = candidate_parse_error {
        return Err(CoreError::Configuration(format!(
            "{command_id} could not resolve a usable PR artifact for work item '{}': {candidate_error}",
            mapping.work_item_id.as_str(),
        )));
    }

    Err(CoreError::Configuration(format!(
        "{command_id} could not resolve a PR artifact for work item '{}'",
        mapping.work_item_id.as_str()
    )))
}

fn repository_ref_for_runtime_mapping(mapping: &RuntimeMappingRecord) -> RepositoryRef {
    let root = PathBuf::from(mapping.worktree.path.clone());
    let inferred_name = root
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    RepositoryRef {
        id: inferred_name.to_owned(),
        name: inferred_name.to_owned(),
        root,
    }
}

fn persist_pull_request_artifact_from_vcs_fallback(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    command_id: &str,
    pr: &PullRequestRef,
) -> Result<(), CoreError> {
    let occurred_at = now_timestamp();
    let artifact_id = ArtifactId::new(next_event_id("artifact-pr-vcs-fallback"));
    let artifact = ArtifactRecord {
        artifact_id: artifact_id.clone(),
        work_item_id: mapping.work_item_id.clone(),
        kind: ArtifactKind::PR,
        metadata: json!({
            "source": "vcs_fallback",
            "command_id": command_id,
            "branch": mapping.worktree.branch.as_str(),
        }),
        storage_ref: pr.url.clone(),
        created_at: occurred_at.clone(),
    };
    store.create_artifact(&artifact)?;
    store.append_event(
        NewEventEnvelope {
            event_id: next_event_id("artifact-created-pr-vcs-fallback"),
            occurred_at,
            work_item_id: Some(mapping.work_item_id.clone()),
            session_id: Some(mapping.session.session_id.clone()),
            payload: OrchestrationEventPayload::ArtifactCreated(ArtifactCreatedPayload {
                artifact_id: artifact_id.clone(),
                work_item_id: mapping.work_item_id.clone(),
                kind: ArtifactKind::PR,
                label: format!("Pull request #{} (vcs fallback)", pr.number),
                uri: pr.url.clone(),
            }),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        },
        &[artifact_id],
    )?;
    Ok(())
}

async fn resolve_pull_request_for_mapping_with_vcs_fallback<C>(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    command_id: &str,
    code_host: &C,
) -> Result<PullRequestRef, CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    match resolve_pull_request_for_mapping_with_command(store, mapping, command_id) {
        Ok(pr) => return Ok(pr),
        Err(CoreError::Configuration(_)) => {}
        Err(error) => return Err(error),
    }

    let repository = repository_ref_for_runtime_mapping(mapping);
    let fallback = code_host
        .find_open_pull_request_for_branch(&repository, mapping.worktree.branch.as_str())
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "{command_id} failed to resolve PR via VCS fallback for work item '{}': {detail}",
                mapping.work_item_id.as_str(),
            ))
        })?;

    let Some(pr) = fallback else {
        return resolve_pull_request_for_mapping_with_command(store, mapping, command_id);
    };
    persist_pull_request_artifact_from_vcs_fallback(store, mapping, command_id, &pr)?;
    Ok(pr)
}

fn is_pull_request_artifact_candidate(kind: &ArtifactKind, url: &str) -> bool {
    matches!(kind, ArtifactKind::PR)
        || (matches!(kind, ArtifactKind::Link) && looks_like_pull_request_url(url))
}

fn looks_like_pull_request_url(url: &str) -> bool {
    let normalized = normalize_pull_request_url(url).to_ascii_lowercase();
    normalized.contains("/pull/")
        || normalized.contains("pullrequest")
        || normalized.contains("pull-request")
}

async fn execute_workflow_approve_pr_ready<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let command_id = command_ids::WORKFLOW_APPROVE_PR_READY;
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context(&store, &context)?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store, &runtime, command_id, code_host,
    )
    .await?;

    code_host
        .mark_ready_for_review(&pr)
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "workflow.approve_pr_ready failed for PR #{}: {detail}",
                pr.number
            ))
        })?;

    runtime_command_response(&format!(
        "workflow.approve_pr_ready command completed for PR #{}",
        pr.number
    ))
}

fn latest_workflow_state_for_work_item(
    store: &SqliteEventStore,
    work_item_id: &WorkItemId,
) -> Result<WorkflowState, CoreError> {
    let mut current = WorkflowState::New;
    let events = store.read_ordered()?;
    for event in events {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }
        if let OrchestrationEventPayload::WorkflowTransition(payload) = event.payload {
            current = payload.to;
        }
    }
    Ok(current)
}

fn append_workflow_transition_event_for_runtime(
    store: &mut SqliteEventStore,
    mapping: &RuntimeMappingRecord,
    from: &WorkflowState,
    to: &WorkflowState,
    reason: WorkflowTransitionReason,
    guards: &WorkflowGuardContext,
    command_id: &str,
) -> Result<WorkflowState, CoreError> {
    let next = apply_workflow_transition(from, to, &reason, guards).map_err(|error| {
        CoreError::InvalidCommandArgs {
            command_id: command_id.to_owned(),
            reason: format!(
                "workflow transition validation failed for work item '{}': {error}",
                mapping.work_item_id.as_str()
            ),
        }
    })?;
    let session_id = mapping.session.session_id.clone();
    store.append(NewEventEnvelope {
        event_id: next_event_id("workflow-transition"),
        occurred_at: now_timestamp(),
        work_item_id: Some(mapping.work_item_id.clone()),
        session_id: Some(session_id),
        payload: OrchestrationEventPayload::WorkflowTransition(WorkflowTransitionPayload {
            work_item_id: mapping.work_item_id.clone(),
            from: from.clone(),
            to: to.clone(),
            reason: Some(reason),
        }),
        schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
    })?;
    Ok(next)
}

async fn execute_workflow_reconcile_pr_merge<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let command_id = command_ids::WORKFLOW_RECONCILE_PR_MERGE;
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context_with_command(&store, &context, command_id)?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store, &runtime, command_id, code_host,
    )
    .await?;

    let merge_state = code_host
        .get_pull_request_merge_state(&pr)
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "{command_id} failed for PR #{}: {detail}",
                pr.number
            ))
        })?;

    if !merge_state.merged {
        let message = json!({
            "command": command_id,
            "work_item_id": runtime.work_item_id.as_str(),
            "merged": false,
            "merge_conflict": merge_state.merge_conflict,
            "base_branch": merge_state.base_branch,
            "head_branch": merge_state.head_branch,
            "completed": false,
            "transitions": []
        })
        .to_string();
        return runtime_command_response(message.as_str());
    }

    let mut transitions = Vec::new();
    let mut current = latest_workflow_state_for_work_item(&store, &runtime.work_item_id)?;

    loop {
        if current == WorkflowState::Done {
            break;
        }

        if current == WorkflowState::ReadyForReview {
            let next = append_workflow_transition_event_for_runtime(
                &mut store,
                &runtime,
                &current,
                &WorkflowState::InReview,
                WorkflowTransitionReason::ReviewStarted,
                &WorkflowGuardContext::default(),
                command_id,
            )?;
            transitions.push("ReadyForReview->InReview".to_owned());
            current = next;
            continue;
        }

        if current == WorkflowState::InReview {
            let next = append_workflow_transition_event_for_runtime(
                &mut store,
                &runtime,
                &current,
                &WorkflowState::Done,
                WorkflowTransitionReason::ReviewApprovedAndMerged,
                &WorkflowGuardContext {
                    merge_completed: true,
                    ..WorkflowGuardContext::default()
                },
                command_id,
            )?;
            transitions.push("InReview->Done".to_owned());
            current = next;
            continue;
        }

        break;
    }

    let message = json!({
        "command": command_id,
        "work_item_id": runtime.work_item_id.as_str(),
        "merged": merge_state.merged,
        "merge_conflict": merge_state.merge_conflict,
        "base_branch": merge_state.base_branch,
        "head_branch": merge_state.head_branch,
        "completed": current == WorkflowState::Done,
        "transitions": transitions
    })
    .to_string();

    runtime_command_response(message.as_str())
}

async fn execute_workflow_merge_pr<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let command_id = command_ids::WORKFLOW_MERGE_PR;
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context_with_command(&store, &context, command_id)?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store, &runtime, command_id, code_host,
    )
    .await?;

    code_host.merge_pull_request(&pr).await.map_err(|error| {
        let detail = sanitize_error_display_text(error.to_string().as_str());
        CoreError::DependencyUnavailable(format!(
            "{command_id} failed for PR #{}: {detail}",
            pr.number
        ))
    })?;

    execute_workflow_reconcile_pr_merge(code_host, event_store_path, context).await
}

fn append_supervisor_event(
    event_store_path: &str,
    event_id: String,
    occurred_at: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    payload: OrchestrationEventPayload,
) -> Result<(), CoreError> {
    let mut store = open_event_store(event_store_path)?;
    store.append(NewEventEnvelope {
        event_id,
        occurred_at,
        work_item_id,
        session_id,
        payload,
        schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
    })?;
    Ok(())
}

fn append_supervisor_event_best_effort(
    event_store_path: &str,
    event_id: String,
    occurred_at: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    payload: OrchestrationEventPayload,
) {
    if let Err(error) = append_supervisor_event(
        event_store_path,
        event_id,
        occurred_at,
        work_item_id,
        session_id,
        payload,
    ) {
        warn!(
            event_store_path,
            "failed to append supervisor lifecycle event: {error}"
        );
    }
}

struct SupervisorLifecycleRecordingStream {
    event_store_path: String,
    work_item_id: Option<WorkItemId>,
    session_id: Option<WorkerSessionId>,
    query_id: String,
    stream_id: String,
    started_at: String,
    started_time: SystemTime,
    cancellation_requested_by_user: Arc<AtomicBool>,
    chunk_count: u32,
    output_chars: u32,
    latest_usage: Option<LlmTokenUsage>,
    finished: bool,
    inner: LlmResponseStream,
}

impl SupervisorLifecycleRecordingStream {
    fn new(
        event_store_path: String,
        work_item_id: Option<WorkItemId>,
        session_id: Option<WorkerSessionId>,
        query_id: String,
        stream_id: String,
        started_at: String,
        started_time: SystemTime,
        cancellation_requested_by_user: Arc<AtomicBool>,
        inner: LlmResponseStream,
    ) -> Self {
        Self {
            event_store_path,
            work_item_id,
            session_id,
            query_id,
            stream_id,
            started_at,
            started_time,
            cancellation_requested_by_user,
            chunk_count: 0,
            output_chars: 0,
            latest_usage: None,
            finished: false,
            inner,
        }
    }

    fn observe_chunk(&mut self, chunk: &LlmStreamChunk) {
        let delta_chars = u32::try_from(chunk.delta.chars().count()).unwrap_or(u32::MAX);
        self.chunk_count = self.chunk_count.saturating_add(1);
        self.output_chars = self.output_chars.saturating_add(delta_chars);
        if let Some(usage) = chunk.usage.as_ref() {
            self.latest_usage = Some(usage.clone());
        }

        let observed_at = now_timestamp();
        append_supervisor_event_best_effort(
            self.event_store_path.as_str(),
            next_event_id("supervisor-query-chunk"),
            observed_at.clone(),
            self.work_item_id.clone(),
            self.session_id.clone(),
            OrchestrationEventPayload::SupervisorQueryChunk(SupervisorQueryChunkPayload {
                query_id: self.query_id.clone(),
                stream_id: self.stream_id.clone(),
                chunk_index: self.chunk_count,
                observed_at,
                delta_chars,
                cumulative_output_chars: self.output_chars,
                usage: chunk.usage.clone(),
                finish_reason: chunk.finish_reason.clone(),
            }),
        );
    }

    fn finalize(
        &mut self,
        reason: LlmFinishReason,
        usage: Option<LlmTokenUsage>,
        error: Option<String>,
    ) {
        if self.finished {
            return;
        }

        self.finished = true;
        let finished_at = now_timestamp();
        let duration_ms = SystemTime::now()
            .duration_since(self.started_time)
            .unwrap_or_default()
            .as_millis();
        let duration_ms = u64::try_from(duration_ms).unwrap_or(u64::MAX);
        let final_usage = usage.or_else(|| self.latest_usage.clone());

        let cancellation_source = if reason == LlmFinishReason::Cancelled {
            if self.cancellation_requested_by_user.load(Ordering::Relaxed) {
                Some(SupervisorQueryCancellationSource::UserInitiated)
            } else {
                Some(SupervisorQueryCancellationSource::Runtime)
            }
        } else {
            None
        };

        append_supervisor_event_best_effort(
            self.event_store_path.as_str(),
            next_event_id("supervisor-query-finished"),
            finished_at.clone(),
            self.work_item_id.clone(),
            self.session_id.clone(),
            OrchestrationEventPayload::SupervisorQueryFinished(SupervisorQueryFinishedPayload {
                query_id: self.query_id.clone(),
                stream_id: self.stream_id.clone(),
                started_at: self.started_at.clone(),
                finished_at,
                duration_ms,
                finish_reason: reason,
                chunk_count: self.chunk_count,
                output_chars: self.output_chars,
                usage: final_usage,
                error,
                cancellation_source,
            }),
        );

        let _ = remove_active_supervisor_query_for_query(
            self.event_store_path.as_str(),
            self.stream_id.as_str(),
            self.query_id.as_str(),
        );
    }
}

impl Drop for SupervisorLifecycleRecordingStream {
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        let source = if self.cancellation_requested_by_user.load(Ordering::Relaxed) {
            SupervisorQueryCancellationSource::UserInitiated
        } else {
            SupervisorQueryCancellationSource::Runtime
        };

        self.finalize(
            LlmFinishReason::Cancelled,
            None,
            Some("supervisor stream dropped before terminal chunk".to_owned()),
        );

        if source == SupervisorQueryCancellationSource::Runtime {
            let cancelled_at = now_timestamp();
            append_supervisor_event_best_effort(
                self.event_store_path.as_str(),
                next_event_id("supervisor-query-cancelled"),
                cancelled_at.clone(),
                self.work_item_id.clone(),
                self.session_id.clone(),
                OrchestrationEventPayload::SupervisorQueryCancelled(
                    SupervisorQueryCancelledPayload {
                        query_id: self.query_id.clone(),
                        stream_id: self.stream_id.clone(),
                        cancelled_at,
                        source,
                    },
                ),
            );
        }
    }
}

#[async_trait::async_trait]
impl LlmResponseSubscription for SupervisorLifecycleRecordingStream {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
        match self.inner.next_chunk().await {
            Ok(Some(chunk)) => {
                self.observe_chunk(&chunk);
                if let Some(reason) = chunk.finish_reason.clone() {
                    self.finalize(reason, chunk.usage.clone(), None);
                }
                Ok(Some(chunk))
            }
            Ok(None) => {
                self.finalize(LlmFinishReason::Stop, None, None);
                Ok(None)
            }
            Err(error) => {
                self.finalize(LlmFinishReason::Error, None, Some(error.to_string()));
                Err(error)
            }
        }
    }
}

pub(crate) fn record_user_initiated_supervisor_cancel(event_store_path: &str, stream_id: &str) {
    let Some(state) = mark_user_cancellation_requested(event_store_path, stream_id) else {
        return;
    };

    let cancelled_at = now_timestamp();
    append_supervisor_event_best_effort(
        event_store_path,
        next_event_id("supervisor-query-cancelled"),
        cancelled_at.clone(),
        state.work_item_id,
        state.session_id,
        OrchestrationEventPayload::SupervisorQueryCancelled(SupervisorQueryCancelledPayload {
            query_id: state.query_id,
            stream_id: stream_id.to_owned(),
            cancelled_at,
            source: SupervisorQueryCancellationSource::UserInitiated,
        }),
    );
}

async fn execute_supervisor_query<P>(
    supervisor: &P,
    ticketing: &(dyn TicketingProvider + Send + Sync),
    event_store_path: &str,
    args: SupervisorQueryArgs,
    fallback_context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let context = merge_supervisor_query_context(fallback_context, args_context(&args));
    let scope = resolve_supervisor_query_scope(&context)?;
    let store = open_event_store(event_store_path)?;
    let query_engine = SupervisorQueryEngine::default();
    let context_pack =
        query_engine.build_context_pack_with_filters(&store, scope.clone(), &context)?;
    let messages = match &args {
        SupervisorQueryArgs::Template {
            template,
            variables,
            ..
        } => build_template_messages_with_variables(template.as_str(), variables, &context_pack)?,
        SupervisorQueryArgs::Freeform { query, .. } => {
            build_freeform_messages(query.as_str(), &context_pack)?
        }
    };

    let mut messages = messages;
    let mut queued_chunks = VecDeque::new();
    let mut completed = false;
    let stream_id = next_runtime_stream_id();

    for iteration in 0..MAX_TOOL_LOOP_ITERATIONS {
        let turn = run_supervisor_turn(supervisor, messages.clone()).await?;
        for chunk in turn.chunks {
            queued_chunks.push_back(chunk);
        }

        if matches!(turn.finish_reason, LlmFinishReason::ToolCall) {
            if turn.assistant_message.tool_calls.is_empty() {
                return Err(CoreError::DependencyUnavailable(
                    "assistant requested a tool call but did not provide tool arguments".to_owned(),
                ));
            }

            messages.push(turn.assistant_message.clone());
            let tool_calls = turn.assistant_message.tool_calls.clone();

            for call in &tool_calls {
                queued_chunks.push_back(tool_progress_chunk(call));
            }

            let outputs = execute_tool_calls(ticketing, tool_calls.clone()).await?;
            let names = tool_calls
                .iter()
                .map(|call| (call.id.as_str(), call.name.as_str()))
                .collect::<std::collections::HashMap<_, _>>();

            for output in outputs {
                let call_name = names
                    .get(output.tool_call_id.as_str())
                    .copied()
                    .unwrap_or("tool");
                queued_chunks.push_back(LlmStreamChunk {
                    delta: format!(
                        "{TOOL_RESULT_PREFIX} {call_name} {} {}",
                        output.tool_call_id, output.output
                    ),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    usage: None,
                    rate_limit: None,
                });
                messages.push(LlmMessage {
                    role: LlmRole::Tool,
                    content: output.output,
                    name: Some(call_name.to_owned()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(output.tool_call_id),
                });
            }

            if iteration + 1 >= MAX_TOOL_LOOP_ITERATIONS {
                return Err(CoreError::DependencyUnavailable(format!(
                    "tool calling exceeded loop limit of {MAX_TOOL_LOOP_ITERATIONS} iterations"
                )));
            }

            continue;
        }

        completed = matches!(
            turn.finish_reason,
            LlmFinishReason::Stop
                | LlmFinishReason::Length
                | LlmFinishReason::ContentFilter
                | LlmFinishReason::Cancelled
                | LlmFinishReason::Error
        );
        break;
    }

    if !completed {
        return Err(CoreError::DependencyUnavailable(format!(
            "tool-calling did not complete within {MAX_TOOL_LOOP_ITERATIONS} turns"
        )));
    }

    let stream = ToolingStream::new(queued_chunks.into_iter().collect());

    let query_id = next_supervisor_query_id();
    let started_at = now_timestamp();
    let started_time = SystemTime::now();
    let (work_item_id, session_id) = event_scope_identifiers(&scope, &context);
    let cancellation_requested_by_user = Arc::new(AtomicBool::new(false));

    append_supervisor_event(
        event_store_path,
        next_event_id("supervisor-query-started"),
        started_at.clone(),
        work_item_id.clone(),
        session_id.clone(),
        OrchestrationEventPayload::SupervisorQueryStarted(SupervisorQueryStartedPayload {
            query_id: query_id.clone(),
            stream_id: stream_id.clone(),
            scope: scope_label(&scope),
            started_at: started_at.clone(),
            kind: query_kind(&args),
            template: query_template(&args),
            query: query_text(&args),
        }),
    )?;

    register_active_supervisor_query(
        event_store_path,
        stream_id.as_str(),
        ActiveSupervisorQueryState {
            query_id: query_id.clone(),
            work_item_id: work_item_id.clone(),
            session_id: session_id.clone(),
            cancellation_requested_by_user: cancellation_requested_by_user.clone(),
        },
    );

    let stream = SupervisorLifecycleRecordingStream::new(
        event_store_path.to_owned(),
        work_item_id,
        session_id,
        query_id,
        stream_id.clone(),
        started_at,
        started_time,
        cancellation_requested_by_user,
        Box::new(stream),
    );

    Ok((stream_id, Box::new(stream)))
}

async fn execute_github_open_review_tabs_with_opener(
    code_host: &impl CodeHostProvider,
    event_store_path: &str,
    context: SupervisorCommandContext,
    url_opener: &impl UrlOpener,
) -> Result<(String, LlmResponseStream), CoreError> {
    let mut store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context_with_command(
        &store,
        &context,
        command_ids::GITHUB_OPEN_REVIEW_TABS,
    )?;
    let pr = resolve_pull_request_for_mapping_with_vcs_fallback(
        &mut store,
        &runtime,
        command_ids::GITHUB_OPEN_REVIEW_TABS,
        code_host,
    )
    .await?;

    let command_id = command_ids::GITHUB_OPEN_REVIEW_TABS;
    UrlOpener::open_url(url_opener, pr.url.as_str())
        .await
        .map_err(|error| {
            let detail = sanitize_error_display_text(error.to_string().as_str());
            CoreError::DependencyUnavailable(format!(
                "{command_id} failed to open PR URL '{}': {detail}",
                pr.url,
            ))
        })?;
    runtime_command_response(&format!("{GITHUB_OPEN_REVIEW_TABS_ACK} {}", pr.url))
}

async fn execute_github_open_review_tabs(
    code_host: &impl CodeHostProvider,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError> {
    let opener = default_system_url_opener()?;
    execute_github_open_review_tabs_with_opener(code_host, event_store_path, context, &opener).await
}

pub(crate) async fn dispatch_supervisor_runtime_command<P>(
    supervisor: &P,
    code_host: &impl CodeHostProvider,
    ticketing: &(dyn TicketingProvider + Send + Sync),
    event_store_path: &str,
    invocation: UntypedCommandInvocation,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let command = CommandRegistry::default().parse_invocation(&invocation)?;
    match command {
        Command::SupervisorQuery(args) => {
            execute_supervisor_query(supervisor, ticketing, event_store_path, args, context).await
        }
        Command::WorkflowApprovePrReady => {
            execute_workflow_approve_pr_ready(code_host, event_store_path, context).await
        }
        Command::WorkflowReconcilePrMerge => {
            execute_workflow_reconcile_pr_merge(code_host, event_store_path, context).await
        }
        Command::WorkflowMergePr => {
            execute_workflow_merge_pr(code_host, event_store_path, context).await
        }
        Command::GithubOpenReviewTabs => {
            execute_github_open_review_tabs(code_host, event_store_path, context).await
        }
        command => Err(invalid_supervisor_runtime_usage(command.id())),
    }
}
