use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::interface::{
    CodeHostKind, CoreCodeHostProvider, CoreError, CoreGithubClient, CreatePullRequestRequest,
    PullRequestCiStatus, PullRequestMergeState, PullRequestRef, PullRequestReviewSummary,
    PullRequestSummary, RepositoryRef, ReviewerRequest, UrlOpener,
};
use crate::interface::{VcsRepoProvider, VcsRepoProviderKind};
use serde::Deserialize;
use serde_json::Value;

const ENV_GH_BIN: &str = "ORCHESTRATOR_GH_BIN";
const ENV_ALLOW_UNSAFE_COMMAND_PATHS: &str = "ORCHESTRATOR_ALLOW_UNSAFE_COMMAND_PATHS";
const DEFAULT_REVIEW_QUEUE_LIMIT: &str = "100";

pub trait CommandRunner: Send + Sync {
    fn run(
        &self,
        program: &str,
        args: &[OsString],
        cwd: Option<&Path>,
    ) -> io::Result<std::process::Output>;
}

#[derive(Debug, Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[OsString],
        cwd: Option<&Path>,
    ) -> io::Result<std::process::Output> {
        let mut command = Command::new(program);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        command.output()
    }
}

pub fn default_system_url_opener() -> Result<SystemUrlOpener<ProcessCommandRunner>, CoreError> {
    SystemUrlOpener::new(ProcessCommandRunner)
}

pub struct SystemUrlOpener<R: CommandRunner> {
    runner: R,
    command: UrlOpenCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UrlOpenCommand {
    program: PathBuf,
    prefix_args: Vec<OsString>,
}

impl<R: CommandRunner> SystemUrlOpener<R> {
    pub fn new(runner: R) -> Result<Self, CoreError> {
        let command = Self::command_for_os(std::env::consts::OS)?;
        Ok(Self { runner, command })
    }

    fn command_for_os(target_os: &str) -> Result<UrlOpenCommand, CoreError> {
        match target_os {
            "macos" => Ok(UrlOpenCommand {
                program: PathBuf::from("open"),
                prefix_args: Vec::new(),
            }),
            "linux" => Ok(UrlOpenCommand {
                program: PathBuf::from("xdg-open"),
                prefix_args: Vec::new(),
            }),
            "windows" => Ok(UrlOpenCommand {
                program: PathBuf::from("cmd"),
                prefix_args: vec![
                    OsString::from("/C"),
                    OsString::from("start"),
                    OsString::from(""),
                ],
            }),
            _ => Err(CoreError::DependencyUnavailable(format!(
                "URL opening is unsupported on `{target_os}`. Supported platforms are macOS, Linux, and Windows."
            ))),
        }
    }

    fn open_args_for_url(&self, url: &str) -> Vec<OsString> {
        let mut args = self.command.prefix_args.clone();
        args.push(OsString::from(url));
        args
    }

    fn run_open_raw(&self, args: &[OsString]) -> Result<std::process::Output, CoreError> {
        let program =
            self.command.program.to_str().ok_or_else(|| {
                CoreError::Configuration("Invalid URL opener binary path".to_owned())
            })?;
        self.runner
            .run(program, args, None)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CoreError::DependencyUnavailable(format!(
                    "URL opener `{}` was not found. Install it and retry.",
                    self.command.program.display()
                )),
                _ => CoreError::DependencyUnavailable(format!(
                    "Failed to execute URL opener `{}`: {error}",
                    self.command.program.display()
                )),
            })
    }

    fn ensure_non_empty_url(url: &str) -> Result<&str, CoreError> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err(CoreError::Configuration(
                "URL must be a non-empty string.".to_owned(),
            ));
        }

        Ok(trimmed)
    }

    fn command_failed(&self, args: &[OsString], output: &std::process::Output) -> CoreError {
        CoreError::DependencyUnavailable(format!(
            "URL opener command failed (`{} {}`): {}",
            self.command.program.display(),
            Self::render_args(args),
            Self::command_output_detail(output)
        ))
    }

    fn render_args(args: &[OsString]) -> String {
        args.iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn command_output_detail(output: &std::process::Output) -> String {
        let stderr = sanitize_command_output_text(String::from_utf8_lossy(&output.stderr).as_ref());
        let stderr = stderr.trim().to_owned();
        if !stderr.is_empty() {
            return stderr;
        }

        let stdout = sanitize_command_output_text(String::from_utf8_lossy(&output.stdout).as_ref());
        let stdout = stdout.trim().to_owned();
        if !stdout.is_empty() {
            return stdout;
        }

        format!("exit status {}", output.status)
    }
}

#[async_trait::async_trait]
impl<R: CommandRunner> UrlOpener for SystemUrlOpener<R> {
    async fn open_url(&self, url: &str) -> Result<(), CoreError> {
        let url = Self::ensure_non_empty_url(url)?;
        let args = self.open_args_for_url(url);
        let output = self.run_open_raw(&args)?;
        if output.status.success() {
            return Ok(());
        }

        Err(self.command_failed(&args, &output))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubGhCliRepoProviderConfig {
    pub binary: PathBuf,
    pub allow_unsafe_command_paths: bool,
}

impl Default for GitHubGhCliRepoProviderConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("gh"),
            allow_unsafe_command_paths: false,
        }
    }
}

pub struct GitHubGhCliRepoProvider<R: CommandRunner = ProcessCommandRunner> {
    runner: R,
    binary: PathBuf,
    allow_unsafe_command_paths: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GhMergeStrategy {
    Merge,
    Squash,
    Rebase,
}

impl GhMergeStrategy {
    const fn flag(self) -> &'static str {
        match self {
            Self::Merge => "--merge",
            Self::Squash => "--squash",
            Self::Rebase => "--rebase",
        }
    }
}

impl<R: CommandRunner> GitHubGhCliRepoProvider<R> {
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn new(
        runner: R,
        binary: PathBuf,
        allow_unsafe_command_paths: bool,
    ) -> Result<Self, CoreError> {
        if binary.as_os_str().is_empty() {
            return Err(CoreError::Configuration(format!(
                "{ENV_GH_BIN} is set but empty. Provide a valid gh binary path or unset it."
            )));
        }
        Ok(Self {
            runner,
            binary,
            allow_unsafe_command_paths,
        })
    }

    pub fn health_check_args() -> Vec<OsString> {
        vec![OsString::from("auth"), OsString::from("status")]
    }

