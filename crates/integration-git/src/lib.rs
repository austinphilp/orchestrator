use std::collections::{BTreeSet, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use orchestrator_core::{
    CoreError, CreateWorktreeRequest, DeleteWorktreeRequest, RepositoryRef, VcsProvider,
    WorktreeStatus, WorktreeSummary,
};

const ENV_GIT_BIN: &str = "ORCHESTRATOR_GIT_BIN";
const ENV_ALLOW_DELETE_UNMERGED_BRANCHES: &str = "ORCHESTRATOR_GIT_ALLOW_DELETE_UNMERGED_BRANCHES";
const DEFAULT_BASE_BRANCH: &str = "main";
const WORKTREE_BRANCH_TEMPLATE: &str = "ap/{issue-key}-{slug}";
const WORKTREE_BRANCH_PREFIX: &str = "ap/";

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[OsString]) -> io::Result<std::process::Output>;
}

#[derive(Debug, Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &str, args: &[OsString]) -> io::Result<std::process::Output> {
        Command::new(program).args(args).output()
    }
}

pub struct GitCliVcsProvider<R: CommandRunner> {
    runner: R,
    binary: PathBuf,
    allow_force_delete_unmerged_branches: bool,
}

struct ParsedWorktreeBranch<'a> {
    issue_key: &'a str,
}

impl<R: CommandRunner> GitCliVcsProvider<R> {
    pub fn new(runner: R) -> Result<Self, CoreError> {
        let binary = std::env::var_os(ENV_GIT_BIN)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("git"));
        if binary.as_os_str().is_empty() {
            return Err(CoreError::Configuration(format!(
                "{ENV_GIT_BIN} is set but empty. Provide a valid git binary path or unset it."
            )));
        }

        let allow_force_delete_unmerged_branches =
            match std::env::var(ENV_ALLOW_DELETE_UNMERGED_BRANCHES) {
                Ok(value) => parse_bool_env(ENV_ALLOW_DELETE_UNMERGED_BRANCHES, &value)?,
                Err(std::env::VarError::NotPresent) => false,
                Err(_) => {
                    return Err(CoreError::Configuration(
                        "ORCHESTRATOR_GIT_ALLOW_DELETE_UNMERGED_BRANCHES contained invalid UTF-8"
                            .to_owned(),
                    ))
                }
            };

