use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use orchestrator_core::{
    CoreError, LlmChatRequest, LlmFinishReason, LlmMessage, LlmProvider, LlmProviderKind,
    LlmRateLimitState, LlmResponseStream, LlmResponseSubscription, LlmRole, LlmStreamChunk,
    LlmTokenUsage, Supervisor,
};
use reqwest::header::HeaderMap;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, Notify};

mod query_engine;
mod template_library;

pub use query_engine::{
    BoundedContextPack, RetrievalFocusFilters, RetrievalPackEvent, RetrievalPackEvidence,
    RetrievalPackLimits, RetrievalPackStats, SupervisorQueryEngine, SupervisorRetrievalSource,
    TicketStatusContext, TicketStatusTransition,
};
pub use template_library::{
    build_freeform_messages, build_inferred_template_messages, build_template_messages,
    build_template_messages_with_variables, infer_template_from_query, supervisor_template_catalog,
    SupervisorTemplate, SUPERVISOR_TEMPLATE_CURRENT_ACTIVITY, SUPERVISOR_TEMPLATE_NEXT_ACTIONS,
    SUPERVISOR_TEMPLATE_RECOMMENDED_RESPONSE, SUPERVISOR_TEMPLATE_RISK_ASSESSMENT,
    SUPERVISOR_TEMPLATE_TICKET_STATUS, SUPERVISOR_TEMPLATE_VARIABLE_OPERATOR_QUESTION,
    SUPERVISOR_TEMPLATE_WHAT_CHANGED, SUPERVISOR_TEMPLATE_WHAT_IS_BLOCKING,
    SUPERVISOR_TEMPLATE_WHAT_NEEDS_ME,
};

const OPENROUTER_API_BASE: &str = "https://openrouter.ai/api/v1";
const STREAM_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone)]
pub struct OpenRouterSupervisor {
    api_key: String,
    base_url: String,
    http_client: reqwest::Client,
    active_streams: Arc<Mutex<HashMap<String, Arc<StreamCancellation>>>>,
    stream_counter: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct StreamCancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl StreamCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct ChannelLlmStream {
    receiver: mpsc::Receiver<Result<LlmStreamChunk, CoreError>>,
}

#[async_trait::async_trait]
impl LlmResponseSubscription for ChannelLlmStream {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
        match self.receiver.recv().await {
            Some(result) => result.map(Some),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Default)]
struct SseDataParser {
    pending: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseDataParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        self.drain_events(false)
    }

    fn finish(&mut self) -> Vec<String> {
        self.drain_events(true)
    }

    fn drain_events(&mut self, flush_partial_line: bool) -> Vec<String> {
        let mut events = Vec::new();
        loop {
            let newline_index = self.pending.iter().position(|&byte| byte == b'\n');
            let Some(newline_index) = newline_index else {
                break;
            };

            let mut line = self.pending.drain(..=newline_index).collect::<Vec<_>>();
            if line.last().copied() == Some(b'\n') {
                line.pop();
            }
            if line.last().copied() == Some(b'\r') {
                line.pop();
            }
            self.consume_line(&line, &mut events);
        }

        if flush_partial_line && !self.pending.is_empty() {
            let mut line = std::mem::take(&mut self.pending);
            if line.last().copied() == Some(b'\r') {
                line.pop();
            }
            self.consume_line(&line, &mut events);
        }

        if flush_partial_line && !self.data_lines.is_empty() {
            events.push(self.data_lines.join("\n"));
            self.data_lines.clear();
        }

        events
    }

    fn consume_line(&mut self, line: &[u8], events: &mut Vec<String>) {
        if line.is_empty() {
            if !self.data_lines.is_empty() {
                events.push(self.data_lines.join("\n"));
                self.data_lines.clear();
            }
            return;
        }

        if line.first().copied() == Some(b':') {
            return;
        }

        if let Some(rest) = line.strip_prefix(b"data:") {
            let value = String::from_utf8_lossy(rest).trim_start().to_owned();
            self.data_lines.push(value);
        }
    }
}

