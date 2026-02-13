use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use orchestrator_core::{CoreError, GithubClient};

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

pub struct GhCliClient<R: CommandRunner> {
    runner: R,
    binary: PathBuf,
}

impl<R: CommandRunner> GhCliClient<R> {
    pub fn new(runner: R) -> Result<Self, CoreError> {
        let binary = std::env::var_os("ORCHESTRATOR_GH_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("gh"));

        Ok(Self { runner, binary })
    }

    pub fn command_args() -> Vec<OsString> {
        vec![OsString::from("auth"), OsString::from("status")]
    }
}

#[async_trait::async_trait]
impl<R: CommandRunner> GithubClient for GhCliClient<R> {
    async fn health_check(&self) -> Result<(), CoreError> {
        let args = Self::command_args();
        let output = self
            .runner
            .run(
                self.binary
                    .to_str()
                    .ok_or_else(|| CoreError::Configuration("Invalid gh binary path".to_owned()))?,
                &args,
            )
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CoreError::DependencyUnavailable(
                    "GitHub CLI `gh` was not found in PATH. Install gh and authenticate with `gh auth login`."
                        .to_owned(),
                ),
                _ => CoreError::DependencyUnavailable(format!(
                    "Failed to execute GitHub CLI `gh`: {error}"
                )),
            })?;

        if output.status.success() {
            return Ok(());
        }

        Err(CoreError::DependencyUnavailable(
            "GitHub CLI is not authenticated. Run `gh auth login` and retry.".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct StubRunner {
        calls: Mutex<Vec<(String, Vec<OsString>)>>,
        result: io::Result<std::process::Output>,
    }

    impl CommandRunner for StubRunner {
        fn run(&self, program: &str, args: &[OsString]) -> io::Result<std::process::Output> {
            self.calls
                .lock()
                .expect("lock")
                .push((program.to_owned(), args.to_vec()));
            self.result
                .as_ref()
                .map(|o| std::process::Output {
                    status: o.status,
                    stdout: o.stdout.clone(),
                    stderr: o.stderr.clone(),
                })
                .map_err(|e| io::Error::new(e.kind(), e.to_string()))
        }
    }

    fn success_output() -> std::process::Output {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            }
        }
    }

    #[tokio::test]
    async fn command_construction_uses_argument_vector() {
        let runner = StubRunner {
            calls: Mutex::new(Vec::new()),
            result: Ok(success_output()),
        };
        let client = GhCliClient::new(runner).expect("init");
        client.health_check().await.expect("health check");

        let calls = client.runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            GhCliClient::<ProcessCommandRunner>::command_args()
        );
    }

    #[tokio::test]
    async fn missing_gh_binary_is_actionable() {
        let runner = StubRunner {
            calls: Mutex::new(Vec::new()),
            result: Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        };
        let client = GhCliClient::new(runner).expect("init");
        let err = client.health_check().await.expect_err("expected error");
        assert!(err.to_string().contains("Install gh"));
    }
}
