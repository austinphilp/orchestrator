use std::sync::Arc;

use crate::commands::UntypedCommandInvocation;
use crate::controller::contracts::{
    InboxPublishRequest, InboxResolveRequest, MergeQueueEvent, SessionWorkflowAdvanceOutcome,
    SupervisorCommandContext, SupervisorCommandDispatcher, TicketPickerProvider,
};
use crate::events::StoredEventEnvelope;
use orchestrator_domain::{
    CoreError, LlmChatRequest, LlmFinishReason, LlmProvider, LlmRateLimitState, LlmResponseStream,
    LlmTokenUsage, WorkerSessionId,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorStreamRunnerEvent {
    Started {
        stream_id: String,
    },
    Delta {
        text: String,
    },
    RateLimit {
        state: LlmRateLimitState,
    },
    Usage {
        usage: LlmTokenUsage,
    },
    Finished {
        reason: LlmFinishReason,
        usage: Option<LlmTokenUsage>,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowAdvanceRunnerEvent {
    Advanced {
        outcome: SessionWorkflowAdvanceOutcome,
    },
    Failed {
        session_id: WorkerSessionId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxPublishRunnerEvent {
    Published { event: StoredEventEnvelope },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxResolveRunnerEvent {
    Resolved { event: Option<StoredEventEnvelope> },
    Failed { message: String },
}

pub async fn run_supervisor_stream_task<E>(
    provider: Arc<dyn LlmProvider>,
    request: LlmChatRequest,
    sender: mpsc::Sender<E>,
) where
    E: From<SupervisorStreamRunnerEvent> + Send + 'static,
{
    let (stream_id, stream) = match provider.stream_chat(request).await {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(
                    SupervisorStreamRunnerEvent::Failed {
                        message: error.to_string(),
                    }
                    .into(),
                )
                .await;
            return;
        }
    };

    relay_supervisor_stream(stream_id, stream, sender).await;
}

pub async fn run_supervisor_command_task<E>(
    dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    invocation: UntypedCommandInvocation,
    context: SupervisorCommandContext,
    sender: mpsc::Sender<E>,
) where
    E: From<SupervisorStreamRunnerEvent> + Send + 'static,
{
    let (stream_id, stream) = match dispatcher
        .dispatch_supervisor_command(invocation, context)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(
                    SupervisorStreamRunnerEvent::Failed {
                        message: error.to_string(),
                    }
                    .into(),
                )
                .await;
            return;
        }
    };

    relay_supervisor_stream(stream_id, stream, sender).await;
}

pub async fn run_session_merge_finalize_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<MergeQueueEvent>,
) {
    match provider
        .complete_session_after_merge(session_id.clone())
        .await
    {
        Ok(outcome) => {
            let _ = sender
                .send(MergeQueueEvent::SessionFinalized {
                    session_id,
                    event: outcome.event,
                })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(MergeQueueEvent::SessionFinalizeFailed {
                    session_id,
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                })
                .await;
        }
    }
}

pub async fn run_session_workflow_advance_task<E>(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<E>,
) where
    E: From<WorkflowAdvanceRunnerEvent> + Send + 'static,
{
    match provider.advance_session_workflow(session_id.clone()).await {
        Ok(outcome) => {
            let _ = sender
                .send(WorkflowAdvanceRunnerEvent::Advanced { outcome }.into())
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(
                    WorkflowAdvanceRunnerEvent::Failed {
                        session_id,
                        message: error.to_string(),
                    }
                    .into(),
                )
                .await;
        }
    }
}

pub async fn run_publish_inbox_item_task<E>(
    provider: Arc<dyn TicketPickerProvider>,
    request: InboxPublishRequest,
    sender: mpsc::Sender<E>,
) where
    E: From<InboxPublishRunnerEvent> + Send + 'static,
{
    match provider.publish_inbox_item(request).await {
        Ok(event) => {
            let _ = sender
                .send(InboxPublishRunnerEvent::Published { event }.into())
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(
                    InboxPublishRunnerEvent::Failed {
                        message: sanitize_terminal_display_text(error.to_string().as_str()),
                    }
                    .into(),
                )
                .await;
        }
    }
}

pub async fn run_resolve_inbox_item_task<E>(
    provider: Arc<dyn TicketPickerProvider>,
    request: InboxResolveRequest,
    sender: mpsc::Sender<E>,
) where
    E: From<InboxResolveRunnerEvent> + Send + 'static,
{
    match provider.resolve_inbox_item(request).await {
        Ok(event) => {
            let _ = sender
                .send(InboxResolveRunnerEvent::Resolved { event }.into())
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(
                    InboxResolveRunnerEvent::Failed {
                        message: sanitize_terminal_display_text(error.to_string().as_str()),
                    }
                    .into(),
                )
                .await;
        }
    }
}

pub async fn run_start_pr_pipeline_polling_task(
    provider: Arc<dyn TicketPickerProvider>,
    sender: mpsc::Sender<MergeQueueEvent>,
) -> Result<(), CoreError> {
    provider.start_pr_pipeline_polling(sender).await
}

pub async fn run_stop_pr_pipeline_polling_task(
    provider: Arc<dyn TicketPickerProvider>,
) -> Result<(), CoreError> {
    provider.stop_pr_pipeline_polling().await
}

async fn relay_supervisor_stream<E>(
    stream_id: String,
    mut stream: LlmResponseStream,
    sender: mpsc::Sender<E>,
) where
    E: From<SupervisorStreamRunnerEvent> + Send + 'static,
{
    if sender
        .send(SupervisorStreamRunnerEvent::Started { stream_id }.into())
        .await
        .is_err()
    {
        return;
    }

    loop {
        match stream.next_chunk().await {
            Ok(Some(chunk)) => {
                let delta = chunk.delta;
                let finish_reason = chunk.finish_reason;
                let usage = chunk.usage;
                let rate_limit = chunk.rate_limit;

                if !delta.is_empty()
                    && sender
                        .send(SupervisorStreamRunnerEvent::Delta { text: delta }.into())
                        .await
                        .is_err()
                {
                    return;
                }

                if let Some(rate_limit) = rate_limit {
                    if sender
                        .send(SupervisorStreamRunnerEvent::RateLimit { state: rate_limit }.into())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }

                if finish_reason.is_none() {
                    if let Some(usage) = usage.clone() {
                        if sender
                            .send(SupervisorStreamRunnerEvent::Usage { usage }.into())
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }

                if let Some(reason) = finish_reason {
                    let _ = sender
                        .send(SupervisorStreamRunnerEvent::Finished { reason, usage }.into())
                        .await;
                    return;
                }
            }
            Ok(None) => {
                let _ = sender
                    .send(
                        SupervisorStreamRunnerEvent::Finished {
                            reason: LlmFinishReason::Stop,
                            usage: None,
                        }
                        .into(),
                    )
                    .await;
                return;
            }
            Err(error) => {
                let _ = sender
                    .send(
                        SupervisorStreamRunnerEvent::Failed {
                            message: error.to_string(),
                        }
                        .into(),
                    )
                    .await;
                return;
            }
        }
    }
}

fn sanitize_terminal_display_text(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            '\u{00a0}' => ' ',
            '\t' => ' ',
            '\n' | '\r' => '\n',
            _ if ch.is_control() => ' ',
            _ => ch,
        })
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use orchestrator_domain::{
        CoreError, InboxItemId, InboxItemKind, LlmProviderKind, LlmResponseSubscription,
        LlmStreamChunk, OrchestrationEventPayload, OrchestrationEventType,
        SelectedTicketFlowResult, SessionCompletedPayload, WorkItemId, WorkerSessionId,
        WorkflowState,
    };
    use orchestrator_ticketing::TicketSummary;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct TestLlmStream {
        chunks: VecDeque<Result<LlmStreamChunk, CoreError>>,
    }

    #[async_trait]
    impl LlmResponseSubscription for TestLlmStream {
        async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
            match self.chunks.pop_front() {
                Some(Ok(chunk)) => Ok(Some(chunk)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        }
    }

    #[derive(Debug)]
    struct TestLlmProvider {
        chunks: Mutex<Option<Vec<Result<LlmStreamChunk, CoreError>>>>,
    }

    impl TestLlmProvider {
        fn new(chunks: Vec<Result<LlmStreamChunk, CoreError>>) -> Self {
            Self {
                chunks: Mutex::new(Some(chunks)),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for TestLlmProvider {
        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::Other("test".to_owned())
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn stream_chat(
            &self,
            _request: LlmChatRequest,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            let chunks = self
                .chunks
                .lock()
                .expect("stream chunk lock")
                .take()
                .unwrap_or_default();
            Ok((
                "test-stream".to_owned(),
                Box::new(TestLlmStream {
                    chunks: chunks.into(),
                }),
            ))
        }

        async fn cancel_stream(&self, _stream_id: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TestSupervisorDispatcher {
        chunks: Mutex<Option<Vec<Result<LlmStreamChunk, CoreError>>>>,
    }

    impl TestSupervisorDispatcher {
        fn new(chunks: Vec<Result<LlmStreamChunk, CoreError>>) -> Self {
            Self {
                chunks: Mutex::new(Some(chunks)),
            }
        }
    }

    #[async_trait]
    impl SupervisorCommandDispatcher for TestSupervisorDispatcher {
        async fn dispatch_supervisor_command(
            &self,
            _invocation: UntypedCommandInvocation,
            _context: SupervisorCommandContext,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            let chunks = self
                .chunks
                .lock()
                .expect("dispatcher stream lock")
                .take()
                .unwrap_or_default();
            Ok((
                "dispatcher-stream".to_owned(),
                Box::new(TestLlmStream {
                    chunks: chunks.into(),
                }),
            ))
        }

        async fn cancel_supervisor_command(&self, _stream_id: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct TestTicketPickerProvider {
        workflow_outcome: Mutex<Option<Result<SessionWorkflowAdvanceOutcome, CoreError>>>,
        merge_outcome: Mutex<
            Option<Result<crate::controller::contracts::SessionMergeFinalizeOutcome, CoreError>>,
        >,
        publish_outcome: Mutex<Option<Result<StoredEventEnvelope, CoreError>>>,
        resolve_outcome: Mutex<Option<Result<Option<StoredEventEnvelope>, CoreError>>>,
        polling_start_outcome: Mutex<Option<Result<(), CoreError>>>,
        polling_stop_outcome: Mutex<Option<Result<(), CoreError>>>,
        polling_started: Mutex<usize>,
        polling_stopped: Mutex<usize>,
    }

    impl TestTicketPickerProvider {
        fn with_defaults() -> Self {
            Self {
                workflow_outcome: Mutex::new(None),
                merge_outcome: Mutex::new(None),
                publish_outcome: Mutex::new(None),
                resolve_outcome: Mutex::new(None),
                polling_start_outcome: Mutex::new(None),
                polling_stop_outcome: Mutex::new(None),
                polling_started: Mutex::new(0),
                polling_stopped: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl TicketPickerProvider for TestTicketPickerProvider {
        async fn list_unfinished_tickets(&self) -> Result<Vec<TicketSummary>, CoreError> {
            Ok(Vec::new())
        }

        async fn start_or_resume_ticket(
            &self,
            _ticket: TicketSummary,
            _repository_override: Option<PathBuf>,
        ) -> Result<SelectedTicketFlowResult, CoreError> {
            Err(CoreError::DependencyUnavailable("not used".to_owned()))
        }

        async fn create_ticket_from_brief(
            &self,
            _request: crate::controller::contracts::CreateTicketFromPickerRequest,
        ) -> Result<TicketSummary, CoreError> {
            Err(CoreError::DependencyUnavailable("not used".to_owned()))
        }

        async fn reload_projection(&self) -> Result<crate::projection::ProjectionState, CoreError> {
            Ok(crate::projection::ProjectionState::default())
        }

        async fn advance_session_workflow(
            &self,
            _session_id: WorkerSessionId,
        ) -> Result<SessionWorkflowAdvanceOutcome, CoreError> {
            self.workflow_outcome
                .lock()
                .expect("workflow outcome lock")
                .take()
                .unwrap_or_else(|| Err(CoreError::DependencyUnavailable("not set".to_owned())))
        }

        async fn complete_session_after_merge(
            &self,
            _session_id: WorkerSessionId,
        ) -> Result<crate::controller::contracts::SessionMergeFinalizeOutcome, CoreError> {
            self.merge_outcome
                .lock()
                .expect("merge outcome lock")
                .take()
                .unwrap_or_else(|| Err(CoreError::DependencyUnavailable("not set".to_owned())))
        }

        async fn publish_inbox_item(
            &self,
            _request: InboxPublishRequest,
        ) -> Result<StoredEventEnvelope, CoreError> {
            self.publish_outcome
                .lock()
                .expect("publish outcome lock")
                .take()
                .unwrap_or_else(|| Err(CoreError::DependencyUnavailable("not set".to_owned())))
        }

        async fn resolve_inbox_item(
            &self,
            _request: InboxResolveRequest,
        ) -> Result<Option<StoredEventEnvelope>, CoreError> {
            self.resolve_outcome
                .lock()
                .expect("resolve outcome lock")
                .take()
                .unwrap_or_else(|| Err(CoreError::DependencyUnavailable("not set".to_owned())))
        }

        async fn start_pr_pipeline_polling(
            &self,
            _sender: mpsc::Sender<MergeQueueEvent>,
        ) -> Result<(), CoreError> {
            if let Some(outcome) = self
                .polling_start_outcome
                .lock()
                .expect("polling start outcome lock")
                .take()
            {
                return outcome;
            }
            let mut started = self.polling_started.lock().expect("polling started lock");
            *started += 1;
            Ok(())
        }

        async fn stop_pr_pipeline_polling(&self) -> Result<(), CoreError> {
            if let Some(outcome) = self
                .polling_stop_outcome
                .lock()
                .expect("polling stop outcome lock")
                .take()
            {
                return outcome;
            }
            let mut stopped = self.polling_stopped.lock().expect("polling stopped lock");
            *stopped += 1;
            Ok(())
        }
    }

    fn stored_event(event_id: &str) -> StoredEventEnvelope {
        StoredEventEnvelope {
            event_id: event_id.to_owned(),
            sequence: 1,
            occurred_at: "2026-02-20T00:00:00Z".to_owned(),
            work_item_id: Some(WorkItemId::new("wi-1")),
            session_id: Some(WorkerSessionId::new("sess-1")),
            event_type: OrchestrationEventType::SessionCompleted,
            payload: OrchestrationEventPayload::SessionCompleted(SessionCompletedPayload {
                session_id: WorkerSessionId::new("sess-1"),
                summary: Some("done".to_owned()),
            }),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn supervisor_stream_runner_emits_usage_and_finished_events() {
        let provider: Arc<dyn LlmProvider> = Arc::new(TestLlmProvider::new(vec![
            Ok(LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: Some(LlmTokenUsage {
                    input_tokens: 5,
                    output_tokens: 7,
                    total_tokens: 12,
                }),
                rate_limit: None,
            }),
            Ok(LlmStreamChunk {
                delta: "ok".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }),
        ]));
        let request = LlmChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            tool_choice: None,
            max_output_tokens: None,
        };
        let (sender, mut receiver) = mpsc::channel(8);

        run_supervisor_stream_task(provider, request, sender).await;

        let mut saw_usage = false;
        let mut saw_finished = false;
        while let Some(event) = receiver.recv().await {
            match event {
                SupervisorStreamRunnerEvent::Usage { usage } => {
                    saw_usage |= usage.total_tokens == 12;
                }
                SupervisorStreamRunnerEvent::Finished { reason, usage } => {
                    saw_finished |= reason == LlmFinishReason::Stop && usage.is_none();
                }
                _ => {}
            }
        }
        assert!(saw_usage);
        assert!(saw_finished);
    }

    #[derive(Debug)]
    struct FailingLlmProvider;

    #[async_trait]
    impl LlmProvider for FailingLlmProvider {
        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::Other("test".to_owned())
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn stream_chat(
            &self,
            _request: LlmChatRequest,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "provider unavailable".to_owned(),
            ))
        }

        async fn cancel_stream(&self, _stream_id: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn supervisor_stream_runner_emits_failed_event_when_provider_errors() {
        let provider: Arc<dyn LlmProvider> = Arc::new(FailingLlmProvider);
        let request = LlmChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            tool_choice: None,
            max_output_tokens: None,
        };
        let (sender, mut receiver) = mpsc::channel(2);

        run_supervisor_stream_task(provider, request, sender).await;

        let event = receiver.recv().await.expect("failed event");
        assert!(matches!(
            event,
            SupervisorStreamRunnerEvent::Failed { message }
                if message.contains("provider unavailable")
        ));
    }

    #[tokio::test]
    async fn supervisor_stream_runner_emits_rate_limit_and_failed_events_on_stream_error() {
        let provider: Arc<dyn LlmProvider> = Arc::new(TestLlmProvider::new(vec![
            Ok(LlmStreamChunk {
                delta: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                rate_limit: Some(LlmRateLimitState {
                    requests_remaining: Some(8),
                    tokens_remaining: Some(1200),
                    reset_at: Some("2026-02-20T00:10:00Z".to_owned()),
                }),
            }),
            Err(CoreError::DependencyUnavailable(
                "stream transport failed".to_owned(),
            )),
        ]));
        let request = LlmChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            tool_choice: None,
            max_output_tokens: None,
        };
        let (sender, mut receiver) = mpsc::channel(4);

        run_supervisor_stream_task(provider, request, sender).await;

        let mut saw_rate_limit = false;
        let mut saw_failed = false;
        while let Some(event) = receiver.recv().await {
            match event {
                SupervisorStreamRunnerEvent::RateLimit { state } => {
                    saw_rate_limit |=
                        state.requests_remaining == Some(8) && state.tokens_remaining == Some(1200);
                }
                SupervisorStreamRunnerEvent::Failed { message } => {
                    saw_failed |= message.contains("stream transport failed");
                }
                _ => {}
            }
        }

        assert!(saw_rate_limit);
        assert!(saw_failed);
    }

    #[tokio::test]
    async fn supervisor_command_runner_emits_started_and_delta_events() {
        let dispatcher: Arc<dyn SupervisorCommandDispatcher> =
            Arc::new(TestSupervisorDispatcher::new(vec![Ok(LlmStreamChunk {
                delta: "hello".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            })]));
        let (sender, mut receiver) = mpsc::channel(4);
        run_supervisor_command_task(
            dispatcher,
            UntypedCommandInvocation {
                command_id: "supervisor.query".to_owned(),
                args: None,
            },
            SupervisorCommandContext::default(),
            sender,
        )
        .await;

        let first = receiver.recv().await.expect("started event");
        assert!(matches!(first, SupervisorStreamRunnerEvent::Started { .. }));
        let second = receiver.recv().await.expect("delta event");
        assert!(matches!(
            second,
            SupervisorStreamRunnerEvent::Delta { text } if text == "hello"
        ));
    }

    struct FailingSupervisorDispatcher;

    #[async_trait]
    impl SupervisorCommandDispatcher for FailingSupervisorDispatcher {
        async fn dispatch_supervisor_command(
            &self,
            _invocation: UntypedCommandInvocation,
            _context: SupervisorCommandContext,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            Err(CoreError::DependencyUnavailable(
                "dispatcher unavailable".to_owned(),
            ))
        }

        async fn cancel_supervisor_command(&self, _stream_id: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn supervisor_command_runner_emits_failed_event_when_dispatch_errors() {
        let dispatcher: Arc<dyn SupervisorCommandDispatcher> =
            Arc::new(FailingSupervisorDispatcher);
        let (sender, mut receiver) = mpsc::channel(2);

        run_supervisor_command_task(
            dispatcher,
            UntypedCommandInvocation {
                command_id: "supervisor.query".to_owned(),
                args: None,
            },
            SupervisorCommandContext::default(),
            sender,
        )
        .await;

        let event = receiver.recv().await.expect("failed event");
        assert!(matches!(
            event,
            SupervisorStreamRunnerEvent::Failed { message }
                if message.contains("dispatcher unavailable")
        ));
    }

    #[tokio::test]
    async fn session_workflow_advance_runner_emits_advanced_event() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        *provider
            .workflow_outcome
            .lock()
            .expect("workflow outcome lock") = Some(Ok(SessionWorkflowAdvanceOutcome {
            session_id: WorkerSessionId::new("sess-1"),
            work_item_id: WorkItemId::new("wi-1"),
            from: WorkflowState::Planning,
            to: WorkflowState::Implementing,
            instruction: Some("continue".to_owned()),
            event: stored_event("evt-workflow-1"),
        }));
        let (sender, mut receiver) = mpsc::channel(2);

        run_session_workflow_advance_task(provider, WorkerSessionId::new("sess-1"), sender).await;

        let event = receiver.recv().await.expect("workflow event");
        assert!(matches!(
            event,
            WorkflowAdvanceRunnerEvent::Advanced { outcome }
                if outcome.event.event_id == "evt-workflow-1"
        ));
    }

    #[tokio::test]
    async fn session_workflow_advance_runner_emits_failed_event() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        *provider
            .workflow_outcome
            .lock()
            .expect("workflow outcome lock") = Some(Err(CoreError::DependencyUnavailable(
            "workflow advance failed".to_owned(),
        )));
        let (sender, mut receiver) = mpsc::channel(2);

        run_session_workflow_advance_task(provider, WorkerSessionId::new("sess-1"), sender).await;

        let event = receiver.recv().await.expect("workflow event");
        assert!(matches!(
            event,
            WorkflowAdvanceRunnerEvent::Failed {
                session_id,
                message,
            } if session_id.as_str() == "sess-1" && message.contains("workflow advance failed")
        ));
    }

    #[tokio::test]
    async fn inbox_publish_and_resolve_runners_emit_events() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        *provider
            .publish_outcome
            .lock()
            .expect("publish outcome lock") = Some(Ok(stored_event("evt-inbox-created")));
        *provider
            .resolve_outcome
            .lock()
            .expect("resolve outcome lock") = Some(Ok(Some(stored_event("evt-inbox-resolved"))));

        let (publish_sender, mut publish_receiver) = mpsc::channel(2);
        run_publish_inbox_item_task(
            provider.clone(),
            InboxPublishRequest {
                work_item_id: WorkItemId::new("wi-1"),
                session_id: Some(WorkerSessionId::new("sess-1")),
                kind: InboxItemKind::NeedsApproval,
                title: "Approval needed".to_owned(),
                coalesce_key: "approval-needed".to_owned(),
            },
            publish_sender,
        )
        .await;
        let publish_event = publish_receiver.recv().await.expect("publish event");
        assert!(matches!(
            publish_event,
            InboxPublishRunnerEvent::Published { event } if event.event_id == "evt-inbox-created"
        ));

        let (resolve_sender, mut resolve_receiver) = mpsc::channel(2);
        run_resolve_inbox_item_task(
            provider,
            InboxResolveRequest {
                inbox_item_id: InboxItemId::new("inbox-1"),
                work_item_id: WorkItemId::new("wi-1"),
            },
            resolve_sender,
        )
        .await;
        let resolve_event = resolve_receiver.recv().await.expect("resolve event");
        assert!(matches!(
            resolve_event,
            InboxResolveRunnerEvent::Resolved { event: Some(event) }
                if event.event_id == "evt-inbox-resolved"
        ));
    }

    #[tokio::test]
    async fn inbox_runners_emit_sanitized_failed_events() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        *provider
            .publish_outcome
            .lock()
            .expect("publish outcome lock") = Some(Err(CoreError::DependencyUnavailable(
            "publish failed:\u{0007}\tbad".to_owned(),
        )));
        *provider
            .resolve_outcome
            .lock()
            .expect("resolve outcome lock") = Some(Err(CoreError::DependencyUnavailable(
            "resolve failed:\u{0001}\tbad".to_owned(),
        )));

        let (publish_sender, mut publish_receiver) = mpsc::channel(2);
        run_publish_inbox_item_task(
            provider.clone(),
            InboxPublishRequest {
                work_item_id: WorkItemId::new("wi-1"),
                session_id: Some(WorkerSessionId::new("sess-1")),
                kind: InboxItemKind::NeedsApproval,
                title: "Approval needed".to_owned(),
                coalesce_key: "approval-needed".to_owned(),
            },
            publish_sender,
        )
        .await;
        let publish_event = publish_receiver.recv().await.expect("publish event");
        assert!(matches!(
            publish_event,
            InboxPublishRunnerEvent::Failed { message }
                if message.contains("publish failed:  bad")
        ));

        let (resolve_sender, mut resolve_receiver) = mpsc::channel(2);
        run_resolve_inbox_item_task(
            provider,
            InboxResolveRequest {
                inbox_item_id: InboxItemId::new("inbox-1"),
                work_item_id: WorkItemId::new("wi-1"),
            },
            resolve_sender,
        )
        .await;
        let resolve_event = resolve_receiver.recv().await.expect("resolve event");
        assert!(matches!(
            resolve_event,
            InboxResolveRunnerEvent::Failed { message }
                if message.contains("resolve failed:  bad")
        ));
    }

    #[tokio::test]
    async fn merge_finalize_runner_emits_failed_event_with_sanitized_message() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        *provider.merge_outcome.lock().expect("merge outcome lock") = Some(Err(
            CoreError::DependencyUnavailable("merge failed:\u{0007}\tboom".to_owned()),
        ));
        let (sender, mut receiver) = mpsc::channel(2);

        run_session_merge_finalize_task(provider, WorkerSessionId::new("sess-1"), sender).await;

        let event = receiver.recv().await.expect("merge event");
        assert!(matches!(
            event,
            MergeQueueEvent::SessionFinalizeFailed { message, .. }
                if message.contains("merge failed:  boom")
        ));
    }

    #[tokio::test]
    async fn polling_runners_delegate_start_and_stop_to_provider() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        let (sender, _receiver) = mpsc::channel(1);

        run_start_pr_pipeline_polling_task(provider.clone(), sender)
            .await
            .expect("start polling");
        run_stop_pr_pipeline_polling_task(provider.clone())
            .await
            .expect("stop polling");

        assert_eq!(*provider.polling_started.lock().expect("started lock"), 1);
        assert_eq!(*provider.polling_stopped.lock().expect("stopped lock"), 1);
    }

    #[tokio::test]
    async fn polling_runners_propagate_provider_errors() {
        let provider = Arc::new(TestTicketPickerProvider::with_defaults());
        *provider
            .polling_start_outcome
            .lock()
            .expect("polling start outcome lock") = Some(Err(CoreError::DependencyUnavailable(
            "polling start failed".to_owned(),
        )));
        *provider
            .polling_stop_outcome
            .lock()
            .expect("polling stop outcome lock") = Some(Err(CoreError::DependencyUnavailable(
            "polling stop failed".to_owned(),
        )));
        let (sender, _receiver) = mpsc::channel(1);

        let start = run_start_pr_pipeline_polling_task(provider.clone(), sender).await;
        let stop = run_stop_pr_pipeline_polling_task(provider).await;

        assert!(matches!(
            start,
            Err(CoreError::DependencyUnavailable(message)) if message.contains("polling start failed")
        ));
        assert!(matches!(
            stop,
            Err(CoreError::DependencyUnavailable(message)) if message.contains("polling stop failed")
        ));
    }
}