    fn create_draft_pr_args(request: &CreatePullRequestRequest) -> Vec<OsString> {
        vec![
            OsString::from("pr"),
            OsString::from("create"),
            OsString::from("--draft"),
            OsString::from("--title"),
            OsString::from(request.title.as_str()),
            OsString::from("--body"),
            OsString::from(request.body.as_str()),
            OsString::from("--base"),
            OsString::from(request.base_branch.as_str()),
            OsString::from("--head"),
            OsString::from(request.head_branch.as_str()),
        ]
    }

    fn mark_ready_args(pr: &PullRequestRef) -> Vec<OsString> {
        vec![
            OsString::from("pr"),
            OsString::from("ready"),
            OsString::from(pr.number.to_string()),
        ]
    }

    fn merge_pull_request_args(
        pr: &PullRequestRef,
        strategy: Option<GhMergeStrategy>,
    ) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("pr"),
            OsString::from("merge"),
            OsString::from(pr.number.to_string()),
        ];
        if let Some(strategy) = strategy {
            args.push(OsString::from(strategy.flag()));
        }
        args.push(OsString::from("--delete-branch"));
        args
    }

    fn pull_request_merge_state_args(pr: &PullRequestRef) -> Vec<OsString> {
        vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(pr.number.to_string()),
            OsString::from("--json"),
            OsString::from(
                "mergedAt,isDraft,state,reviewDecision,latestReviews,mergeStateStatus,baseRefName,headRefName",
            ),
        ]
    }

    fn pull_request_for_branch_args(head_branch: &str) -> Vec<OsString> {
        vec![
            OsString::from("pr"),
            OsString::from("list"),
            OsString::from("--state"),
            OsString::from("open"),
            OsString::from("--head"),
            OsString::from(head_branch),
            OsString::from("--json"),
            OsString::from("number,url"),
            OsString::from("--limit"),
            OsString::from("1"),
        ]
    }

    fn pull_request_ci_statuses_args(pr: &PullRequestRef) -> Vec<OsString> {
        vec![
            OsString::from("pr"),
            OsString::from("checks"),
            OsString::from(pr.number.to_string()),
            OsString::from("--required"),
            OsString::from("--json"),
            OsString::from("name,bucket,state,link,workflow"),
        ]
    }

    fn request_reviewers_args(pr: &PullRequestRef, reviewers: &ReviewerRequest) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("pr"),
            OsString::from("edit"),
            OsString::from(pr.number.to_string()),
        ];
        let mut seen = HashSet::new();

        for reviewer in reviewers
            .users
            .iter()
            .chain(reviewers.teams.iter())
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            let canonical = reviewer.to_ascii_lowercase();
            if !seen.insert(canonical) {
                continue;
            }

            args.push(OsString::from("--add-reviewer"));
            args.push(OsString::from(reviewer));
        }

        args
    }

    fn review_queue_args() -> Vec<OsString> {
        vec![
            OsString::from("search"),
            OsString::from("prs"),
            OsString::from("--state"),
            OsString::from("open"),
            OsString::from("--review-requested"),
            OsString::from("@me"),
            OsString::from("--json"),
            OsString::from("number,title,url,isDraft,repository"),
            OsString::from("--limit"),
            OsString::from(DEFAULT_REVIEW_QUEUE_LIMIT),
        ]
    }

    fn run_gh_raw(
        &self,
        args: &[OsString],
        cwd: Option<&Path>,
    ) -> Result<std::process::Output, CoreError> {
        validate_command_binary_path(&self.binary, ENV_GH_BIN, self.allow_unsafe_command_paths)?;
        let program = self
            .binary
            .to_str()
            .ok_or_else(|| CoreError::Configuration("Invalid gh binary path".to_owned()))?;
        self.runner
            .run(program, args, cwd)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CoreError::DependencyUnavailable(
                    format!(
                        "GitHub CLI `{}` was not found. Install gh and authenticate with `gh auth login`.",
                        self.binary.display()
                    ),
                ),
                _ => CoreError::DependencyUnavailable(format!(
                    "Failed to execute GitHub CLI `{}`: {error}",
                    self.binary.display()
                )),
            })
    }

    fn run_gh(
        &self,
        args: &[OsString],
        cwd: Option<&Path>,
    ) -> Result<std::process::Output, CoreError> {
        let output = self.run_gh_raw(args, cwd)?;
        if output.status.success() {
            return Ok(output);
        }

        Err(self.command_failed(args, &output))
    }

    fn command_failed(&self, args: &[OsString], output: &std::process::Output) -> CoreError {
        CoreError::DependencyUnavailable(format!(
            "GitHub CLI command failed (`{} {}`): {}",
            self.binary.display(),
            Self::render_args(args),
            Self::command_output_detail(output)
        ))
    }

    fn render_args(args: &[OsString]) -> String {
        args.iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn command_output_detail(output: &std::process::Output) -> String {
        let stderr = sanitize_command_output_text(String::from_utf8_lossy(&output.stderr).as_ref());
        let stderr = stderr.trim().to_owned();
        if !stderr.is_empty() {
            return stderr;
        }

        let stdout = sanitize_command_output_text(String::from_utf8_lossy(&output.stdout).as_ref());
        let stdout = stdout.trim().to_owned();
        if !stdout.is_empty() {
            return stdout;
        }

        format!("exit status {}", output.status)
    }

    fn extract_pull_request_url(output: &std::process::Output) -> Option<String> {
        for stream in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(stream);
            for token in text.split_whitespace() {
                let cleaned = token
                    .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '(' || ch == ')')
                    .trim_end_matches(|ch: char| ch == ',' || ch == ';' || ch == '.');
                if (cleaned.starts_with("https://") || cleaned.starts_with("http://"))
                    && cleaned.contains("/pull/")
                {
                    return Some(cleaned.to_owned());
                }
            }
        }

        None
    }

    fn parse_pull_request_number(url: &str) -> Result<u64, CoreError> {
        let pull_segment = url.split("/pull/").nth(1).ok_or_else(|| {
            CoreError::DependencyUnavailable(format!(
                "GitHub pull request URL did not include `/pull/`: {url}"
            ))
        })?;
        let number = pull_segment
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if number.is_empty() {
            return Err(CoreError::DependencyUnavailable(format!(
                "GitHub pull request URL did not include a numeric pull request number: {url}"
            )));
        }

        number.parse::<u64>().map_err(|error| {
            CoreError::DependencyUnavailable(format!(
                "Failed to parse pull request number from URL `{url}`: {error}"
            ))
        })
    }

    fn ensure_repo_root_available(root: &Path, operation: &str) -> Result<(), CoreError> {
        if root.as_os_str().is_empty() {
            return Err(CoreError::Configuration(format!(
                "Cannot {operation}: repository root is empty."
            )));
        }
        if !root.exists() {
            return Err(CoreError::Configuration(format!(
                "Cannot {operation}: repository root '{}' does not exist.",
                root.display()
            )));
        }
        if !root.is_dir() {
            return Err(CoreError::Configuration(format!(
                "Cannot {operation}: repository root '{}' is not a directory.",
                root.display()
            )));
        }

        Ok(())
    }

    fn ensure_non_empty(value: &str, field: &str) -> Result<(), CoreError> {
        if value.trim().is_empty() {
            return Err(CoreError::Configuration(format!(
                "{field} must be a non-empty string."
            )));
        }

        Ok(())
    }

    fn extract_repository_name(item: &GhReviewQueuePullRequest, url: &str) -> Option<String> {
        if let Some(value) = item
            .repository
            .as_ref()
            .and_then(|repository| repository.name_with_owner.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_owned());
        }

        if let Some(value) = item
            .repository
            .as_ref()
            .and_then(|repository| repository.owner.as_ref())
            .and_then(|owner| owner.login.as_deref())
            .zip(
                item.repository
                    .as_ref()
                    .and_then(|repository| repository.name.as_deref()),
            )
            .map(|(owner, name)| format!("{owner}/{name}"))
        {
            return Some(value);
        }

        Self::extract_repository_name_from_pr_url(url)
    }

    fn extract_repository_name_from_pr_url(url: &str) -> Option<String> {
        let path = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split_once('/'))
            .map(|(_, path)| path)?;
        let mut segments = path.split('/');
        let owner = segments.next()?.trim();
        let repository = segments.next()?.trim();
        if owner.is_empty() || repository.is_empty() {
            return None;
        }

        Some(format!("{owner}/{repository}"))
    }

    fn truncate_for_error(body: &str) -> String {
        const MAX_LEN: usize = 200;
        if body.chars().count() <= MAX_LEN {
            body.to_owned()
        } else {
            format!("{}...", body.chars().take(MAX_LEN).collect::<String>())
        }
    }

    fn is_already_ready_error(detail: &str) -> bool {
        detail.contains("already") && detail.contains("ready")
    }

    fn is_existing_review_request_error(detail: &str) -> bool {
        detail.contains("already")
            && detail.contains("review")
            && (detail.contains("request") || detail.contains("requested"))
    }

    fn is_already_merged_error(detail: &str) -> bool {
        detail.contains("already") && detail.contains("merged")
    }

    fn requires_explicit_merge_strategy(detail: &str) -> bool {
        let mentions_strategy_flags = detail.contains("--merge")
            || detail.contains("--rebase")
            || detail.contains("--squash");
        mentions_strategy_flags && detail.contains("not running interactively")
    }

    fn is_merge_strategy_disabled(detail: &str, strategy: GhMergeStrategy) -> bool {
        if !(detail.contains("not allowed")
            || detail.contains("disabled")
            || detail.contains("not enabled"))
        {
            return false;
        }

        match strategy {
            GhMergeStrategy::Merge => detail.contains("merge"),
            GhMergeStrategy::Squash => detail.contains("squash"),
            GhMergeStrategy::Rebase => detail.contains("rebase"),
        }
    }

    fn extract_ci_workflow_name(value: &Value) -> Option<String> {
        match value {
            Value::String(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                }
            }
            Value::Object(map) => map
                .get("name")
                .and_then(|entry| entry.as_str())
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned),
            _ => None,
        }
    }
}