impl OpenRouterSupervisor {
    pub fn from_env() -> Result<Self, CoreError> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            CoreError::Configuration(
                "OPENROUTER_API_KEY is not set. Export a valid key before starting orchestrator-app."
                    .to_owned(),
            )
        })?;

        let base_url =
            std::env::var("OPENROUTER_BASE_URL").unwrap_or_else(|_| OPENROUTER_API_BASE.to_owned());
        Self::with_base_url(api_key, base_url)
    }

    pub fn new(api_key: impl Into<String>) -> Result<Self, CoreError> {
        Self::with_base_url(api_key, OPENROUTER_API_BASE)
    }

    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, CoreError> {
        let api_key = api_key.into().trim().to_owned();
        if api_key.trim().is_empty() {
            return Err(CoreError::Configuration(
                "OPENROUTER_API_KEY is empty. Provide a non-empty API key.".to_owned(),
            ));
        }

        let base_url = normalize_base_url(base_url.into());
        if base_url.is_empty() {
            return Err(CoreError::Configuration(
                "OPENROUTER_BASE_URL is empty. Provide a valid base URL.".to_owned(),
            ));
        }
        validate_base_url(&base_url)?;

        Ok(Self {
            api_key,
            base_url,
            http_client: reqwest::Client::new(),
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            stream_counter: Arc::new(AtomicU64::new(1)),
        })
    }

    fn next_stream_id(&self) -> String {
        let sequence = self.stream_counter.fetch_add(1, Ordering::Relaxed);
        format!("openrouter-stream-{sequence}")
    }

    fn stream_endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn validate_credentials(&self) -> Result<(), CoreError> {
        if self.api_key.is_empty() {
            return Err(CoreError::Configuration(
                "OpenRouter supervisor was initialized without credentials.".to_owned(),
            ));
        }

        Ok(())
    }

    fn insert_stream_control(&self, stream_id: &str) -> Arc<StreamCancellation> {
        let cancellation = Arc::new(StreamCancellation::default());
        let mut active = self
            .active_streams
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        active.insert(stream_id.to_owned(), cancellation.clone());
        cancellation
    }
}

#[async_trait::async_trait]
impl Supervisor for OpenRouterSupervisor {
    async fn health_check(&self) -> Result<(), CoreError> {
        self.validate_credentials()
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenRouterSupervisor {
    fn kind(&self) -> LlmProviderKind {
        LlmProviderKind::OpenRouter
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        self.validate_credentials()
    }

    async fn stream_chat(
        &self,
        request: LlmChatRequest,
    ) -> Result<(String, LlmResponseStream), CoreError> {
        self.validate_credentials()?;
        validate_chat_request(&request)?;

        let stream_id = self.next_stream_id();
        let cancellation = self.insert_stream_control(&stream_id);
        let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);

        let client = self.http_client.clone();
        let endpoint = self.stream_endpoint();
        let api_key = self.api_key.clone();
        let stream_id_for_task = stream_id.clone();
        let active_streams = self.active_streams.clone();
        tokio::spawn(async move {
            run_openrouter_stream(client, endpoint, api_key, request, cancellation, sender).await;

            let mut active = active_streams.lock().unwrap_or_else(|p| p.into_inner());
            active.remove(&stream_id_for_task);
        });

        Ok((stream_id, Box::new(ChannelLlmStream { receiver })))
    }

    async fn cancel_stream(&self, stream_id: &str) -> Result<(), CoreError> {
        let active = self
            .active_streams
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(cancellation) = active.get(stream_id) {
            cancellation.cancel();
        }

        Ok(())
    }
}

fn normalize_base_url(base_url: String) -> String {
    base_url.trim().trim_end_matches('/').to_owned()
}

fn validate_base_url(base_url: &str) -> Result<(), CoreError> {
    let parsed = reqwest::Url::parse(base_url).map_err(|error| {
        CoreError::Configuration(format!("OPENROUTER_BASE_URL is invalid: {error}"))
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(CoreError::Configuration(format!(
            "OPENROUTER_BASE_URL must use http or https, got scheme '{scheme}'."
        ))),
    }
}

fn validate_chat_request(request: &LlmChatRequest) -> Result<(), CoreError> {
    if request.model.trim().is_empty() {
        return Err(CoreError::Configuration(
            "LlmChatRequest.model is empty. Provide a non-empty model identifier.".to_owned(),
        ));
    }

    if request.messages.is_empty() {
        return Err(CoreError::Configuration(
            "LlmChatRequest.messages is empty. Provide at least one message.".to_owned(),
        ));
    }

    Ok(())
}

fn llm_role(role: LlmRole) -> &'static str {
    match role {
        LlmRole::System => "system",
        LlmRole::User => "user",
        LlmRole::Assistant => "assistant",
        LlmRole::Tool => "tool",
    }
}

fn as_openrouter_message(message: LlmMessage) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "role".to_owned(),
        Value::String(llm_role(message.role).to_owned()),
    );
    payload.insert("content".to_owned(), Value::String(message.content));
    if let Some(name) = message.name {
        payload.insert("name".to_owned(), Value::String(name));
    }

    Value::Object(payload)
}

