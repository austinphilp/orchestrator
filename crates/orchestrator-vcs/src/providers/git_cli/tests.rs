#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{
        CoreVcsProvider, CreateWorktreeRequest, VcsProvider as LocalVcsProvider, WorktreeId,
        WorktreeSummary,
    };
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

    #[test]
    fn provider_identity_is_namespaced_git_cli() {
        let provider = GitCliVcsProvider::with_binary(
            StubRunner::with_results(Vec::new()),
            PathBuf::from("git"),
            false,
        );
        assert_eq!(
            LocalVcsProvider::kind(&provider),
            crate::interface::VcsProviderKind::GitCli
        );
        assert_eq!(LocalVcsProvider::provider_key(&provider), "vcs.git_cli");
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
    async fn create_worktree_reuses_existing_branch_when_branch_already_exists() {
        let runner = StubRunner::with_results(vec![
            Ok(output_with_status(
                1,
                b"",
                b"fatal: a branch named 'ap/AP-122-repo-discovery' already exists\n",
            )),
            Ok(success_output()),
            Ok(success_output()),
        ]);
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
            .expect("create worktree with existing branch");

        let calls = provider.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0].1,
            GitCliVcsProvider::<ProcessCommandRunner>::create_worktree_args(
                &request.repository.root,
                &request.worktree_path,
                &request.branch,
                &request.base_branch,
            )
        );
        assert_eq!(
            calls[1].1,
            GitCliVcsProvider::<ProcessCommandRunner>::worktree_prune_args(
                &request.repository.root
            )
        );
        assert_eq!(
            calls[2].1,
            GitCliVcsProvider::<ProcessCommandRunner>::create_worktree_existing_branch_args(
                &request.repository.root,
                &request.worktree_path,
                &request.branch,
            )
        );
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
    async fn delete_worktree_rejects_destructive_cleanup_when_not_explicitly_enabled() {
        let runner = StubRunner::with_results(Vec::new());
        let provider = GitCliVcsProvider::with_binary(runner, PathBuf::from("git"), false);
        let request = DeleteWorktreeRequest {
            worktree: sample_worktree_summary(PathBuf::from("/tmp/orchestrator/repo")),
            delete_branch: true,
            delete_directory: true,
        };

        let err = provider
            .delete_worktree(request)
            .await
            .expect_err("destructive cleanup should be blocked by default");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_GIT_ALLOW_DESTRUCTIVE_AUTOMATION"));
        assert!(provider.runner.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn delete_worktree_uses_safe_branch_delete_by_default() {
        let runner = StubRunner::with_results(vec![Ok(success_output()), Ok(success_output())]);
        let provider = GitCliVcsProvider::with_binary_and_safety(
            runner,
            PathBuf::from("git"),
            true,
            false,
            false,
            false,
        );

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
        let provider = GitCliVcsProvider::with_binary_and_safety(
            runner,
            PathBuf::from("git"),
            true,
            false,
            true,
            false,
        );

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
        let provider = GitCliVcsProvider::with_binary_and_safety(
            runner,
            PathBuf::from("git"),
            true,
            false,
            false,
            false,
        );

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
        let provider = GitCliVcsProvider::with_binary_and_safety(
            runner,
            PathBuf::from("git"),
            true,
            false,
            false,
            false,
        );

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
        let provider = GitCliVcsProvider::with_binary_and_safety(
            runner,
            PathBuf::from("git"),
            true,
            false,
            false,
            false,
        );

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

    #[test]
    fn force_push_is_rejected_by_default() {
        let args = vec![OsString::from("push"), OsString::from("--force-with-lease")];
        let err = GitCliVcsProvider::<ProcessCommandRunner>::validate_git_command_safety(
            &args, true, false,
        )
        .expect_err("force push should be rejected by default");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_GIT_ALLOW_FORCE_PUSH"));
    }

    #[test]
    fn force_push_with_combined_short_flags_is_rejected_by_default() {
        let args = vec![
            OsString::from("push"),
            OsString::from("-fu"),
            OsString::from("origin"),
            OsString::from("main"),
        ];
        let err = GitCliVcsProvider::<ProcessCommandRunner>::validate_git_command_safety(
            &args, true, false,
        )
        .expect_err("combined short force flag should be rejected by default");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_GIT_ALLOW_FORCE_PUSH"));
    }

    #[test]
    fn force_push_with_parameterized_force_with_lease_is_rejected_by_default() {
        let args = vec![
            OsString::from("push"),
            OsString::from("--force-with-lease=main"),
            OsString::from("origin"),
            OsString::from("main"),
        ];
        let err = GitCliVcsProvider::<ProcessCommandRunner>::validate_git_command_safety(
            &args, true, false,
        )
        .expect_err("parameterized force-with-lease should be rejected by default");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_GIT_ALLOW_FORCE_PUSH"));
    }

    #[test]
    fn force_push_requires_global_destructive_opt_in_even_when_force_push_flag_is_enabled() {
        let args = vec![OsString::from("push"), OsString::from("--force")];
        let err = GitCliVcsProvider::<ProcessCommandRunner>::validate_git_command_safety(
            &args, false, true,
        )
        .expect_err("force push should require global destructive opt-in");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_GIT_ALLOW_DESTRUCTIVE_AUTOMATION"));
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_GIT_ALLOW_FORCE_PUSH"));
    }

    #[test]
    fn force_push_is_allowed_only_when_all_destructive_opt_ins_are_enabled() {
        let args = vec![
            OsString::from("push"),
            OsString::from("--force-if-includes"),
            OsString::from("origin"),
            OsString::from("main"),
        ];
        GitCliVcsProvider::<ProcessCommandRunner>::validate_git_command_safety(&args, true, true)
            .expect("force push should be allowed when all opt-ins are enabled");
    }

    #[test]
    fn command_path_guard_rejects_non_bare_binary_by_default() {
        let err = validate_command_binary_path(Path::new("./bin/git"), ENV_GIT_BIN, false)
            .expect_err("relative command paths should be blocked by default");
        assert!(err
            .to_string()
            .contains("ORCHESTRATOR_ALLOW_UNSAFE_COMMAND_PATHS"));
    }

    #[test]
    fn command_path_guard_accepts_non_bare_binary_when_explicitly_enabled() {
        validate_command_binary_path(Path::new("./bin/git"), ENV_GIT_BIN, true)
            .expect("unsafe command path opt-in should pass");
    }
}