impl GitHubGhCliRepoProvider<ProcessCommandRunner> {
    pub fn from_config(config: GitHubGhCliRepoProviderConfig) -> Result<Self, CoreError> {
        Self::new(
            ProcessCommandRunner,
            config.binary,
            config.allow_unsafe_command_paths,
        )
    }
}

fn is_bare_command_name(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn sanitize_command_output_text(input: &str) -> String {
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

fn validate_command_binary_path(
    binary: &Path,
    env_name: &str,
    allow_unsafe_command_paths: bool,
) -> Result<(), CoreError> {
    if allow_unsafe_command_paths || is_bare_command_name(binary) {
        return Ok(());
    }

    Err(CoreError::Configuration(format!(
        "{env_name} resolves to '{}' which is treated as an unsafe command path by default. Use a bare command name or set {ENV_ALLOW_UNSAFE_COMMAND_PATHS}=true to allow explicit paths.",
        binary.display()
    )))
}

#[async_trait::async_trait]
impl<R: CommandRunner> CoreGithubClient for GitHubGhCliRepoProvider<R> {
    async fn health_check(&self) -> Result<(), CoreError> {
        self.run_gh(&Self::health_check_args(), None).map(|_| ())
    }
}

#[async_trait::async_trait]
impl<R: CommandRunner> CoreCodeHostProvider for GitHubGhCliRepoProvider<R> {
    fn kind(&self) -> CodeHostKind {
        CodeHostKind::Github
    }

    async fn health_check(&self) -> Result<(), CoreError> {
        CoreGithubClient::health_check(self).await
    }

    async fn create_draft_pull_request(
        &self,
        request: CreatePullRequestRequest,
    ) -> Result<PullRequestSummary, CoreError> {
        Self::ensure_repo_root_available(&request.repository.root, "create draft pull request")?;
        Self::ensure_non_empty(&request.title, "Pull request title")?;
        Self::ensure_non_empty(&request.base_branch, "Pull request base branch")?;
        Self::ensure_non_empty(&request.head_branch, "Pull request head branch")?;

        let args = Self::create_draft_pr_args(&request);
        let output = self.run_gh(&args, Some(request.repository.root.as_path()))?;
        let url = Self::extract_pull_request_url(&output).ok_or_else(|| {
            CoreError::DependencyUnavailable(
                "GitHub CLI did not return a pull request URL after `gh pr create`.".to_owned(),
            )
        })?;
        let number = Self::parse_pull_request_number(&url)?;

        Ok(PullRequestSummary {
            reference: PullRequestRef {
                repository: request.repository,
                number,
                url,
            },
            title: request.title,
            is_draft: true,
        })
    }

    async fn mark_ready_for_review(&self, pr: &PullRequestRef) -> Result<(), CoreError> {
        if pr.number == 0 {
            return Err(CoreError::Configuration(
                "Pull request number must be greater than zero.".to_owned(),
            ));
        }
        Self::ensure_repo_root_available(
            &pr.repository.root,
            "mark pull request ready for review",
        )?;

        let args = Self::mark_ready_args(pr);
        let output = self.run_gh_raw(&args, Some(pr.repository.root.as_path()))?;
        if output.status.success() {
            return Ok(());
        }

        let detail = Self::command_output_detail(&output).to_ascii_lowercase();
        if Self::is_already_ready_error(&detail) {
            return Ok(());
        }

        Err(self.command_failed(&args, &output))
    }

    async fn request_reviewers(
        &self,
        pr: &PullRequestRef,
        reviewers: ReviewerRequest,
    ) -> Result<(), CoreError> {
        if pr.number == 0 {
            return Err(CoreError::Configuration(
                "Pull request number must be greater than zero.".to_owned(),
            ));
        }
        Self::ensure_repo_root_available(&pr.repository.root, "request pull request reviewers")?;

        let args = Self::request_reviewers_args(pr, &reviewers);
        if args.len() == 3 {
            return Ok(());
        }

        let output = self.run_gh_raw(&args, Some(pr.repository.root.as_path()))?;
        if output.status.success() {
            return Ok(());
        }

        let detail = Self::command_output_detail(&output).to_ascii_lowercase();
        if Self::is_existing_review_request_error(&detail) {
            return Ok(());
        }

        Err(self.command_failed(&args, &output))
    }

    async fn list_waiting_for_my_review(&self) -> Result<Vec<PullRequestSummary>, CoreError> {
        let args = Self::review_queue_args();
        let output = self.run_gh(&args, None)?;
        let prs: Vec<GhReviewQueuePullRequest> =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Failed to parse `gh search prs` JSON output: {error}. Output: {}",
                    Self::truncate_for_error(&String::from_utf8_lossy(&output.stdout))
                ))
            })?;

        let mut summaries = Vec::with_capacity(prs.len());
        for pr in prs {
            let repository_name = Self::extract_repository_name(&pr, &pr.url)
                .unwrap_or_else(|| "unknown/unknown".to_owned());
            summaries.push(PullRequestSummary {
                reference: PullRequestRef {
                    repository: RepositoryRef {
                        id: repository_name.clone(),
                        name: repository_name,
                        root: PathBuf::new(),
                    },
                    number: pr.number,
                    url: pr.url,
                },
                title: pr.title,
                is_draft: pr.is_draft,
            });
        }

        Ok(summaries)
    }

    async fn get_pull_request_merge_state(
        &self,
        pr: &PullRequestRef,
    ) -> Result<PullRequestMergeState, CoreError> {
        if pr.number == 0 {
            return Err(CoreError::Configuration(
                "Pull request number must be greater than zero.".to_owned(),
            ));
        }
        Self::ensure_repo_root_available(&pr.repository.root, "read pull request merge state")?;

        let args = Self::pull_request_merge_state_args(pr);
        let output = self.run_gh(&args, Some(pr.repository.root.as_path()))?;
        let parsed: GhPullRequestMergeState =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Failed to parse `gh pr view` merge-state JSON output: {error}. Output: {}",
                    Self::truncate_for_error(&String::from_utf8_lossy(&output.stdout))
                ))
            })?;

        Ok(PullRequestMergeState {
            merged: parsed.merged_at.is_some(),
            is_draft: parsed.is_draft,
            state: parsed
                .state
                .and_then(|value| sanitize_optional_non_empty(value.as_str())),
            review_decision: parsed
                .review_decision
                .and_then(|value| sanitize_optional_non_empty(value.as_str())),
            review_summary: summarize_reviews(parsed.latest_reviews),
            merge_conflict: parsed
                .merge_state_status
                .as_deref()
                .map(|status| {
                    let normalized = status.trim().to_ascii_uppercase();
                    normalized == "DIRTY" || normalized == "CONFLICTING"
                })
                .unwrap_or(false),
            base_branch: parsed.base_ref_name.and_then(|branch| {
                let trimmed = branch.trim().to_owned();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }),
            head_branch: parsed.head_ref_name.and_then(|branch| {
                let trimmed = branch.trim().to_owned();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }),
        })
    }

    async fn list_pull_request_ci_statuses(
        &self,
        pr: &PullRequestRef,
    ) -> Result<Vec<PullRequestCiStatus>, CoreError> {
        if pr.number == 0 {
            return Err(CoreError::Configuration(
                "Pull request number must be greater than zero.".to_owned(),
            ));
        }
        Self::ensure_repo_root_available(&pr.repository.root, "read pull request CI checks")?;

        let args = Self::pull_request_ci_statuses_args(pr);
        let output = self.run_gh_raw(&args, Some(pr.repository.root.as_path()))?;
        if !output.status.success() && output.status.code() != Some(8) {
            return Err(self.command_failed(&args, &output));
        }

        let body = String::from_utf8_lossy(&output.stdout);
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }

        let parsed: Vec<GhPullRequestCheck> =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Failed to parse `gh pr checks` JSON output: {error}. Output: {}",
                    Self::truncate_for_error(body.as_ref())
                ))
            })?;

        let mut checks = Vec::with_capacity(parsed.len());
        for entry in parsed {
            let workflow = entry
                .workflow
                .as_ref()
                .and_then(Self::extract_ci_workflow_name);
            let name = entry
                .name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| workflow.clone())
                .unwrap_or_else(|| "unnamed-check".to_owned());
            let bucket = entry
                .bucket
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_owned());
            let state = entry
                .state
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| bucket.clone());
            let link = entry
                .link
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);

            checks.push(PullRequestCiStatus {
                name,
                workflow,
                bucket,
                state,
                link,
            });
        }

        Ok(checks)
    }

    async fn merge_pull_request(&self, pr: &PullRequestRef) -> Result<(), CoreError> {
        if pr.number == 0 {
            return Err(CoreError::Configuration(
                "Pull request number must be greater than zero.".to_owned(),
            ));
        }
        Self::ensure_repo_root_available(&pr.repository.root, "merge pull request")?;

        let args = Self::merge_pull_request_args(pr, None);
        let output = self.run_gh_raw(&args, Some(pr.repository.root.as_path()))?;
        if output.status.success() {
            return Ok(());
        }

        let detail = Self::command_output_detail(&output).to_ascii_lowercase();
        if Self::is_already_merged_error(&detail) {
            return Ok(());
        }
        if !Self::requires_explicit_merge_strategy(&detail) {
            return Err(self.command_failed(&args, &output));
        }

        let mut first_non_strategy_error: Option<CoreError> = None;
        let mut last_strategy_disabled_error: Option<CoreError> = None;
        for strategy in [
            GhMergeStrategy::Merge,
            GhMergeStrategy::Squash,
            GhMergeStrategy::Rebase,
        ] {
            let strategy_args = Self::merge_pull_request_args(pr, Some(strategy));
            let strategy_output =
                self.run_gh_raw(&strategy_args, Some(pr.repository.root.as_path()))?;
            if strategy_output.status.success() {
                return Ok(());
            }

            let strategy_detail =
                Self::command_output_detail(&strategy_output).to_ascii_lowercase();
            if Self::is_already_merged_error(&strategy_detail) {
                return Ok(());
            }

            let error = self.command_failed(&strategy_args, &strategy_output);
            if Self::is_merge_strategy_disabled(&strategy_detail, strategy) {
                last_strategy_disabled_error = Some(error);
                continue;
            }

            if first_non_strategy_error.is_none() {
                first_non_strategy_error = Some(error);
            }
        }

        Err(first_non_strategy_error
            .or(last_strategy_disabled_error)
            .unwrap_or_else(|| self.command_failed(&args, &output)))
    }

    async fn find_open_pull_request_for_branch(
        &self,
        repository: &RepositoryRef,
        head_branch: &str,
    ) -> Result<Option<PullRequestRef>, CoreError> {
        Self::ensure_repo_root_available(
            &repository.root,
            "resolve pull request for branch fallback",
        )?;
        let head_branch = head_branch.trim();
        Self::ensure_non_empty(head_branch, "Pull request head branch")?;

        let args = Self::pull_request_for_branch_args(head_branch);
        let output = self.run_gh(&args, Some(repository.root.as_path()))?;
        let prs: Vec<GhBranchPullRequest> =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Failed to parse `gh pr list` JSON output: {error}. Output: {}",
                    Self::truncate_for_error(&String::from_utf8_lossy(&output.stdout))
                ))
            })?;

        let Some(pr) = prs.into_iter().next() else {
            return Ok(None);
        };

        let repository_name = Self::extract_repository_name_from_pr_url(pr.url.as_str())
            .or_else(|| {
                let candidate = repository.name.trim();
                if candidate.is_empty() {
                    None
                } else {
                    Some(candidate.to_owned())
                }
            })
            .or_else(|| {
                let candidate = repository.id.trim();
                if candidate.is_empty() {
                    None
                } else {
                    Some(candidate.to_owned())
                }
            })
            .unwrap_or_else(|| "unknown/unknown".to_owned());

        Ok(Some(PullRequestRef {
            repository: RepositoryRef {
                id: repository_name.clone(),
                name: repository_name,
                root: repository.root.clone(),
            },
            number: pr.number,
            url: pr.url,
        }))
    }
}