fn request_body(request: LlmChatRequest) -> Value {
    let mut payload = Map::new();
    payload.insert("model".to_owned(), Value::String(request.model));
    payload.insert(
        "messages".to_owned(),
        Value::Array(
            request
                .messages
                .into_iter()
                .map(as_openrouter_message)
                .collect(),
        ),
    );
    payload.insert("stream".to_owned(), Value::Bool(true));

    if let Some(temperature) = request.temperature {
        payload.insert("temperature".to_owned(), Value::from(temperature));
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        payload.insert("max_tokens".to_owned(), Value::from(max_output_tokens));
    }

    Value::Object(payload)
}

async fn run_openrouter_stream(
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    request: LlmChatRequest,
    cancellation: Arc<StreamCancellation>,
    sender: mpsc::Sender<Result<LlmStreamChunk, CoreError>>,
) {
    if cancellation.is_cancelled() {
        let _ = sender
            .send(Ok(LlmStreamChunk {
                delta: String::new(),
                finish_reason: Some(LlmFinishReason::Cancelled),
                usage: None,
                rate_limit: None,
            }))
            .await;
        return;
    }

    let mut request_future = Box::pin(
        client
            .post(endpoint)
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&request_body(request))
            .send(),
    );

    let response = tokio::select! {
        _ = cancellation.notify.notified() => {
            let _ = sender.send(Ok(LlmStreamChunk {
                delta: String::new(),
                finish_reason: Some(LlmFinishReason::Cancelled),
                usage: None,
                rate_limit: None,
            })).await;
            return;
        }
        result = &mut request_future => {
            match result {
                Ok(response) => response,
                Err(error) => {
                    let _ = sender.send(Err(CoreError::DependencyUnavailable(
                        format!("OpenRouter request failed before streaming started: {error}"),
                    ))).await;
                    return;
                }
            }
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let _ = sender
            .send(Err(CoreError::DependencyUnavailable(format_http_error(
                status, &body,
            ))))
            .await;
        return;
    }

    let mut rate_limit = rate_limit_from_headers(response.headers());
    let mut parser = SseDataParser::default();
    let mut response = response;
    let mut finished = false;

    loop {
        if cancellation.is_cancelled() {
            let _ = sender
                .send(Ok(LlmStreamChunk {
                    delta: String::new(),
                    finish_reason: Some(LlmFinishReason::Cancelled),
                    usage: None,
                    rate_limit: None,
                }))
                .await;
            return;
        }

        let chunk_result = tokio::select! {
            _ = cancellation.notify.notified() => {
                let _ = sender.send(Ok(LlmStreamChunk {
                    delta: String::new(),
                    finish_reason: Some(LlmFinishReason::Cancelled),
                    usage: None,
                    rate_limit: None,
                })).await;
                return;
            }
            result = response.chunk() => result
        };

        match chunk_result {
            Ok(Some(chunk)) => {
                for event in parser.push(chunk.as_ref()) {
                    if event.trim() == "[DONE]" {
                        if !finished
                            && sender
                                .send(Ok(LlmStreamChunk {
                                    delta: String::new(),
                                    finish_reason: Some(LlmFinishReason::Stop),
                                    usage: None,
                                    rate_limit: rate_limit.take(),
                                }))
                                .await
                                .is_err()
                        {
                            return;
                        }
                        finished = true;
                        continue;
                    }

                    let mut parsed_chunks = match parse_stream_event(&event) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            return;
                        }
                    };
                    if let Some(first) = parsed_chunks.first_mut() {
                        first.rate_limit = rate_limit.take();
                    }
                    for parsed in parsed_chunks {
                        if parsed.finish_reason.is_some() {
                            finished = true;
                        }
                        if sender.send(Ok(parsed)).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Ok(None) => {
                let trailing_events = parser.finish();
                for event in trailing_events {
                    let mut parsed_chunks = match parse_stream_event(&event) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            return;
                        }
                    };
                    if let Some(first) = parsed_chunks.first_mut() {
                        first.rate_limit = rate_limit.take();
                    }
                    for parsed in parsed_chunks {
                        if parsed.finish_reason.is_some() {
                            finished = true;
                        }
                        if sender.send(Ok(parsed)).await.is_err() {
                            return;
                        }
                    }
                }

                if !finished {
                    let _ = sender
                        .send(Ok(LlmStreamChunk {
                            delta: String::new(),
                            finish_reason: Some(LlmFinishReason::Stop),
                            usage: None,
                            rate_limit: rate_limit.take(),
                        }))
                        .await;
                }
                return;
            }
            Err(error) => {
                let _ = sender
                    .send(Err(CoreError::DependencyUnavailable(format!(
                        "OpenRouter streaming transport failed: {error}"
                    ))))
                    .await;
                return;
            }
        }
    }
}

