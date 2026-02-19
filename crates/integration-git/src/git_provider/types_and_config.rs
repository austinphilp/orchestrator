use std::collections::{BTreeSet, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use orchestrator_core::{
    CoreError, CreateWorktreeRequest, DeleteWorktreeRequest, RepositoryRef, VcsProvider,
    WorktreeStatus, WorktreeSummary,
};

const ENV_GIT_BIN: &str = "ORCHESTRATOR_GIT_BIN";
const ENV_ALLOW_DELETE_UNMERGED_BRANCHES: &str = "ORCHESTRATOR_GIT_ALLOW_DELETE_UNMERGED_BRANCHES";
const ENV_ALLOW_DESTRUCTIVE_AUTOMATION: &str = "ORCHESTRATOR_GIT_ALLOW_DESTRUCTIVE_AUTOMATION";
const ENV_ALLOW_FORCE_PUSH: &str = "ORCHESTRATOR_GIT_ALLOW_FORCE_PUSH";
const ENV_ALLOW_UNSAFE_COMMAND_PATHS: &str = "ORCHESTRATOR_ALLOW_UNSAFE_COMMAND_PATHS";
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
    allow_destructive_automation: bool,
    allow_force_push: bool,
    allow_force_delete_unmerged_branches: bool,
    allow_unsafe_command_paths: bool,
}

struct ParsedWorktreeBranch<'a> {
    issue_key: &'a str,
}

