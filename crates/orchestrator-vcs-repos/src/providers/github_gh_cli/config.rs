#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubGhCliRepoProviderConfig {
    pub binary_name: String,
}

impl Default for GitHubGhCliRepoProviderConfig {
    fn default() -> Self {
        Self {
            binary_name: "gh".to_owned(),
        }
    }
}
