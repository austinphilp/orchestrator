impl<R: CommandRunner> GitCliVcsProvider<R> {
    pub fn new(
        runner: R,
        binary: PathBuf,
        allow_destructive_automation: bool,
        allow_force_push: bool,
        allow_force_delete_unmerged_branches: bool,
        allow_unsafe_command_paths: bool,
    ) -> Result<Self, CoreError> {
        if binary.as_os_str().is_empty() {
            return Err(CoreError::Configuration(format!(
                "{ENV_GIT_BIN} is set but empty. Provide a valid git binary path or unset it."
            )));
        }

        if allow_force_push && !allow_destructive_automation {
            return Err(CoreError::Configuration(format!(
                "{ENV_ALLOW_FORCE_PUSH}=true requires {ENV_ALLOW_DESTRUCTIVE_AUTOMATION}=true."
            )));
        }
        if allow_force_delete_unmerged_branches && !allow_destructive_automation {
            return Err(CoreError::Configuration(format!(
                "{ENV_ALLOW_DELETE_UNMERGED_BRANCHES}=true requires {ENV_ALLOW_DESTRUCTIVE_AUTOMATION}=true."
            )));
        }

        Ok(Self::with_binary_and_safety(
            runner,
            binary,
            allow_destructive_automation,
            allow_force_push,
            allow_force_delete_unmerged_branches,
            allow_unsafe_command_paths,
        ))
    }

    pub fn with_binary(
        runner: R,
        binary: PathBuf,
        allow_force_delete_unmerged_branches: bool,
    ) -> Self {
        Self::with_binary_and_safety(
            runner,
            binary,
            false,
            false,
            allow_force_delete_unmerged_branches,
            false,
        )
    }

    pub fn with_binary_and_safety(
        runner: R,
        binary: PathBuf,
        allow_destructive_automation: bool,
        allow_force_push: bool,
        allow_force_delete_unmerged_branches: bool,
        allow_unsafe_command_paths: bool,
    ) -> Self {
        Self {
            runner,
            binary,
            allow_destructive_automation,
            allow_force_push,
            allow_force_delete_unmerged_branches,
            allow_unsafe_command_paths,
        }
    }

    pub fn health_check_args() -> Vec<OsString> {
        vec![OsString::from("--version")]
    }

    fn run_git_raw(&self, args: &[OsString]) -> Result<std::process::Output, CoreError> {
        validate_command_binary_path(&self.binary, ENV_GIT_BIN, self.allow_unsafe_command_paths)?;
        Self::validate_git_command_safety(
            args,
            self.allow_destructive_automation,
            self.allow_force_push,
        )?;

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

    fn validate_git_command_safety(
        args: &[OsString],
        allow_destructive_automation: bool,
        allow_force_push: bool,
    ) -> Result<(), CoreError> {
        let Some(push_args) = Self::push_subcommand_args(args) else {
            return Ok(());
        };

        if !Self::contains_force_push_flag(push_args) {
            return Ok(());
        }

        if !allow_destructive_automation {
            return Err(CoreError::Configuration(format!(
                "Refusing to run force-push via git automation by default. Set {ENV_ALLOW_DESTRUCTIVE_AUTOMATION}=true and {ENV_ALLOW_FORCE_PUSH}=true to explicitly allow this destructive path."
            )));
        }

        if !allow_force_push {
            return Err(CoreError::Configuration(format!(
                "Refusing to run force-push via git automation by default. Set {ENV_ALLOW_FORCE_PUSH}=true to explicitly allow this destructive path."
            )));
        }

        Ok(())
    }

    fn push_subcommand_args(args: &[OsString]) -> Option<&[OsString]> {
        let index = args
            .iter()
            .position(|arg| arg.to_string_lossy().eq_ignore_ascii_case("push"))?;
        Some(&args[index + 1..])
    }

    fn contains_force_push_flag(args: &[OsString]) -> bool {
        args.iter()
            .take_while(|arg| arg.to_string_lossy() != "--")
            .any(|arg| {
                let normalized = arg.to_string_lossy().to_ascii_lowercase();
                if normalized == "--force" || normalized.starts_with("--force=") {
                    return true;
                }
                if normalized == "--force-with-lease"
                    || normalized.starts_with("--force-with-lease=")
                {
                    return true;
                }
                if normalized == "--force-if-includes"
                    || normalized.starts_with("--force-if-includes=")
                {
                    return true;
                }

                if normalized.starts_with('-') && !normalized.starts_with("--") {
                    return normalized[1..].chars().any(|flag| flag == 'f');
                }

                false
            })
    }

    fn discover_repositories_under_root(&self, root: &Path) -> Result<Vec<PathBuf>, CoreError> {
        if !root.exists() {
            fs::create_dir_all(root).map_err(|error| {
                CoreError::Configuration(format!(
                    "Failed to create configured project root '{}': {error}",
                    root.display()
                ))
            })?;
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

    fn create_worktree_existing_branch_args(
        repository_root: &Path,
        worktree_path: &Path,
        branch: &str,
    ) -> Vec<OsString> {
        vec![
            OsString::from("-C"),
            repository_root.as_os_str().to_owned(),
            OsString::from("worktree"),
            OsString::from("add"),
            worktree_path.as_os_str().to_owned(),
            OsString::from(branch),
        ]
    }

    fn worktree_prune_args(repository_root: &Path) -> Vec<OsString> {
        vec![
            OsString::from("-C"),
            repository_root.as_os_str().to_owned(),
            OsString::from("worktree"),
            OsString::from("prune"),
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

    fn is_branch_already_exists_error(error: &CoreError) -> bool {
        let message = error.to_string().to_ascii_lowercase();
        message.contains("branch named") && message.contains("already exists")
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

    fn ensure_destructive_delete_allowed(
        &self,
        request: &DeleteWorktreeRequest,
    ) -> Result<(), CoreError> {
        if !request.delete_branch && !request.delete_directory {
            return Ok(());
        }

        if self.allow_destructive_automation {
            return Ok(());
        }

        Err(CoreError::Configuration(format!(
            "Refusing destructive worktree cleanup (delete_branch/delete_directory) by default. Set {ENV_ALLOW_DESTRUCTIVE_AUTOMATION}=true to explicitly allow it."
        )))
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