        Ok(Self::with_binary(
            runner,
            binary,
            allow_force_delete_unmerged_branches,
        ))
    }

    pub fn with_binary(
        runner: R,
        binary: PathBuf,
        allow_force_delete_unmerged_branches: bool,
    ) -> Self {
        Self {
            runner,
            binary,
            allow_force_delete_unmerged_branches,
        }
    }

    pub fn health_check_args() -> Vec<OsString> {
        vec![OsString::from("--version")]
    }

    fn run_git_raw(&self, args: &[OsString]) -> Result<std::process::Output, CoreError> {
        let program = self
            .binary
            .to_str()
            .ok_or_else(|| CoreError::Configuration("Invalid git binary path".to_owned()))?;
        self.runner
            .run(program, args)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CoreError::DependencyUnavailable(
                    format!(
                        "Git CLI `{}` was not found. Install Git or set {ENV_GIT_BIN} to a valid binary path.",
                        self.binary.display()
                    ),
                ),
                _ => CoreError::DependencyUnavailable(format!(
                    "Failed to execute Git CLI `{}`: {error}",
                    self.binary.display()
                )),
            })
    }

    fn run_git(&self, args: &[OsString]) -> Result<std::process::Output, CoreError> {
        let output = self.run_git_raw(args)?;
        if output.status.success() {
            return Ok(output);
        }

        Err(self.command_failed(args, &output))
    }

    fn command_failed(&self, args: &[OsString], output: &std::process::Output) -> CoreError {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit status {}", output.status)
        };
        let rendered_args = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ");

        CoreError::DependencyUnavailable(format!(
            "Git command failed (`{} {rendered_args}`): {detail}",
            self.binary.display()
        ))
    }

    fn discover_repositories_under_root(&self, root: &Path) -> Result<Vec<PathBuf>, CoreError> {
        if !root.exists() {
            return Err(CoreError::Configuration(format!(
                "Configured project root '{}' does not exist.",
                root.display()
            )));
        }
        if !root.is_dir() {
            return Err(CoreError::Configuration(format!(
                "Configured project root '{}' is not a directory.",
                root.display()
            )));
        }

        let canonical_root = fs::canonicalize(root).map_err(|error| {
            CoreError::Configuration(format!(
                "Failed to canonicalize project root '{}': {error}",
                root.display()
            ))
        })?;

        let mut stack = VecDeque::from([canonical_root.clone()]);
        let mut visited = HashSet::new();
        let mut repositories = BTreeSet::new();

        while let Some(current) = stack.pop_back() {
            if !visited.insert(current.clone()) {
                continue;
            }

            if self.is_git_repository_root(&current)? {
                repositories.insert(current);
                continue;
            }

            let entries = match fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(error)
                    if error.kind() == io::ErrorKind::PermissionDenied
                        && current != canonical_root =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(CoreError::Configuration(format!(
                        "Failed to read directory '{}' while discovering repositories: {error}",
                        current.display()
                    )))
                }
            };

            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => continue,
                    Err(error) => {
                        return Err(CoreError::Configuration(format!(
                            "Failed to inspect directory entry under '{}': {error}",
                            current.display()
                        )))
                    }
                };

                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => continue,
                    Err(error) => {
                        return Err(CoreError::Configuration(format!(
                            "Failed to inspect directory entry type '{}': {error}",
                            entry.path().display()
                        )))
                    }
                };

                if !file_type.is_dir() || file_type.is_symlink() {
                    continue;
                }

                if entry.file_name() == ".git" {
                    continue;
                }

                stack.push_back(entry.path());
            }
        }

        Ok(repositories.into_iter().collect())
    }

    fn is_git_repository_root(&self, path: &Path) -> Result<bool, CoreError> {
        let dot_git = path.join(".git");
        if !dot_git.exists() {
            return Ok(false);
        }

        let metadata = fs::metadata(&dot_git).map_err(|error| {
            CoreError::Configuration(format!(
                "Failed to inspect '{}': {error}",
                dot_git.display()
            ))
        })?;

        if metadata.is_dir() {
            return Ok(true);
        }

        if metadata.is_file() {
            let contents = fs::read(&dot_git).map_err(|error| {
                CoreError::Configuration(format!("Failed to read '{}': {error}", dot_git.display()))
            })?;
            let first_non_whitespace = contents
                .iter()
                .position(|byte| !byte.is_ascii_whitespace())
                .unwrap_or(contents.len());
            return Ok(contents[first_non_whitespace..].starts_with(b"gitdir:"));
        }

        Ok(false)
    }

    fn repository_ref_from_path(root: PathBuf) -> RepositoryRef {
        let id = root.to_string_lossy().to_string();
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.clone());

        RepositoryRef { id, name, root }
    }

    fn create_worktree_args(
        repository_root: &Path,
        worktree_path: &Path,
        branch: &str,
        base_branch: &str,
    ) -> Vec<OsString> {
        vec![
            OsString::from("-C"),
            repository_root.as_os_str().to_owned(),
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("-b"),
            OsString::from(branch),
            worktree_path.as_os_str().to_owned(),
            OsString::from(base_branch),
        ]
    }

    fn delete_worktree_args(
        repository_root: &Path,
        worktree_path: &Path,
        delete_directory: bool,
    ) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("-C"),
            repository_root.as_os_str().to_owned(),
            OsString::from("worktree"),
            OsString::from("remove"),
        ];
        if delete_directory {
            args.push(OsString::from("--force"));
        }
        args.push(worktree_path.as_os_str().to_owned());
        args
    }

    fn delete_branch_args(repository_root: &Path, branch: &str, force: bool) -> Vec<OsString> {
        vec![
            OsString::from("-C"),
            repository_root.as_os_str().to_owned(),
            OsString::from("branch"),
            OsString::from(if force { "-D" } else { "-d" }),
            OsString::from(branch),
        ]
    }

    fn status_porcelain_args(worktree_path: &Path) -> Vec<OsString> {
        vec![
            OsString::from("-C"),
            worktree_path.as_os_str().to_owned(),
            OsString::from("status"),
            OsString::from("--porcelain"),
        ]
    }

    fn ahead_behind_args(worktree_path: &Path) -> Vec<OsString> {
        vec![
            OsString::from("-C"),
            worktree_path.as_os_str().to_owned(),
            OsString::from("rev-list"),
            OsString::from("--left-right"),
            OsString::from("--count"),
            OsString::from("@{upstream}...HEAD"),
        ]
    }

    fn parse_ahead_behind(stdout: &[u8]) -> Result<(u32, u32), CoreError> {
        let output = String::from_utf8_lossy(stdout);
        let mut parts = output.split_whitespace();

        let behind = parts
            .next()
            .ok_or_else(|| {
                CoreError::DependencyUnavailable(
                    "Git ahead/behind output is missing the `behind` count.".to_owned(),
                )
            })?
            .parse::<u32>()
            .map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Git ahead/behind output has invalid `behind` count: {error}"
                ))
            })?;

        let ahead = parts
            .next()
            .ok_or_else(|| {
                CoreError::DependencyUnavailable(
                    "Git ahead/behind output is missing the `ahead` count.".to_owned(),
                )
            })?
            .parse::<u32>()
            .map_err(|error| {
                CoreError::DependencyUnavailable(format!(
                    "Git ahead/behind output has invalid `ahead` count: {error}"
                ))
            })?;

        Ok((ahead, behind))
    }

    fn validate_worktree_create_request(
        request: &CreateWorktreeRequest,
    ) -> Result<(String, String), CoreError> {
        let branch = request.branch.trim();
        if branch.is_empty() {
            return Err(CoreError::Configuration(
                "Worktree branch must be a non-empty string.".to_owned(),
            ));
        }

        let parsed_branch = Self::parse_worktree_branch(branch)?;
        if let Some(ticket_identifier) = request
            .ticket_identifier
            .as_deref()
            .map(str::trim)
            .filter(|identifier| !identifier.is_empty())
        {
            if !parsed_branch
                .issue_key
                .eq_ignore_ascii_case(ticket_identifier)
            {
                return Err(CoreError::Configuration(format!(
                    "Worktree branch issue key '{}' must match ticket identifier '{}'.",
                    parsed_branch.issue_key, ticket_identifier
                )));
            }
        }

        let normalized_base_branch = Self::normalize_base_branch(request.base_branch.trim());

        Ok((branch.to_owned(), normalized_base_branch))
    }

    fn validate_worktree_delete_request(
        request: &DeleteWorktreeRequest,
    ) -> Result<String, CoreError> {
        let branch = request.worktree.branch.trim();
        if request.delete_branch && branch.is_empty() {
            return Err(CoreError::Configuration(
                "Branch deletion requested but worktree branch is empty.".to_owned(),
            ));
        }

        let normalized_base_branch =
            Self::normalize_base_branch(request.worktree.base_branch.trim());
        if request.delete_branch && branch.eq_ignore_ascii_case(&normalized_base_branch) {
            return Err(CoreError::Configuration(format!(
                "Refusing to delete base branch '{}'.",
                normalized_base_branch
            )));
        }

        if request.delete_branch && Self::parse_worktree_branch(branch).is_err() {
            return Err(CoreError::Configuration(format!(
                "Refusing to delete non-managed branch '{}'; managed branches must match '{}'.",
                branch, WORKTREE_BRANCH_TEMPLATE
            )));
        }

        Ok(branch.to_owned())
    }

    fn normalize_base_branch(base_branch: &str) -> String {
        if base_branch.is_empty() {
            DEFAULT_BASE_BRANCH.to_owned()
        } else {
            base_branch.to_owned()
        }
    }

    fn parse_worktree_branch(branch: &str) -> Result<ParsedWorktreeBranch<'_>, CoreError> {
        let suffix = branch.strip_prefix(WORKTREE_BRANCH_PREFIX).ok_or_else(|| {
            CoreError::Configuration(format!(
                "Worktree branch '{}' must match '{}' (missing '{}' prefix).",
                branch, WORKTREE_BRANCH_TEMPLATE, WORKTREE_BRANCH_PREFIX
            ))
        })?;

        let bytes = suffix.as_bytes();
        if bytes.is_empty() || !bytes[0].is_ascii_uppercase() {
            return Err(CoreError::Configuration(format!(
                "Worktree branch '{}' must include an uppercase issue key before the ticket number.",
                branch
            )));
        }

        let mut index = 1;
        while index < bytes.len()
            && (bytes[index].is_ascii_uppercase() || bytes[index].is_ascii_digit())
        {
            index += 1;
        }

        if index >= bytes.len() || bytes[index] != b'-' {
            return Err(CoreError::Configuration(format!(
                "Worktree branch '{}' must match '{}'.",
                branch, WORKTREE_BRANCH_TEMPLATE
            )));
        }

        index += 1;
        let ticket_number_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }

        if ticket_number_start == index || index >= bytes.len() || bytes[index] != b'-' {
            return Err(CoreError::Configuration(format!(
                "Worktree branch '{}' must include a numeric issue number and a slug (e.g. '{}').",
                branch, WORKTREE_BRANCH_TEMPLATE
            )));
        }

        let issue_key = &suffix[..index];
        let slug = &suffix[index + 1..];
        if !Self::is_valid_branch_slug(slug) {
            return Err(CoreError::Configuration(format!(
                "Worktree branch '{}' has invalid slug '{}'; use lowercase letters, digits, and single hyphens.",
                branch, slug
            )));
        }

        Ok(ParsedWorktreeBranch { issue_key })
    }

    fn is_valid_branch_slug(slug: &str) -> bool {
        if slug.is_empty() || slug.starts_with('-') || slug.ends_with('-') {
            return false;
        }

        let mut previous_was_dash = false;
        for byte in slug.bytes() {
            let is_dash = byte == b'-';
            let valid = byte.is_ascii_lowercase() || byte.is_ascii_digit() || is_dash;
            if !valid {
                return false;
            }
            if is_dash && previous_was_dash {
                return false;
            }
            previous_was_dash = is_dash;
        }

        true
    }

    fn validate_directory_cleanup_target(
        repository_root: &Path,
        worktree_path: &Path,
    ) -> Result<(), CoreError> {
        if worktree_path.parent().is_none() {
            return Err(CoreError::Configuration(format!(
                "Refusing to delete worktree path '{}' because it points to a filesystem root.",
                worktree_path.display()
            )));
        }

        if repository_root == worktree_path {
            return Err(CoreError::Configuration(format!(
                "Refusing to delete repository root '{}' as a worktree directory.",
                repository_root.display()
            )));
        }

        if let (Ok(canonical_repository_root), Ok(canonical_worktree_path)) = (
            fs::canonicalize(repository_root),
            fs::canonicalize(worktree_path),
        ) {
            if canonical_repository_root == canonical_worktree_path {
                return Err(CoreError::Configuration(format!(
                    "Refusing to delete repository root '{}' as a worktree directory.",
                    canonical_repository_root.display()
                )));
            }
        }

        Ok(())
    }

    fn ensure_worktree_parent_exists(worktree_path: &Path) -> Result<(), CoreError> {
        let parent = worktree_path.parent().ok_or_else(|| {
            CoreError::Configuration(format!(
                "Worktree path '{}' has no parent directory.",
                worktree_path.display()
            ))
        })?;

        fs::create_dir_all(parent).map_err(|error| {
            CoreError::Configuration(format!(
                "Failed to create parent directory '{}' for worktree: {error}",
                parent.display()
            ))
        })
    }

    fn cleanup_worktree_directory(path: &Path) -> Result<(), CoreError> {
        if !path.exists() {
            return Ok(());
        }

        let metadata = fs::symlink_metadata(path).map_err(|error| {
            CoreError::Configuration(format!(
                "Failed to inspect worktree path '{}' for cleanup: {error}",
                path.display()
            ))
        })?;

        if metadata.is_dir() {
            fs::remove_dir_all(path).map_err(|error| {
                CoreError::Configuration(format!(
                    "Failed to remove worktree directory '{}': {error}",
                    path.display()
                ))
            })
        } else {
            fs::remove_file(path).map_err(|error| {
                CoreError::Configuration(format!(
                    "Failed to remove worktree path '{}': {error}",
                    path.display()
                ))
            })
        }
    }

    fn maybe_read_ahead_behind(&self, worktree_path: &Path) -> Result<(u32, u32), CoreError> {
        let args = Self::ahead_behind_args(worktree_path);
        let output = self.run_git_raw(&args)?;
        if output.status.success() {
            return Self::parse_ahead_behind(&output.stdout);
        }

        let detail = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stderr).to_ascii_lowercase(),
            String::from_utf8_lossy(&output.stdout).to_ascii_lowercase()
        );
        if detail.contains("no upstream") || detail.contains("unknown revision") {
            return Ok((0, 0));
        }

        Err(self.command_failed(&args, &output))
    }
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool, CoreError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(CoreError::Configuration(format!(
            "{name} must be a boolean (true/false)."
        ))),
    }
}