fn parse_stream_event(event: &str) -> Result<Vec<LlmStreamChunk>, CoreError> {
    let payload: Value = serde_json::from_str(event).map_err(|error| {
        CoreError::DependencyUnavailable(format!(
            "OpenRouter stream returned invalid JSON event: {error}"
        ))
    })?;

    if let Some(error) = payload.get("error") {
        let detail = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("OpenRouter returned an unspecified streaming error");
        return Err(CoreError::DependencyUnavailable(detail.to_owned()));
    }

    let usage = parse_usage(payload.get("usage"));
    let choices = payload
        .get("choices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut chunks = Vec::new();
    for choice in choices {
        let delta = choice
            .get("delta")
            .and_then(|delta| {
                delta
                    .get("content")
                    .and_then(Value::as_str)
                    .or_else(|| delta.as_str())
            })
            .unwrap_or_default()
            .to_owned();
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(to_finish_reason);

        if delta.is_empty() && finish_reason.is_none() && usage.is_none() {
            continue;
        }

        chunks.push(LlmStreamChunk {
            delta,
            finish_reason,
            usage: usage.clone(),
            rate_limit: None,
        });
    }

    if chunks.is_empty() {
        if let Some(usage) = usage {
            chunks.push(LlmStreamChunk {
                delta: String::new(),
                finish_reason: None,
                usage: Some(usage),
                rate_limit: None,
            });
        }
    }

    Ok(chunks)
}

fn parse_usage(value: Option<&Value>) -> Option<LlmTokenUsage> {
    let usage = value?;
    let input_tokens = usage_u32(usage, &["prompt_tokens", "input_tokens"])?;
    let output_tokens = usage_u32(usage, &["completion_tokens", "output_tokens"])?;
    let total_tokens = usage_u32(usage, &["total_tokens"])
        .unwrap_or_else(|| input_tokens.saturating_add(output_tokens));

    Some(LlmTokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}

fn usage_u32(usage: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        usage
            .get(*key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    })
}

fn to_finish_reason(reason: &str) -> LlmFinishReason {
    match reason {
        "stop" => LlmFinishReason::Stop,
        "length" => LlmFinishReason::Length,
        "tool_calls" | "tool_call" => LlmFinishReason::ToolCall,
        "content_filter" => LlmFinishReason::ContentFilter,
        "cancelled" | "canceled" => LlmFinishReason::Cancelled,
        _ => LlmFinishReason::Error,
    }
}

fn format_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| {
            json.get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    json.get("message")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
        })
        .unwrap_or_else(|| body.trim().to_owned());

    if detail.is_empty() {
        format!("OpenRouter request failed with status {status}")
    } else {
        format!("OpenRouter request failed with status {status}: {detail}")
    }
}