impl<R: CommandRunner> VcsRepoProvider for GitHubGhCliRepoProvider<R> {
    fn kind(&self) -> VcsRepoProviderKind {
        VcsRepoProviderKind::GitHubGhCli
    }
}

fn sanitize_optional_non_empty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn summarize_reviews(
    reviews: Option<Vec<GhPullRequestReview>>,
) -> Option<PullRequestReviewSummary> {
    let reviews = reviews?;
    if reviews.is_empty() {
        return None;
    }

    let mut summary = PullRequestReviewSummary::default();
    for review in reviews {
        let Some(state) = review.state else {
            continue;
        };
        let normalized = state.trim().to_ascii_uppercase();
        if normalized.is_empty() {
            continue;
        }
        summary.total = summary.total.saturating_add(1);
        match normalized.as_str() {
            "APPROVED" => summary.approved = summary.approved.saturating_add(1),
            "CHANGES_REQUESTED" => {
                summary.changes_requested = summary.changes_requested.saturating_add(1)
            }
            "COMMENTED" => summary.commented = summary.commented.saturating_add(1),
            "PENDING" => summary.pending = summary.pending.saturating_add(1),
            "DISMISSED" => summary.dismissed = summary.dismissed.saturating_add(1),
            _ => {}
        }
    }

    if summary.total == 0 {
        None
    } else {
        Some(summary)
    }
}

