#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCliVcsTransport {
    pub protocol: String,
}

impl Default for GitCliVcsTransport {
    fn default() -> Self {
        Self {
            protocol: "cli".to_owned(),
        }
    }
}