fn rate_limit_from_headers(headers: &HeaderMap) -> Option<LlmRateLimitState> {
    let requests_remaining = header_u32(
        headers,
        &[
            "x-ratelimit-remaining-requests",
            "x-ratelimit-requests-remaining",
            "x-ratelimit-remaining",
        ],
    );
    let tokens_remaining = header_u32(
        headers,
        &[
            "x-ratelimit-remaining-tokens",
            "x-ratelimit-tokens-remaining",
        ],
    );
    let reset_at = header_string(
        headers,
        &["x-ratelimit-reset", "x-ratelimit-reset-requests"],
    );

    if requests_remaining.is_none() && tokens_remaining.is_none() && reset_at.is_none() {
        return None;
    }

    Some(LlmRateLimitState {
        requests_remaining,
        tokens_remaining,
        reset_at,
    })
}

fn header_u32(headers: &HeaderMap, names: &[&str]) -> Option<u32> {
    names.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u32>().ok())
    })
}

fn header_string(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::test_support::with_env_var;
    use orchestrator_core::{LlmProvider, LlmRole};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn missing_openrouter_api_key_is_actionable() {
        with_env_var("OPENROUTER_API_KEY", None, || {
            let result = OpenRouterSupervisor::from_env();
            let err = result.expect_err("expected missing env var to fail");
            assert!(err.to_string().contains("OPENROUTER_API_KEY"));
        });
    }

    #[test]
    fn empty_openrouter_api_key_is_rejected() {
        with_env_var("OPENROUTER_API_KEY", Some("   "), || {
            let result = OpenRouterSupervisor::from_env();
            assert!(result.is_err());
        });
    }

    #[test]
    fn invalid_openrouter_base_url_is_rejected() {
        let invalid = OpenRouterSupervisor::with_base_url("test-key", "not a url");
        assert!(invalid.is_err());

        let unsupported = OpenRouterSupervisor::with_base_url("test-key", "ftp://localhost");
        assert!(unsupported.is_err());
    }

    #[tokio::test]
    async fn stream_chat_rejects_invalid_requests() {
        let provider = OpenRouterSupervisor::with_base_url("test-key", "http://127.0.0.1:8080")
            .expect("provider");

        let empty_model = match provider
            .stream_chat(LlmChatRequest {
                model: "   ".to_owned(),
                messages: vec![LlmMessage {
                    role: LlmRole::User,
                    content: "hello".to_owned(),
                    name: None,
                }],
                temperature: None,
                max_output_tokens: None,
            })
            .await
        {
            Ok(_) => panic!("expected empty model to fail"),
            Err(error) => error,
        };
        assert!(empty_model.to_string().contains("model"));

        let empty_messages = match provider
            .stream_chat(LlmChatRequest {
                model: "openrouter/mock-model".to_owned(),
                messages: Vec::new(),
                temperature: None,
                max_output_tokens: None,
            })
            .await
        {
            Ok(_) => panic!("expected empty messages to fail"),
            Err(error) => error,
        };
        assert!(empty_messages.to_string().contains("messages"));
    }

    #[tokio::test]
    async fn stream_chat_yields_incremental_chunks_with_usage_and_rate_limit() {
        let base_url = spawn_test_server(|stream| {
            write_http_response(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nx-ratelimit-remaining-requests: 49\r\nx-ratelimit-remaining-tokens: 2048\r\n\r\n",
                &[
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
                    "data: [DONE]\n\n",
                ],
                None,
            );
        });

        let provider = OpenRouterSupervisor::with_base_url("test-key", base_url).expect("provider");
        let request = LlmChatRequest {
            model: "openrouter/mock-model".to_owned(),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "Say hello".to_owned(),
                name: None,
            }],
            temperature: Some(0.2),
            max_output_tokens: Some(32),
        };

        let (_stream_id, mut stream) = provider.stream_chat(request).await.expect("stream chat");
        let first = stream
            .next_chunk()
            .await
            .expect("first stream chunk")
            .expect("first chunk value");
        assert_eq!(first.delta, "Hel");
        assert!(first.finish_reason.is_none());
        let rate_limit = first.rate_limit.expect("rate limit in first chunk");
        assert_eq!(rate_limit.requests_remaining, Some(49));
        assert_eq!(rate_limit.tokens_remaining, Some(2048));

        let second = stream
            .next_chunk()
            .await
            .expect("second stream chunk")
            .expect("second chunk value");
        assert_eq!(second.delta, "lo");
        assert_eq!(second.finish_reason, Some(LlmFinishReason::Stop));
        assert_eq!(
            second.usage,
            Some(LlmTokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                total_tokens: 5,
            })
        );
        assert!(second.rate_limit.is_none());

        let maybe_done = stream.next_chunk().await.expect("stream close");
        assert!(maybe_done.is_none());
    }

    #[tokio::test]
    async fn cancel_stream_emits_cancelled_finish_reason() {
        let base_url = spawn_test_server(|stream| {
            write_http_response(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                &[
                    "data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"second\"}}]}\n\n",
                    "data: [DONE]\n\n",
                ],
                Some(Duration::from_millis(250)),
            );
        });

        let provider = OpenRouterSupervisor::with_base_url("test-key", base_url).expect("provider");
        let request = LlmChatRequest {
            model: "openrouter/mock-model".to_owned(),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "Cancel me".to_owned(),
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
        };

        let (stream_id, mut stream) = provider.stream_chat(request).await.expect("stream chat");
        let first = stream
            .next_chunk()
            .await
            .expect("first stream chunk")
            .expect("first chunk value");
        assert_eq!(first.delta, "first");

        provider
            .cancel_stream(&stream_id)
            .await
            .expect("cancel stream");

        let cancelled = stream
            .next_chunk()
            .await
            .expect("cancelled chunk")
            .expect("cancelled chunk value");
        assert_eq!(cancelled.finish_reason, Some(LlmFinishReason::Cancelled));
        assert_eq!(cancelled.delta, "");
    }

    #[tokio::test]
    async fn stream_chat_surfaces_http_errors_on_stream() {
        let base_url = spawn_test_server(|mut stream| {
            let response = "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":{\"message\":\"rate limit exceeded for test\"}}";
            stream
                .write_all(response.as_bytes())
                .expect("write error response");
            stream.flush().expect("flush error response");
        });

        let provider = OpenRouterSupervisor::with_base_url("test-key", base_url).expect("provider");
        let request = LlmChatRequest {
            model: "openrouter/mock-model".to_owned(),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "Trigger failure".to_owned(),
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
        };

        let (_stream_id, mut stream) = provider.stream_chat(request).await.expect("stream chat");
        let error = stream
            .next_chunk()
            .await
            .expect_err("expected stream error from HTTP failure");
        assert!(error.to_string().contains("429"));
        assert!(error.to_string().contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn stream_chat_emits_stop_when_done_arrives_without_finish_reason() {
        let base_url = spawn_test_server(|stream| {
            write_http_response(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                &[
                    "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                    "data: [DONE]\n\n",
                ],
                None,
            );
        });

        let provider = OpenRouterSupervisor::with_base_url("test-key", base_url).expect("provider");
        let request = LlmChatRequest {
            model: "openrouter/mock-model".to_owned(),
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: "stop on done".to_owned(),
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
        };

        let (_stream_id, mut stream) = provider.stream_chat(request).await.expect("stream chat");
        let first = stream
            .next_chunk()
            .await
            .expect("first stream chunk")
            .expect("first chunk value");
        assert_eq!(first.delta, "partial");
        assert!(first.finish_reason.is_none());

        let second = stream
            .next_chunk()
            .await
            .expect("stop stream chunk")
            .expect("stop chunk value");
        assert_eq!(second.finish_reason, Some(LlmFinishReason::Stop));
        assert_eq!(second.delta, "");

        let end = stream.next_chunk().await.expect("stream close");
        assert!(end.is_none());
    }

    #[test]
    fn sse_parser_emits_events_across_chunk_boundaries() {
        let mut parser = SseDataParser::default();
        let first = parser.push(b"data: one\n\nda");
        assert_eq!(first, vec!["one".to_owned()]);

        let second = parser.push(b"ta: two\n\n");
        assert_eq!(second, vec!["two".to_owned()]);
    }

    #[test]
    fn sse_parser_supports_comments_and_multiline_data_fields() {
        let mut parser = SseDataParser::default();
        assert!(parser.push(b":keepalive\n").is_empty());

        let events = parser.push(b"data: line one\ndata: line two\n\n");
        assert_eq!(events, vec!["line one\nline two".to_owned()]);
    }

    #[test]
    fn sse_parser_finish_flushes_partial_event() {
        let mut parser = SseDataParser::default();
        assert!(parser.push(b"data: partial event").is_empty());
        assert_eq!(parser.finish(), vec!["partial event".to_owned()]);
    }

    #[test]
    fn sse_parser_finish_trims_partial_line_carriage_return() {
        let mut parser = SseDataParser::default();
        assert!(parser.push(b"data: partial event\r").is_empty());
        assert_eq!(parser.finish(), vec!["partial event".to_owned()]);
    }

    #[test]
    fn parse_stream_event_table_driven_edges() {
        enum Expected<'a> {
            Chunk {
                delta: &'a str,
                finish_reason: Option<LlmFinishReason>,
                usage: Option<LlmTokenUsage>,
            },
            ErrorContains(&'a str),
        }

        struct Case<'a> {
            name: &'a str,
            event: &'a str,
            expected: Expected<'a>,
        }

        let cases = vec![
            Case {
                name: "delta as plain string is accepted",
                event: r#"{"choices":[{"delta":"hello"}]}"#,
                expected: Expected::Chunk {
                    delta: "hello",
                    finish_reason: None,
                    usage: None,
                },
            },
            Case {
                name: "unknown finish reason maps to error sentinel",
                event: r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"mystery"}]}"#,
                expected: Expected::Chunk {
                    delta: "x",
                    finish_reason: Some(LlmFinishReason::Error),
                    usage: None,
                },
            },
            Case {
                name: "usage-only event still emits a chunk",
                event: r#"{"choices":[],"usage":{"input_tokens":2,"output_tokens":3}}"#,
                expected: Expected::Chunk {
                    delta: "",
                    finish_reason: None,
                    usage: Some(LlmTokenUsage {
                        input_tokens: 2,
                        output_tokens: 3,
                        total_tokens: 5,
                    }),
                },
            },
            Case {
                name: "error payload is surfaced",
                event: r#"{"error":{"message":"quota exhausted"}}"#,
                expected: Expected::ErrorContains("quota exhausted"),
            },
            Case {
                name: "invalid json is rejected",
                event: r#"{"choices":[{"delta":{"content":"broken"}}]"#,
                expected: Expected::ErrorContains("invalid JSON"),
            },
        ];

        for case in cases {
            match case.expected {
                Expected::Chunk {
                    delta,
                    finish_reason,
                    usage,
                } => {
                    let chunks = parse_stream_event(case.event)
                        .unwrap_or_else(|error| panic!("case '{}' failed: {error}", case.name));
                    assert_eq!(
                        chunks.len(),
                        1,
                        "case '{}' should emit one chunk",
                        case.name
                    );
                    let chunk = &chunks[0];
                    assert_eq!(chunk.delta, delta, "case '{}' delta mismatch", case.name);
                    assert_eq!(
                        chunk.finish_reason, finish_reason,
                        "case '{}' finish reason mismatch",
                        case.name
                    );
                    assert_eq!(chunk.usage, usage, "case '{}' usage mismatch", case.name);
                }
                Expected::ErrorContains(needle) => {
                    let error = parse_stream_event(case.event)
                        .expect_err("case should produce parser error");
                    assert!(
                        error.to_string().contains(needle),
                        "case '{}' expected error containing '{needle}', got '{error}'",
                        case.name
                    );
                }
            }
        }
    }

    #[test]
    fn parse_stream_event_derives_total_tokens_when_missing() {
        let event = r#"{"choices":[{"delta":{"content":"hi"}}],"usage":{"prompt_tokens":4,"completion_tokens":2}}"#;
        let chunks = parse_stream_event(event).expect("parse stream event");
        assert_eq!(
            chunks.first().and_then(|chunk| chunk.usage.clone()),
            Some(LlmTokenUsage {
                input_tokens: 4,
                output_tokens: 2,
                total_tokens: 6,
            })
        );
    }

    #[test]
    fn parse_stream_event_ignores_usage_values_above_u32() {
        let event = r#"{"choices":[{"delta":{"content":"hi"}}],"usage":{"prompt_tokens":4294967296,"completion_tokens":2,"total_tokens":4294967298}}"#;
        let chunks = parse_stream_event(event).expect("parse stream event");
        assert_eq!(chunks.first().and_then(|chunk| chunk.usage.clone()), None);
    }

    #[tokio::test]
    async fn cancel_stream_is_idempotent_for_unknown_stream_id() {
        let provider = OpenRouterSupervisor::with_base_url("test-key", "http://127.0.0.1:8080")
            .expect("provider");
        provider
            .cancel_stream("openrouter-stream-missing")
            .await
            .expect("unknown stream id should be ignored");
    }

    #[tokio::test]
    async fn cancel_stream_marks_registered_stream_as_cancelled() {
        let provider = OpenRouterSupervisor::with_base_url("test-key", "http://127.0.0.1:8080")
            .expect("provider");
        let control = provider.insert_stream_control("openrouter-stream-test");
        assert!(!control.is_cancelled(), "stream should start as active");

        provider
            .cancel_stream("openrouter-stream-test")
            .await
            .expect("cancel should succeed");
        assert!(control.is_cancelled(), "stream should be marked cancelled");
    }

    fn spawn_test_server(handler: impl FnOnce(TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("server address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            read_http_request(&mut stream);
            handler(stream);
        });

        format!("http://{address}")
    }

    fn read_http_request(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut buffer = [0_u8; 2048];
        let mut request = Vec::new();
        let mut header_end = None;
        loop {
            let read = stream.read(&mut buffer).expect("read request bytes");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = find_header_end(&request) {
                header_end = Some(index);
                break;
            }
        }

        let Some(header_end) = header_end else {
            return;
        };
        let headers = &request[..header_end];
        let content_length = parse_content_length(headers);
        let body_already_read = request.len().saturating_sub(header_end + 4);

        if content_length > body_already_read {
            let mut remaining = content_length - body_already_read;
            while remaining > 0 {
                let read = stream.read(&mut buffer).expect("read request body");
                if read == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(read);
            }
        }
    }

    fn write_http_response(
        mut stream: TcpStream,
        headers: &str,
        sse_events: &[&str],
        per_event_delay: Option<Duration>,
    ) {
        stream
            .write_all(headers.as_bytes())
            .expect("write response headers");
        stream.flush().expect("flush response headers");

        for event in sse_events {
            let chunk_header = format!("{:X}\r\n", event.len());
            stream
                .write_all(chunk_header.as_bytes())
                .expect("write chunk size");
            stream.write_all(event.as_bytes()).expect("write chunk");
            stream.write_all(b"\r\n").expect("write chunk suffix");
            stream.flush().expect("flush chunk");
            if let Some(delay) = per_event_delay {
                thread::sleep(delay);
            }
        }

        stream
            .write_all(b"0\r\n\r\n")
            .expect("write terminal chunk");
        stream.flush().expect("flush terminal chunk");
    }

    fn parse_content_length(headers: &[u8]) -> usize {
        let text = String::from_utf8_lossy(headers);
        text.lines()
            .find_map(|line| {
                let mut split = line.splitn(2, ':');
                let name = split.next()?.trim();
                let value = split.next()?.trim();
                if name.eq_ignore_ascii_case("content-length") {
                    return value.parse::<usize>().ok();
                }
                None
            })
            .unwrap_or(0)
    }

    fn find_header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }
}