#[derive(Debug, Deserialize)]
struct GhReviewQueuePullRequest {
    number: u64,
    title: String,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(default)]
    repository: Option<GhReviewQueueRepository>,
}

#[derive(Debug, Default, Deserialize)]
struct GhReviewQueueRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owner: Option<GhReviewQueueOwner>,
}

#[derive(Debug, Deserialize)]
struct GhReviewQueueOwner {
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhPullRequestMergeState {
    #[serde(rename = "mergedAt", default)]
    merged_at: Option<String>,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "latestReviews", default)]
    latest_reviews: Option<Vec<GhPullRequestReview>>,
    #[serde(rename = "baseRefName", default)]
    base_ref_name: Option<String>,
    #[serde(rename = "headRefName", default)]
    head_ref_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhPullRequestReview {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhPullRequestCheck {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bucket: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    workflow: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct GhBranchPullRequest {
    number: u64,
    url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{
        CoreCodeHostProvider as CodeHostProvider, CoreGithubClient as GithubClient,
        CreatePullRequestRequest, UrlOpener, VcsRepoProvider, VcsRepoProviderKind,
    };
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct StubRunner {
        calls: Mutex<Vec<(String, Vec<OsString>, Option<PathBuf>)>>,
        results: Mutex<VecDeque<io::Result<std::process::Output>>>,
    }

    impl StubRunner {
        fn with_results(results: Vec<io::Result<std::process::Output>>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                results: Mutex::new(VecDeque::from(results)),
            }
        }
    }

    impl CommandRunner for StubRunner {
        fn run(
            &self,
            program: &str,
            args: &[OsString],
            cwd: Option<&Path>,
        ) -> io::Result<std::process::Output> {
            self.calls.lock().expect("lock").push((
                program.to_owned(),
                args.to_vec(),
                cwd.map(Path::to_path_buf),
            ));

            self.results
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "missing stubbed command output",
                    ))
                })
                .map(|output| std::process::Output {
                    status: output.status,
                    stdout: output.stdout,
                    stderr: output.stderr,
                })
        }
    }

    fn output(status_code: i32, stdout: &str, stderr: &str) -> std::process::Output {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(status_code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(status_code as u32),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }
    }

    fn success_output() -> std::process::Output {
        output(0, "", "")
    }

    fn sample_repository() -> RepositoryRef {
        RepositoryRef {
            id: "repo-1".to_owned(),
            name: "repo".to_owned(),
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    #[test]
    fn provider_exposes_vcs_repo_provider_kind_and_key() {
        let provider =
            GitHubGhCliRepoProvider::from_config(GitHubGhCliRepoProviderConfig::default())
                .expect("init");
        assert_eq!(VcsRepoProvider::kind(&provider), VcsRepoProviderKind::GitHubGhCli);
        assert_eq!(provider.provider_key(), "vcs_repos.github_gh_cli");
    }

    #[test]
    fn system_url_opener_selects_supported_commands() {
        let mac = SystemUrlOpener::<ProcessCommandRunner>::command_for_os("macos")
            .expect("macos command");
        assert_eq!(mac.program, PathBuf::from("open"));
        assert!(mac.prefix_args.is_empty());

        let linux = SystemUrlOpener::<ProcessCommandRunner>::command_for_os("linux")
            .expect("linux command");
        assert_eq!(linux.program, PathBuf::from("xdg-open"));
        assert!(linux.prefix_args.is_empty());

        let windows = SystemUrlOpener::<ProcessCommandRunner>::command_for_os("windows")
            .expect("windows command");
        assert_eq!(
            windows.prefix_args,
            vec![
                OsString::from("/C"),
                OsString::from("start"),
                OsString::from(""),
            ]
        );
        assert_eq!(windows.program, PathBuf::from("cmd"));
    }

    #[test]
    fn system_url_opener_rejects_unsupported_os() {
        let err = SystemUrlOpener::<ProcessCommandRunner>::command_for_os("freebsd")
            .expect_err("freebsd should be unsupported");
        assert!(err.to_string().contains("unsupported"));
    }

    #[tokio::test]
    async fn system_url_opener_uses_platform_command() {
        let expected_program = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "linux") {
            "xdg-open"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            return;
        };
        let expected_args = if cfg!(target_os = "windows") {
            vec![
                OsString::from("/C"),
                OsString::from("start"),
                OsString::from(""),
                OsString::from("https://github.com/octocat/orchestrator/pull/42"),
            ]
        } else {
            vec![OsString::from(
                "https://github.com/octocat/orchestrator/pull/42",
            )]
        };
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let opener = SystemUrlOpener::new(runner).expect("init");

        UrlOpener::open_url(&opener, "https://github.com/octocat/orchestrator/pull/42")
            .await
            .expect("open url");

        let calls = opener.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, expected_program);
        assert_eq!(calls[0].1, expected_args);
        assert!(calls[0].2.is_none());
    }

    #[tokio::test]
    async fn system_url_opener_rejects_empty_url() {
        let runner = StubRunner::with_results(Vec::new());
        let opener = SystemUrlOpener::new(runner).expect("init");

        let err = UrlOpener::open_url(&opener, "   ")
            .await
            .expect_err("empty URL should fail");
        assert!(err.to_string().contains("non-empty"));

        let calls = opener.runner.calls.lock().expect("lock");
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn system_url_opener_surfaces_missing_binary() {
        let runner = StubRunner::with_results(vec![Err(io::Error::new(
            io::ErrorKind::NotFound,
            "missing",
        ))]);
        let opener = SystemUrlOpener::new(runner).expect("init");

        let err = UrlOpener::open_url(&opener, "https://example.com")
            .await
            .expect_err("missing binary should fail");
        assert!(err.to_string().contains("was not found"));
    }

    #[tokio::test]
    async fn system_url_opener_surfaces_command_failure() {
        let runner = StubRunner::with_results(vec![Ok(output(1, "", "Unable to open URL"))]);
        let opener = SystemUrlOpener::new(runner).expect("init");

        let err = UrlOpener::open_url(&opener, "https://example.com")
            .await
            .expect_err("open failure should fail");
        assert!(err.to_string().contains("Unable to open URL"));
    }

    #[test]
    fn command_output_detail_strips_control_characters() {
        let detail = GitHubGhCliRepoProvider::<ProcessCommandRunner>::command_output_detail(&output(
            1,
            "",
            "GitHub CLI command fa\0i\u{2400}led\r\n",
        ));
        assert_eq!(detail, "GitHub CLI command failed");
    }

    #[tokio::test]
    async fn command_construction_uses_argument_vector() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        GithubClient::health_check(&client)
            .await
            .expect("health check");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            GitHubGhCliRepoProvider::<ProcessCommandRunner>::health_check_args()
        );
        assert!(calls[0].2.is_none());
    }

    #[tokio::test]
    async fn missing_gh_binary_is_actionable() {
        let runner = StubRunner::with_results(vec![Err(io::Error::new(
            io::ErrorKind::NotFound,
            "missing",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let err = GithubClient::health_check(&client)
            .await
            .expect_err("expected error");
        assert!(err.to_string().contains("Install gh"));
    }

    #[tokio::test]
    async fn create_draft_pull_request_uses_repo_root_and_parses_url() {
        let runner = StubRunner::with_results(vec![Ok(output(
            0,
            "https://github.com/octocat/orchestrator/pull/42\n",
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let repository = sample_repository();

        let summary = CodeHostProvider::create_draft_pull_request(
            &client,
            CreatePullRequestRequest {
                repository: repository.clone(),
                title: "Expand gh adapter".to_owned(),
                body: "Implements full PR lifecycle operations.".to_owned(),
                base_branch: "main".to_owned(),
                head_branch: "ap/AP-124-gh-pr-lifecycle".to_owned(),
                ticket: None,
            },
        )
        .await
        .expect("create draft pull request");

        assert_eq!(summary.reference.number, 42);
        assert_eq!(
            summary.reference.url,
            "https://github.com/octocat/orchestrator/pull/42"
        );
        assert_eq!(summary.reference.repository, repository);
        assert!(summary.is_draft);

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("create"),
                OsString::from("--draft"),
                OsString::from("--title"),
                OsString::from("Expand gh adapter"),
                OsString::from("--body"),
                OsString::from("Implements full PR lifecycle operations."),
                OsString::from("--base"),
                OsString::from("main"),
                OsString::from("--head"),
                OsString::from("ap/AP-124-gh-pr-lifecycle"),
            ]
        );
        assert_eq!(calls[0].2, Some(repository.root.clone()));
    }

    #[tokio::test]
    async fn create_draft_pull_request_rejects_missing_repository_root() {
        let runner = StubRunner::with_results(Vec::new());
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let missing_root = std::env::temp_dir().join(format!(
            "orchestrator-gh-missing-root-{}-{timestamp}",
            std::process::id(),
        ));
        let request = CreatePullRequestRequest {
            repository: RepositoryRef {
                id: "repo-1".to_owned(),
                name: "repo".to_owned(),
                root: missing_root,
            },
            title: "Expand gh adapter".to_owned(),
            body: "Implements full PR lifecycle operations.".to_owned(),
            base_branch: "main".to_owned(),
            head_branch: "ap/AP-124-gh-pr-lifecycle".to_owned(),
            ticket: None,
        };

        let error = CodeHostProvider::create_draft_pull_request(&client, request)
            .await
            .expect_err("missing repository root should fail");
        assert!(error.to_string().contains("does not exist"));

        let calls = client.runner.calls.lock().expect("lock");
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn mark_ready_for_review_uses_pr_number() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 7,
            url: "https://github.com/octocat/orchestrator/pull/7".to_owned(),
        };

        CodeHostProvider::mark_ready_for_review(&client, &pr)
            .await
            .expect("mark ready");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("ready"),
                OsString::from("7"),
            ]
        );
        assert_eq!(calls[0].2, Some(pr.repository.root.clone()));
    }

    #[tokio::test]
    async fn mark_ready_for_review_is_idempotent_when_already_ready() {
        let runner = StubRunner::with_results(vec![Ok(output(
            1,
            "",
            "pull request is already marked ready for review",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 9,
            url: "https://github.com/octocat/orchestrator/pull/9".to_owned(),
        };

        CodeHostProvider::mark_ready_for_review(&client, &pr)
            .await
            .expect("already-ready response should be accepted");
    }

    #[tokio::test]
    async fn merge_pull_request_retries_with_explicit_strategy_when_required() {
        let runner = StubRunner::with_results(vec![
            Ok(output(
                1,
                "",
                "--merge, --rebase, or --squash required when not running interactively",
            )),
            Ok(success_output()),
        ]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 7,
            url: "https://github.com/octocat/orchestrator/pull/7".to_owned(),
        };

        CodeHostProvider::merge_pull_request(&client, &pr)
            .await
            .expect("merge should succeed via strategy retry");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from("7"),
                OsString::from("--delete-branch"),
            ]
        );
        assert_eq!(
            calls[1].1,
            vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from("7"),
                OsString::from("--merge"),
                OsString::from("--delete-branch"),
            ]
        );
    }

    #[tokio::test]
    async fn merge_pull_request_falls_back_when_primary_strategy_is_disabled() {
        let runner = StubRunner::with_results(vec![
            Ok(output(
                1,
                "",
                "--merge, --rebase, or --squash required when not running interactively",
            )),
            Ok(output(
                1,
                "",
                "merge commits are not allowed on this repository",
            )),
            Ok(success_output()),
        ]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 11,
            url: "https://github.com/octocat/orchestrator/pull/11".to_owned(),
        };

        CodeHostProvider::merge_pull_request(&client, &pr)
            .await
            .expect("merge should fall back to a supported strategy");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[1].1,
            vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from("11"),
                OsString::from("--merge"),
                OsString::from("--delete-branch"),
            ]
        );
        assert_eq!(
            calls[2].1,
            vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from("11"),
                OsString::from("--squash"),
                OsString::from("--delete-branch"),
            ]
        );
    }

    #[tokio::test]
    async fn merge_pull_request_continues_retrying_strategies_after_non_policy_failure() {
        let runner = StubRunner::with_results(vec![
            Ok(output(
                1,
                "",
                "--merge, --rebase, or --squash required when not running interactively",
            )),
            Ok(output(
                1,
                "",
                "merge commits are disabled for this repository",
            )),
            Ok(output(
                1,
                "",
                "merge blocked: required status checks have not passed",
            )),
            Ok(success_output()),
        ]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 13,
            url: "https://github.com/octocat/orchestrator/pull/13".to_owned(),
        };

        CodeHostProvider::merge_pull_request(&client, &pr)
            .await
            .expect("merge should continue retrying strategies and eventually succeed");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[2].1,
            vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from("13"),
                OsString::from("--squash"),
                OsString::from("--delete-branch"),
            ]
        );
        assert_eq!(
            calls[3].1,
            vec![
                OsString::from("pr"),
                OsString::from("merge"),
                OsString::from("13"),
                OsString::from("--rebase"),
                OsString::from("--delete-branch"),
            ]
        );
    }

    #[tokio::test]
    async fn request_reviewers_short_circuits_when_empty() {
        let runner = StubRunner::with_results(Vec::new());
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 12,
            url: "https://github.com/octocat/orchestrator/pull/12".to_owned(),
        };

        CodeHostProvider::request_reviewers(&client, &pr, ReviewerRequest::default())
            .await
            .expect("empty reviewers no-op");

        let calls = client.runner.calls.lock().expect("lock");
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn request_reviewers_includes_users_and_teams() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 12,
            url: "https://github.com/octocat/orchestrator/pull/12".to_owned(),
        };

        CodeHostProvider::request_reviewers(
            &client,
            &pr,
            ReviewerRequest {
                users: vec!["alice".to_owned()],
                teams: vec!["octo/reviewers".to_owned()],
            },
        )
        .await
        .expect("request reviewers");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("edit"),
                OsString::from("12"),
                OsString::from("--add-reviewer"),
                OsString::from("alice"),
                OsString::from("--add-reviewer"),
                OsString::from("octo/reviewers"),
            ]
        );
    }

    #[tokio::test]
    async fn request_reviewers_deduplicates_and_trims() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 12,
            url: "https://github.com/octocat/orchestrator/pull/12".to_owned(),
        };

        CodeHostProvider::request_reviewers(
            &client,
            &pr,
            ReviewerRequest {
                users: vec!["alice".to_owned(), " Alice ".to_owned()],
                teams: vec!["octo/reviewers".to_owned(), " ".to_owned()],
            },
        )
        .await
        .expect("request reviewers");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("edit"),
                OsString::from("12"),
                OsString::from("--add-reviewer"),
                OsString::from("alice"),
                OsString::from("--add-reviewer"),
                OsString::from("octo/reviewers"),
            ]
        );
    }

    #[tokio::test]
    async fn request_reviewers_is_idempotent_when_already_requested() {
        let runner = StubRunner::with_results(vec![Ok(output(
            1,
            "",
            "review request already exists for alice",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 12,
            url: "https://github.com/octocat/orchestrator/pull/12".to_owned(),
        };

        CodeHostProvider::request_reviewers(
            &client,
            &pr,
            ReviewerRequest {
                users: vec!["alice".to_owned()],
                teams: vec![],
            },
        )
        .await
        .expect("existing review request should be accepted");
    }

    #[tokio::test]
    async fn review_queue_parses_pr_summaries() {
        let runner = StubRunner::with_results(vec![Ok(output(
            0,
            r#"[{"number":3,"title":"Fix race condition","url":"https://github.com/octo/repo/pull/3","isDraft":false,"repository":{"nameWithOwner":"octo/repo"}}]"#,
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");

        let queue = CodeHostProvider::list_waiting_for_my_review(&client)
            .await
            .expect("list review queue");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].reference.number, 3);
        assert_eq!(queue[0].title, "Fix race condition");
        assert_eq!(queue[0].reference.repository.id, "octo/repo");
        assert_eq!(queue[0].reference.repository.root, PathBuf::new());
    }

    #[tokio::test]
    async fn review_queue_falls_back_to_url_when_repository_payload_missing() {
        let runner = StubRunner::with_results(vec![Ok(output(
            0,
            r#"[{"number":5,"title":"Docs update","url":"https://github.com/octo/docs/pull/5","isDraft":true}]"#,
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");

        let queue = CodeHostProvider::list_waiting_for_my_review(&client)
            .await
            .expect("list review queue");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].reference.repository.id, "octo/docs");
        assert!(queue[0].is_draft);
    }

    #[tokio::test]
    async fn pull_request_merge_state_parses_pr_state_and_reviews() {
        let runner = StubRunner::with_results(vec![Ok(output(
            0,
            r#"{"mergedAt":null,"isDraft":false,"state":"OPEN","reviewDecision":"CHANGES_REQUESTED","latestReviews":[{"state":"APPROVED"},{"state":"CHANGES_REQUESTED"},{"state":"COMMENTED"}],"mergeStateStatus":"CLEAN","baseRefName":"main","headRefName":"ap/AP-280"}"#,
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 88,
            url: "https://github.com/octocat/orchestrator/pull/88".to_owned(),
        };

        let state = CodeHostProvider::get_pull_request_merge_state(&client, &pr)
            .await
            .expect("read merge state");
        assert!(!state.merged);
        assert!(!state.is_draft);
        assert_eq!(state.state.as_deref(), Some("OPEN"));
        assert_eq!(state.review_decision.as_deref(), Some("CHANGES_REQUESTED"));
        let review_summary = state.review_summary.expect("review summary");
        assert_eq!(review_summary.total, 3);
        assert_eq!(review_summary.approved, 1);
        assert_eq!(review_summary.changes_requested, 1);
        assert_eq!(review_summary.commented, 1);
        assert!(!state.merge_conflict);
        assert_eq!(state.base_branch.as_deref(), Some("main"));
        assert_eq!(state.head_branch.as_deref(), Some("ap/AP-280"));

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("view"),
                OsString::from("88"),
                OsString::from("--json"),
                OsString::from(
                    "mergedAt,isDraft,state,reviewDecision,latestReviews,mergeStateStatus,baseRefName,headRefName",
                ),
            ]
        );
    }

    #[tokio::test]
    async fn pull_request_ci_statuses_parse_required_check_rows() {
        let runner = StubRunner::with_results(vec![Ok(output(
            0,
            r#"[{"name":"build","bucket":"pass","state":"SUCCESS","link":"https://github.com/octo/repo/actions/runs/1","workflow":"Build"},{"name":"tests","bucket":"fail","state":"FAILURE","link":"https://github.com/octo/repo/actions/runs/2","workflow":{"name":"Tests"}}]"#,
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 99,
            url: "https://github.com/octocat/orchestrator/pull/99".to_owned(),
        };

        let checks = CodeHostProvider::list_pull_request_ci_statuses(&client, &pr)
            .await
            .expect("read CI checks");
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "build");
        assert_eq!(checks[0].bucket, "pass");
        assert_eq!(checks[0].workflow.as_deref(), Some("Build"));
        assert_eq!(checks[1].name, "tests");
        assert_eq!(checks[1].bucket, "fail");
        assert_eq!(checks[1].workflow.as_deref(), Some("Tests"));

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("checks"),
                OsString::from("99"),
                OsString::from("--required"),
                OsString::from("--json"),
                OsString::from("name,bucket,state,link,workflow"),
            ]
        );
    }

    #[tokio::test]
    async fn pull_request_ci_statuses_accept_pending_exit_code() {
        let runner = StubRunner::with_results(vec![Ok(output(
            8,
            r#"[{"name":"tests","bucket":"pending","state":"IN_PROGRESS"}]"#,
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let pr = PullRequestRef {
            repository: sample_repository(),
            number: 101,
            url: "https://github.com/octocat/orchestrator/pull/101".to_owned(),
        };

        let checks = CodeHostProvider::list_pull_request_ci_statuses(&client, &pr)
            .await
            .expect("read pending CI checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "tests");
        assert_eq!(checks[0].bucket, "pending");
        assert_eq!(checks[0].state, "IN_PROGRESS");
    }

    #[tokio::test]
    async fn find_open_pull_request_for_branch_uses_head_filter_and_parses_result() {
        let runner = StubRunner::with_results(vec![Ok(output(
            0,
            r#"[{"number":52,"url":"https://github.com/octocat/orchestrator/pull/52"}]"#,
            "",
        ))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let repository = sample_repository();

        let pr = CodeHostProvider::find_open_pull_request_for_branch(
            &client,
            &repository,
            "feature/resolve-pr",
        )
        .await
        .expect("find branch PR")
        .expect("expected PR");

        assert_eq!(pr.number, 52);
        assert_eq!(pr.url, "https://github.com/octocat/orchestrator/pull/52");
        assert_eq!(pr.repository.id, "octocat/orchestrator");
        assert_eq!(pr.repository.root, repository.root);

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec![
                OsString::from("pr"),
                OsString::from("list"),
                OsString::from("--state"),
                OsString::from("open"),
                OsString::from("--head"),
                OsString::from("feature/resolve-pr"),
                OsString::from("--json"),
                OsString::from("number,url"),
                OsString::from("--limit"),
                OsString::from("1"),
            ]
        );
        assert_eq!(calls[0].2, Some(repository.root.clone()));
    }

    #[tokio::test]
    async fn find_open_pull_request_for_branch_returns_none_when_not_found() {
        let runner = StubRunner::with_results(vec![Ok(output(0, "[]", ""))]);
        let client = GitHubGhCliRepoProvider::new(runner, PathBuf::from("gh"), false).expect("init");
        let repository = sample_repository();

        let pr = CodeHostProvider::find_open_pull_request_for_branch(
            &client,
            &repository,
            "feature/no-pr",
        )
        .await
        .expect("find branch PR");

        assert!(pr.is_none());
    }

    #[test]
    fn command_path_guard_rejects_non_bare_binary_by_default() {
        let err = validate_command_binary_path(Path::new("./bin/gh"), ENV_GH_BIN, false)
            .expect_err("relative command paths should be blocked by default");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_ALLOW_UNSAFE_COMMAND_PATHS"));
    }

    #[test]
    fn command_path_guard_accepts_non_bare_binary_when_explicitly_enabled() {
        validate_command_binary_path(Path::new("./bin/gh"), ENV_GH_BIN, true)
            .expect("unsafe command path opt-in should pass");
    }
}
