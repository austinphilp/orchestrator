use orchestrator_core::{CoreError, Supervisor};

#[derive(Debug, Clone)]
pub struct OpenRouterSupervisor {
    api_key: String,
}

impl OpenRouterSupervisor {
    pub fn from_env() -> Result<Self, CoreError> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            CoreError::Configuration(
                "OPENROUTER_API_KEY is not set. Export a valid key before starting orchestrator-app."
                    .to_owned(),
            )
        })?;

        if api_key.trim().is_empty() {
            return Err(CoreError::Configuration(
                "OPENROUTER_API_KEY is empty. Provide a non-empty API key.".to_owned(),
            ));
        }

        Ok(Self { api_key })
    }
}

#[async_trait::async_trait]
impl Supervisor for OpenRouterSupervisor {
    async fn health_check(&self) -> Result<(), CoreError> {
        if self.api_key.is_empty() {
            return Err(CoreError::Configuration(
                "OpenRouter supervisor was initialized without credentials.".to_owned(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::test_support::with_env_var;

    #[test]
    fn missing_openrouter_api_key_is_actionable() {
        with_env_var("OPENROUTER_API_KEY", None, || {
            let result = OpenRouterSupervisor::from_env();
            let err = result.expect_err("expected missing env var to fail");
            assert!(err.to_string().contains("OPENROUTER_API_KEY"));
        });
    }

    #[test]
    fn empty_openrouter_api_key_is_rejected() {
        with_env_var("OPENROUTER_API_KEY", Some("   "), || {
            let result = OpenRouterSupervisor::from_env();
            assert!(result.is_err());
        });
    }
}
