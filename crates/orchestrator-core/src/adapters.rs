use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, ArtifactKind, CoreError, TicketId, TicketProvider, WorkerSessionId, WorktreeId,
};

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    OpenCode,
    Codex,
    ClaudeCode,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub structured_events: bool,
    pub session_export: bool,
    pub diff_provider: bool,
    pub supports_background: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub session_id: WorkerSessionId,
    pub workdir: PathBuf,
    pub model: Option<String>,
    pub instruction_prelude: Option<String>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandle {
    pub session_id: WorkerSessionId,
    pub backend: BackendKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendOutputEvent {
    pub stream: BackendOutputStream,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCheckpointEvent {
    pub summary: String,
    pub detail: Option<String>,
    pub file_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendNeedsInputEvent {
    pub prompt_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub default_option: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendBlockedEvent {
    pub reason: String,
    pub hint: Option<String>,
    pub log_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendArtifactEvent {
    pub kind: ArtifactKind,
    pub artifact_id: Option<ArtifactId>,
    pub label: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendDoneEvent {
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCrashedEvent {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendEvent {
    Output(BackendOutputEvent),
    Checkpoint(BackendCheckpointEvent),
    NeedsInput(BackendNeedsInputEvent),
    Blocked(BackendBlockedEvent),
    Artifact(BackendArtifactEvent),
    Done(BackendDoneEvent),
    Crashed(BackendCrashedEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub lines: Vec<String>,
}

#[async_trait]
pub trait WorkerEventSubscription: Send {
    async fn next_event(&mut self) -> Result<Option<BackendEvent>, CoreError>;
}

pub type WorkerEventStream = Box<dyn WorkerEventSubscription>;

#[async_trait]
pub trait WorkerBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> BackendCapabilities;

    async fn health_check(&self) -> Result<(), CoreError>;
    async fn spawn(&self, spec: SpawnSpec) -> Result<SessionHandle, CoreError>;
    async fn kill(&self, session: &SessionHandle) -> Result<(), CoreError>;
    async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> Result<(), CoreError>;
    async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> Result<(), CoreError>;
    async fn subscribe(&self, session: &SessionHandle) -> Result<WorkerEventStream, CoreError>;
    async fn snapshot(&self, session: &SessionHandle) -> Result<TerminalSnapshot, CoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TicketQuery {
    pub assigned_to_me: bool,
    pub states: Vec<String>,
    pub search: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSummary {
    pub ticket_id: TicketId,
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub url: String,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTicketRequest {
    pub title: String,
    pub description: Option<String>,
    pub state: Option<String>,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTicketStateRequest {
    pub ticket_id: TicketId,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketAttachment {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddTicketCommentRequest {
    pub ticket_id: TicketId,
    pub comment: String,
    pub attachments: Vec<TicketAttachment>,
}

#[async_trait]
pub trait TicketingProvider: Send + Sync {
    fn provider(&self) -> TicketProvider;
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn list_tickets(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError>;
    async fn create_ticket(&self, request: CreateTicketRequest)
        -> Result<TicketSummary, CoreError>;
    async fn update_ticket_state(&self, request: UpdateTicketStateRequest)
        -> Result<(), CoreError>;
    async fn add_comment(&self, request: AddTicketCommentRequest) -> Result<(), CoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorktreeRequest {
    pub worktree_id: WorktreeId,
    pub repository: RepositoryRef,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_branch: String,
    pub ticket_identifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSummary {
    pub worktree_id: WorktreeId,
    pub repository: RepositoryRef,
    pub path: PathBuf,
    pub branch: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteWorktreeRequest {
    pub worktree: WorktreeSummary,
    pub delete_branch: bool,
    pub delete_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeStatus {
    pub is_dirty: bool,
    pub commits_ahead: u32,
    pub commits_behind: u32,
}

#[async_trait]
pub trait VcsProvider: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn discover_repositories(
        &self,
        roots: &[PathBuf],
    ) -> Result<Vec<RepositoryRef>, CoreError>;
    async fn create_worktree(
        &self,
        request: CreateWorktreeRequest,
    ) -> Result<WorktreeSummary, CoreError>;
    async fn delete_worktree(&self, request: DeleteWorktreeRequest) -> Result<(), CoreError>;
    async fn worktree_status(&self, worktree_path: &Path) -> Result<WorktreeStatus, CoreError>;
}

pub trait WorktreeManager: VcsProvider {}

impl<T: VcsProvider + ?Sized> WorktreeManager for T {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeHostKind {
    Github,
    Gitlab,
    Bitbucket,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePullRequestRequest {
    pub repository: RepositoryRef,
    pub title: String,
    pub body: String,
    pub base_branch: String,
    pub head_branch: String,
    pub ticket: Option<TicketId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repository: RepositoryRef,
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    pub reference: PullRequestRef,
    pub title: String,
    pub is_draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReviewerRequest {
    pub users: Vec<String>,
    pub teams: Vec<String>,
}

#[async_trait]
pub trait CodeHostProvider: Send + Sync {
    fn kind(&self) -> CodeHostKind;
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn create_draft_pull_request(
        &self,
        request: CreatePullRequestRequest,
    ) -> Result<PullRequestSummary, CoreError>;
    async fn mark_ready_for_review(&self, pr: &PullRequestRef) -> Result<(), CoreError>;
    async fn request_reviewers(
        &self,
        pr: &PullRequestRef,
        reviewers: ReviewerRequest,
    ) -> Result<(), CoreError>;
    async fn list_waiting_for_my_review(&self) -> Result<Vec<PullRequestSummary>, CoreError>;
}

#[async_trait]
pub trait UrlOpener: Send + Sync {
    async fn open_url(&self, url: &str) -> Result<(), CoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmProviderKind {
    OpenRouter,
    OpenAI,
    Anthropic,
    Local,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmChatRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmFinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRateLimitState {
    pub requests_remaining: Option<u32>,
    pub tokens_remaining: Option<u32>,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmStreamChunk {
    pub delta: String,
    pub finish_reason: Option<LlmFinishReason>,
    pub usage: Option<LlmTokenUsage>,
    pub rate_limit: Option<LlmRateLimitState>,
}

#[async_trait]
pub trait LlmResponseSubscription: Send {
    async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError>;
}

pub type LlmResponseStream = Box<dyn LlmResponseSubscription>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn kind(&self) -> LlmProviderKind;
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn stream_chat(
        &self,
        request: LlmChatRequest,
    ) -> Result<(String, LlmResponseStream), CoreError>;
    async fn cancel_stream(&self, stream_id: &str) -> Result<(), CoreError>;
}
