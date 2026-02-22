fn is_bare_command_name(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
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

impl<R: CommandRunner> crate::interface::VcsProvider for GitCliVcsProvider<R> {
    fn kind(&self) -> crate::interface::VcsProviderKind {
        crate::interface::VcsProviderKind::GitCli
    }
}

#[async_trait::async_trait]
impl<R: CommandRunner> orchestrator_core::VcsProvider for GitCliVcsProvider<R> {
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
        if let Err(error) = self.run_git(&args) {
            if !Self::is_branch_already_exists_error(&error) {
                return Err(error);
            }

            let prune_args = Self::worktree_prune_args(&request.repository.root);
            let _ = self.run_git(&prune_args);
            let fallback_args = Self::create_worktree_existing_branch_args(
                &request.repository.root,
                &request.worktree_path,
                &branch,
            );
            self.run_git(&fallback_args)?;
        }

        Ok(WorktreeSummary {
            worktree_id: request.worktree_id,
            repository: request.repository,
            path: request.worktree_path,
            branch,
            base_branch,
        })
    }

    async fn delete_worktree(&self, request: DeleteWorktreeRequest) -> Result<(), CoreError> {
        self.ensure_destructive_delete_allowed(&request)?;
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