#[async_trait::async_trait]
impl<R: CommandRunner> VcsProvider for GitCliVcsProvider<R> {
    async fn health_check(&self) -> Result<(), CoreError> {
        self.run_git(&Self::health_check_args())?;
        Ok(())
    }

    async fn discover_repositories(
        &self,
        roots: &[PathBuf],
    ) -> Result<Vec<RepositoryRef>, CoreError> {
        let mut discovered = BTreeSet::new();
        for root in roots {
            for repository_root in self.discover_repositories_under_root(root)? {
                discovered.insert(repository_root);
            }
        }

        Ok(discovered
            .into_iter()
            .map(Self::repository_ref_from_path)
            .collect())
    }

    async fn create_worktree(
        &self,
        request: CreateWorktreeRequest,
    ) -> Result<WorktreeSummary, CoreError> {
        let (branch, base_branch) = Self::validate_worktree_create_request(&request)?;
        Self::ensure_worktree_parent_exists(&request.worktree_path)?;

        let args = Self::create_worktree_args(
            &request.repository.root,
            &request.worktree_path,
            &branch,
            &base_branch,
        );
        self.run_git(&args)?;

        Ok(WorktreeSummary {
            worktree_id: request.worktree_id,
            repository: request.repository,
            path: request.worktree_path,
            branch,
            base_branch,
        })
    }

