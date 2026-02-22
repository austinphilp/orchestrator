#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubGhCliRepoTransport {
    pub protocol: String,
}

impl Default for GitHubGhCliRepoTransport {
    fn default() -> Self {
        Self {
            protocol: "cli".to_owned(),
        }
    }
}
