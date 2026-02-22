use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsProviderKind {
    GitCli,
}

impl VcsProviderKind {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::GitCli => "vcs.git_cli",
        }
    }

    pub fn from_key(provider_key: &str) -> Option<Self> {
        match provider_key {
            "vcs.git_cli" => Some(Self::GitCli),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsCommandRequest {
    pub worktree_path: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsCommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait VcsProvider: Send + Sync {
    fn kind(&self) -> VcsProviderKind;

    fn provider_key(&self) -> &'static str {
        self.kind().as_key()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsProviderError {
    #[error("unknown vcs provider key: {0}")]
    UnknownProviderKey(String),
}
