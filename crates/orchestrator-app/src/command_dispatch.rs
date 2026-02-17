use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use orchestrator_core::{
    command_ids, resolve_supervisor_query_scope, ArtifactKind, Command, CommandRegistry, CodeHostProvider,
    CoreError, EventStore, LlmChatRequest, LlmFinishReason, LlmProvider, LlmResponseStream,
    LlmResponseSubscription, LlmStreamChunk, LlmTokenUsage, NewEventEnvelope,
    OrchestrationEventPayload, PullRequestRef, RepositoryRef, RetrievalScope, RuntimeMappingRecord,
    SqliteEventStore,
    SupervisorQueryArgs, SupervisorQueryCancellationSource, SupervisorQueryCancelledPayload,
    SupervisorQueryChunkPayload, SupervisorQueryContextArgs, SupervisorQueryFinishedPayload,
    SupervisorQueryKind, SupervisorQueryStartedPayload, UntypedCommandInvocation, WorkItemId,
    WorkerSessionId, DOMAIN_EVENT_SCHEMA_VERSION,
};
use orchestrator_supervisor::{
    build_freeform_messages, build_inferred_template_messages,
    build_template_messages_with_variables, infer_template_from_query, SupervisorQueryEngine,
};
use orchestrator_ui::SupervisorCommandContext;
use tracing::warn;

use crate::{open_event_store, supervisor_model_from_env};

static SUPERVISOR_QUERY_EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);
static SUPERVISOR_QUERY_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static RUNTIME_COMMAND_STREAM_COUNTER: AtomicU64 = AtomicU64::new(1);

const SUPPORTED_RUNTIME_COMMAND_IDS: [&str; 3] = [
    command_ids::SUPERVISOR_QUERY,
    command_ids::WORKFLOW_APPROVE_PR_READY,
    command_ids::GITHUB_OPEN_REVIEW_TABS,
];

const GITHUB_OPEN_REVIEW_TABS_ACK: &str =
    "github.open_review_tabs command accepted. This action is not yet wired to a tab launcher.";

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
        SupervisorQueryArgs::Freeform { query, .. } => {
            infer_template_from_query(query.as_str()).map(|template| template.key().to_owned())
        }
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

fn runtime_command_response(
    message: &str,
) -> Result<(String, LlmResponseStream), CoreError> {
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
    let segment = normalized
        .split("/pull/")
        .nth(1)
        .ok_or_else(|| CoreError::Configuration(format!("Could not resolve PR number from URL `{url}`")))?;

    let digits = segment.chars().take_while(char::is_ascii_digit).collect::<String>();
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

    #[test]
    fn parse_pull_request_number_supports_query_and_fragment_suffix() {
        let number = parse_pull_request_number("https://github.com/acme/repo/pull/123?draft=true#section")
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
}

fn normalize_pull_request_url(url: &str) -> &str {
    url.trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '(' || ch == ')')
        .trim_end_matches(|ch: char| ch == ',' || ch == ';' || ch == '.')
}

fn resolve_runtime_mapping_for_context(
    store: &SqliteEventStore,
    context: &SupervisorCommandContext,
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
            "workflow.approve_pr_ready could not resolve runtime mapping for selected session '{session_id}'"
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
            "workflow.approve_pr_ready could not resolve runtime mapping for selected work item '{work_item_id}'"
        )));
    }

    Err(CoreError::Configuration(
        "workflow.approve_pr_ready requires an active runtime session or selected work item context".to_owned(),
    ))
}

fn resolve_pull_request_for_mapping(
    store: &SqliteEventStore,
    mapping: &RuntimeMappingRecord,
) -> Result<PullRequestRef, CoreError> {
    let events = store.read_event_with_artifacts(&mapping.work_item_id)?;
    let mut candidate_parse_error: Option<CoreError> = None;

    for stored in events.iter().rev() {
        for artifact_id in stored.artifact_ids.iter().rev() {
            let Some(artifact) = store.get_artifact(artifact_id)? else {
                continue;
            };

            if artifact.kind != ArtifactKind::PR {
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
            "workflow.approve_pr_ready could not resolve a usable PR artifact for work item '{}': {}",
            mapping.work_item_id.as_str(),
            candidate_error
        )));
    }

    Err(CoreError::Configuration(format!(
        "workflow.approve_pr_ready could not resolve a PR artifact for work item '{}'",
        mapping.work_item_id.as_str()
    )))
}

async fn execute_workflow_approve_pr_ready<C>(
    code_host: &C,
    event_store_path: &str,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    C: CodeHostProvider + ?Sized,
{
    let store = open_event_store(event_store_path)?;
    let runtime = resolve_runtime_mapping_for_context(&store, &context)?;
    let pr = resolve_pull_request_for_mapping(&store, &runtime)?;

    code_host
        .mark_ready_for_review(&pr)
        .await
        .map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "workflow.approve_pr_ready failed for PR #{}: {error}",
                pr.number
            ))
        })?;

    runtime_command_response(&format!(
        "workflow.approve_pr_ready command completed for PR #{}",
        pr.number
    ))
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
            if let Some(template) = infer_template_from_query(query.as_str()) {
                build_inferred_template_messages(template, query.as_str(), &context_pack)?
            } else {
                build_freeform_messages(query.as_str(), &context_pack)?
            }
        }
    };

    let (stream_id, stream) = supervisor
        .stream_chat(LlmChatRequest {
            model: supervisor_model_from_env(),
            messages,
            temperature: Some(0.2),
            max_output_tokens: Some(700),
        })
        .await?;

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
        stream,
    );

    Ok((stream_id, Box::new(stream)))
}

pub(crate) async fn dispatch_supervisor_runtime_command<P>(
    supervisor: &P,
    code_host: &impl CodeHostProvider,
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
            execute_supervisor_query(supervisor, event_store_path, args, context).await
        }
        Command::WorkflowApprovePrReady => {
            execute_workflow_approve_pr_ready(code_host, event_store_path, context).await
        }
        Command::GithubOpenReviewTabs => runtime_command_response(
            GITHUB_OPEN_REVIEW_TABS_ACK,
        ),
        command => Err(invalid_supervisor_runtime_usage(command.id())),
    }
}
