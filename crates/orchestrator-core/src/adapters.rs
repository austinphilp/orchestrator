use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use serde::{Deserialize, Serialize};

use crate::{CoreError, TicketId, TicketProvider, WorktreeId};

const DEFAULT_BASE_BRANCH: &str = "main";

fn default_base_branch() -> String {
    DEFAULT_BASE_BRANCH.to_owned()
}

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn health_check(&self) -> Result<(), CoreError>;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub state: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTicketRequest {
    pub ticket_id: TicketId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketDetails {
    #[serde(flatten)]
    pub summary: TicketSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTicketDescriptionRequest {
    pub ticket_id: TicketId,
    pub description: String,
}

#[async_trait]
pub trait TicketingProvider: Send + Sync {
    fn provider(&self) -> TicketProvider;
    async fn health_check(&self) -> Result<(), CoreError>;
    async fn list_tickets(&self, query: TicketQuery) -> Result<Vec<TicketSummary>, CoreError>;
    async fn create_ticket(&self, request: CreateTicketRequest)
        -> Result<TicketSummary, CoreError>;
    async fn get_ticket(&self, request: GetTicketRequest)
        -> Result<TicketDetails, CoreError> {
        Err(CoreError::DependencyUnavailable(format!(
            "get_ticket is not implemented by {:?} provider",
            self.provider()
        )))
    }
    async fn update_ticket_state(&self, request: UpdateTicketStateRequest)
        -> Result<(), CoreError>;
    async fn update_ticket_description(
        &self,
        request: UpdateTicketDescriptionRequest,
    ) -> Result<(), CoreError> {
        Err(CoreError::DependencyUnavailable(format!(
            "update_ticket_description is not implemented by {:?} provider",
            self.provider()
        )))
    }
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
    #[serde(default = "default_base_branch")]
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
    #[serde(default)]
    pub delete_branch: bool,
    #[serde(default)]
    pub delete_directory: bool,
}

impl DeleteWorktreeRequest {
    pub fn non_destructive(worktree: WorktreeSummary) -> Self {
        Self {
            worktree,
            delete_branch: false,
            delete_directory: false,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_worktree_request_defaults_base_branch_to_main() {
        let request: CreateWorktreeRequest = serde_json::from_str(
            r#"{
                "worktree_id":"wt-1",
                "repository":{"id":"repo-1","name":"repo","root":"/tmp/repo"},
                "worktree_path":"/tmp/worktree",
                "branch":"ap/AP-123-ticket-setup"
            }"#,
        )
        .expect("deserialize create worktree request");

        assert_eq!(request.base_branch, "main");
    }

    #[test]
    fn delete_worktree_request_defaults_to_non_destructive_cleanup() {
        let request: DeleteWorktreeRequest = serde_json::from_str(
            r#"{
                "worktree":{
                    "worktree_id":"wt-1",
                    "repository":{"id":"repo-1","name":"repo","root":"/tmp/repo"},
                    "path":"/tmp/worktree",
                    "branch":"ap/AP-123-ticket-setup",
                    "base_branch":"main"
                }
            }"#,
        )
        .expect("deserialize delete worktree request");

        assert!(!request.delete_branch);
        assert!(!request.delete_directory);
    }

    #[test]
    fn delete_worktree_request_non_destructive_constructor_disables_deletion_flags() {
        let worktree = WorktreeSummary {
            worktree_id: WorktreeId::new("wt-1"),
            repository: RepositoryRef {
                id: "repo-1".to_owned(),
                name: "repo".to_owned(),
                root: PathBuf::from("/tmp/repo"),
            },
            path: PathBuf::from("/tmp/worktree"),
            branch: "ap/AP-123-ticket-setup".to_owned(),
            base_branch: "main".to_owned(),
        };

        let request = DeleteWorktreeRequest::non_destructive(worktree);
        assert!(!request.delete_branch);
        assert!(!request.delete_directory);
    }
}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<LlmToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolFunction {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: LlmToolFunction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolResult {
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LlmToolChoice {
    Auto(String),
    None(String),
    Required(String),
    Specific { function: LlmToolChoiceFunction },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolChoiceFunction {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolCallOutput {
    pub tool_call_id: String,
    pub output: String,
}

impl Default for LlmToolChoice {
    fn default() -> Self {
        Self::Auto("auto".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmChatRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<LlmTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<LlmToolCall>,
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
