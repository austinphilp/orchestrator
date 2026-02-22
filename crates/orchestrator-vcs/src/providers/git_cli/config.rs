#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCliVcsProviderConfig {
    pub binary_name: String,
}

impl Default for GitCliVcsProviderConfig {
    fn default() -> Self {
        Self {
            binary_name: "git".to_owned(),
        }
    }
}