    async fn delete_worktree(&self, request: DeleteWorktreeRequest) -> Result<(), CoreError> {
        let branch = Self::validate_worktree_delete_request(&request)?;
        if request.delete_directory {
            Self::validate_directory_cleanup_target(
                &request.worktree.repository.root,
                &request.worktree.path,
            )?;
        }

        let remove_args = Self::delete_worktree_args(
            &request.worktree.repository.root,
            &request.worktree.path,
            request.delete_directory,
        );
        self.run_git(&remove_args)?;

        if request.delete_branch {
            let delete_args = Self::delete_branch_args(
                &request.worktree.repository.root,
                &branch,
                self.allow_force_delete_unmerged_branches,
            );
            self.run_git(&delete_args)?;
        }

        if request.delete_directory {
            Self::cleanup_worktree_directory(&request.worktree.path)?;
        }

        Ok(())
    }

    async fn worktree_status(&self, worktree_path: &Path) -> Result<WorktreeStatus, CoreError> {
        let status_args = Self::status_porcelain_args(worktree_path);
        let status_output = self.run_git(&status_args)?;
        let is_dirty = !String::from_utf8_lossy(&status_output.stdout)
            .trim()
            .is_empty();
        let (commits_ahead, commits_behind) = self.maybe_read_ahead_behind(worktree_path)?;

        Ok(WorktreeStatus {
            is_dirty,
            commits_ahead,
            commits_behind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{CreateWorktreeRequest, WorktreeId, WorktreeSummary};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct StubRunner {
        calls: Mutex<Vec<(String, Vec<OsString>)>>,
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
        fn run(&self, program: &str, args: &[OsString]) -> io::Result<std::process::Output> {
            self.calls
                .lock()
                .expect("lock")
                .push((program.to_owned(), args.to_vec()));

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
        }
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "orchestrator-{label}-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn output_with_status(code: i32, stdout: &[u8], stderr: &[u8]) -> std::process::Output {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(code),
                stdout: stdout.to_vec(),
                stderr: stderr.to_vec(),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(code as u32),
                stdout: stdout.to_vec(),
                stderr: stderr.to_vec(),
            }
        }
    }

    fn success_output() -> std::process::Output {
        output_with_status(0, &[], &[])
    }

    fn sample_repository(root: PathBuf) -> RepositoryRef {
        RepositoryRef {
            id: root.to_string_lossy().to_string(),
            name: "repo".to_owned(),
            root,
        }
    }

    fn sample_worktree_summary(repository_root: PathBuf) -> WorktreeSummary {
        WorktreeSummary {
            worktree_id: WorktreeId::new("wt-1"),
            repository: sample_repository(repository_root),
            path: PathBuf::from("/tmp/orchestrator/wt-1"),
            branch: "ap/AP-122-repo-discovery".to_owned(),
            base_branch: "main".to_owned(),
        }
    }

    #[tokio::test]
    async fn command_construction_uses_argument_vector() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);
        provider.health_check().await.expect("health check");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "git");
        assert_eq!(
            calls[0].1,
            GitCliVcsProvider::<ProcessCommandRunner>::health_check_args()
        );
    }

    #[tokio::test]
    async fn discover_repositories_finds_nested_git_roots_under_configured_root() {
        let project_root = TempDir::new("git-discovery");
        let repo_a = project_root.path.join("repo-a");
        let repo_b = project_root.path.join("nested/repo-b");
        fs::create_dir_all(repo_a.join(".git")).expect("repo a");
        fs::create_dir_all(&repo_b).expect("repo b");
        fs::write(repo_b.join(".git"), "gitdir: /tmp/shared").expect("worktree-style git file");

        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let repositories = provider
            .discover_repositories(std::slice::from_ref(&project_root.path))
            .await
            .expect("discover repositories");

        assert_eq!(repositories.len(), 2);
        assert!(repositories.iter().any(|repo| repo.root == repo_a));
        assert!(repositories.iter().any(|repo| repo.root == repo_b));
    }

    #[tokio::test]
    async fn discover_repositories_ignores_non_git_marker_files() {
        let project_root = TempDir::new("git-discovery-ignore-markers");
        let fake_repo = project_root.path.join("not-a-repo");
        let real_repo = project_root.path.join("real-repo");

        fs::create_dir_all(&fake_repo).expect("fake repo directory");
        fs::write(fake_repo.join(".git"), "not a gitdir marker").expect("fake marker");
        fs::create_dir_all(real_repo.join(".git")).expect("real repo directory");

        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let repositories = provider
            .discover_repositories(std::slice::from_ref(&project_root.path))
            .await
            .expect("discover repositories");

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].root, real_repo);
    }

    #[tokio::test]
    async fn create_worktree_uses_expected_git_arguments() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = CreateWorktreeRequest {
            worktree_id: WorktreeId::new("wt-122"),
            repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
            worktree_path: PathBuf::from("/tmp/orchestrator/worktrees/wt-122"),
            branch: "ap/AP-122-repo-discovery".to_owned(),
            base_branch: "main".to_owned(),
            ticket_identifier: Some("AP-122".to_owned()),
        };

        provider
            .create_worktree(request.clone())
            .await
            .expect("create worktree");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            GitCliVcsProvider::<ProcessCommandRunner>::create_worktree_args(
                &request.repository.root,
                &request.worktree_path,
                &request.branch,
                &request.base_branch,
            )
        );
    }

    #[tokio::test]
    async fn create_worktree_defaults_blank_base_branch_to_main() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = CreateWorktreeRequest {
            worktree_id: WorktreeId::new("wt-123"),
            repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
            worktree_path: PathBuf::from("/tmp/orchestrator/worktrees/wt-123"),
            branch: "ap/AP-123-branch-policy".to_owned(),
            base_branch: "   ".to_owned(),
            ticket_identifier: Some("AP-123".to_owned()),
        };

        let summary = provider
            .create_worktree(request.clone())
            .await
            .expect("create worktree");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            GitCliVcsProvider::<ProcessCommandRunner>::create_worktree_args(
                &request.repository.root,
                &request.worktree_path,
                &request.branch,
                "main",
            )
        );
        assert_eq!(summary.base_branch, "main");
    }

    #[tokio::test]
    async fn create_worktree_rejects_branch_outside_policy_template() {
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = CreateWorktreeRequest {
            worktree_id: WorktreeId::new("wt-124"),
            repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
            worktree_path: PathBuf::from("/tmp/orchestrator/worktrees/wt-124"),
            branch: "feature/AP-124-branch-policy".to_owned(),
            base_branch: "main".to_owned(),
            ticket_identifier: Some("AP-124".to_owned()),
        };

        let err = provider
            .create_worktree(request)
            .await
            .expect_err("expected branch naming validation error");
        assert!(err.to_string().contains("must match"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn create_worktree_rejects_ticket_identifier_mismatch() {
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = CreateWorktreeRequest {
            worktree_id: WorktreeId::new("wt-125"),
            repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
            worktree_path: PathBuf::from("/tmp/orchestrator/worktrees/wt-125"),
            branch: "ap/AP-125-branch-policy".to_owned(),
            base_branch: "main".to_owned(),
            ticket_identifier: Some("AP-999".to_owned()),
        };

        let err = provider
            .create_worktree(request)
            .await
            .expect_err("expected branch/ticket mismatch error");
        assert!(err.to_string().contains("must match ticket identifier"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn create_worktree_accepts_issue_keys_with_digits_and_case_insensitive_ticket_match() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = CreateWorktreeRequest {
            worktree_id: WorktreeId::new("wt-126"),
            repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
            worktree_path: PathBuf::from("/tmp/orchestrator/worktrees/wt-126"),
            branch: "ap/ORCH2-126-start-resume-flow".to_owned(),
            base_branch: "main".to_owned(),
            ticket_identifier: Some("orch2-126".to_owned()),
        };

        provider
            .create_worktree(request)
            .await
            .expect("create worktree");
    }

    #[tokio::test]
    async fn create_worktree_rejects_invalid_slug_characters() {
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = CreateWorktreeRequest {
            worktree_id: WorktreeId::new("wt-127"),
            repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
            worktree_path: PathBuf::from("/tmp/orchestrator/worktrees/wt-127"),
            branch: "ap/AP-127-Invalid-Slug".to_owned(),
            base_branch: "main".to_owned(),
            ticket_identifier: Some("AP-127".to_owned()),
        };

        let err = provider
            .create_worktree(request)
            .await
            .expect_err("expected invalid slug validation error");
        assert!(err.to_string().contains("invalid slug"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn delete_worktree_only_removes_branch_when_requested() {
        let runner = StubRunner::with_results(vec![Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = DeleteWorktreeRequest {
            worktree: sample_worktree_summary(PathBuf::from("/tmp/orchestrator/repo")),
            delete_branch: false,
            delete_directory: false,
        };

        provider
            .delete_worktree(request.clone())
            .await
            .expect("delete worktree");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            GitCliVcsProvider::<ProcessCommandRunner>::delete_worktree_args(
                &request.worktree.repository.root,
                &request.worktree.path,
                request.delete_directory,
            )
        );
    }

    #[tokio::test]
    async fn delete_worktree_uses_safe_branch_delete_by_default() {
        let runner = StubRunner::with_results(vec![Ok(success_output()), Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = DeleteWorktreeRequest {
            worktree: sample_worktree_summary(PathBuf::from("/tmp/orchestrator/repo")),
            delete_branch: true,
            delete_directory: true,
        };

        provider
            .delete_worktree(request.clone())
            .await
            .expect("delete worktree + branch");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1].1,
            GitCliVcsProvider::<ProcessCommandRunner>::delete_branch_args(
                &request.worktree.repository.root,
                &request.worktree.branch,
                false,
            )
        );
    }

    #[tokio::test]
    async fn delete_worktree_can_force_delete_branch_when_configured() {
        let runner = StubRunner::with_results(vec![Ok(success_output()), Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), true);

        let request = DeleteWorktreeRequest {
            worktree: sample_worktree_summary(PathBuf::from("/tmp/orchestrator/repo")),
            delete_branch: true,
            delete_directory: true,
        };

        provider
            .delete_worktree(request.clone())
            .await
            .expect("delete worktree + branch");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1].1,
            GitCliVcsProvider::<ProcessCommandRunner>::delete_branch_args(
                &request.worktree.repository.root,
                &request.worktree.branch,
                true,
            )
        );
    }

    #[tokio::test]
    async fn delete_worktree_refuses_to_delete_repository_root_directory() {
        let repository_root = TempDir::new("repo-root");
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = DeleteWorktreeRequest {
            worktree: WorktreeSummary {
                worktree_id: WorktreeId::new("wt-1"),
                repository: sample_repository(repository_root.path.clone()),
                path: repository_root.path.clone(),
                branch: "ap/AP-122-repo-discovery".to_owned(),
                base_branch: "main".to_owned(),
            },
            delete_branch: false,
            delete_directory: true,
        };

        let err = provider
            .delete_worktree(request)
            .await
            .expect_err("expected safety error");
        assert!(err
            .to_string()
            .contains("Refusing to delete repository root"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn delete_worktree_refuses_to_delete_non_managed_branch() {
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = DeleteWorktreeRequest {
            worktree: WorktreeSummary {
                worktree_id: WorktreeId::new("wt-128"),
                repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
                path: PathBuf::from("/tmp/orchestrator/wt-128"),
                branch: "feature/AP-128-workflow".to_owned(),
                base_branch: "main".to_owned(),
            },
            delete_branch: true,
            delete_directory: false,
        };

        let err = provider
            .delete_worktree(request)
            .await
            .expect_err("expected managed branch safety error");
        assert!(err
            .to_string()
            .contains("Refusing to delete non-managed branch"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn delete_worktree_refuses_to_delete_default_base_branch_when_base_branch_is_blank() {
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let request = DeleteWorktreeRequest {
            worktree: WorktreeSummary {
                worktree_id: WorktreeId::new("wt-129"),
                repository: sample_repository(PathBuf::from("/tmp/orchestrator/repo")),
                path: PathBuf::from("/tmp/orchestrator/wt-129"),
                branch: "main".to_owned(),
                base_branch: " ".to_owned(),
            },
            delete_branch: true,
            delete_directory: false,
        };

        let err = provider
            .delete_worktree(request)
            .await
            .expect_err("expected base branch protection");
        assert!(err
            .to_string()
            .contains("Refusing to delete base branch 'main'"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn missing_git_binary_is_actionable() {
        let runner = StubRunner::with_results(vec![Err(io::Error::new(
            io::ErrorKind::NotFound,
            "missing",
        ))]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let err = provider.health_check().await.expect_err("expected error");
        assert!(err.to_string().contains("Install Git"));
    }

    #[tokio::test]
    async fn worktree_status_parses_dirty_and_upstream_divergence() {
        let runner = StubRunner::with_results(vec![
            Ok(output_with_status(0, b" M src/lib.rs\n", b"")),
            Ok(output_with_status(0, b"2\t5\n", b"")),
        ]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let status = provider
            .worktree_status(Path::new("/tmp/orchestrator/worktrees/wt-1"))
            .await
            .expect("status");

        assert!(status.is_dirty);
        assert_eq!(status.commits_behind, 2);
        assert_eq!(status.commits_ahead, 5);
    }

    #[tokio::test]
    async fn worktree_status_defaults_ahead_behind_when_no_upstream_exists() {
        let runner = StubRunner::with_results(vec![
            Ok(output_with_status(0, b"", b"")),
            Ok(output_with_status(
                1,
                b"",
                b"fatal: no upstream configured\n",
            )),
        ]);
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);

        let status = provider
            .worktree_status(Path::new("/tmp/orchestrator/worktrees/wt-1"))
            .await
            .expect("status");

        assert!(!status.is_dirty);
        assert_eq!(status.commits_behind, 0);
        assert_eq!(status.commits_ahead, 0);
    }

    #[test]
    fn parse_ahead_behind_rejects_non_numeric_output() {
        let err = GitCliVcsProvider::<ProcessCommandRunner>::parse_ahead_behind(b"behind\t2\n")
            .expect_err("expected parse error");
        assert!(err.to_string().contains("invalid `behind`"));
    }
}
