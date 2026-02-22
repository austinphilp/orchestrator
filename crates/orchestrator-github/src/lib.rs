pub use orchestrator_vcs_repos::{
    default_system_url_opener, CommandRunner, GitHubGhCliRepoProvider,
    GitHubGhCliRepoProviderConfig, ProcessCommandRunner, SystemUrlOpener,
};

pub type GhCliClient<R = ProcessCommandRunner> = GitHubGhCliRepoProvider<R>;
