use orchestrator_core::{
    rebuild_projection, CoreError, EventStore, GithubClient, ProjectionState,
    SelectedTicketFlowConfig, SelectedTicketFlowResult, SqliteEventStore, Supervisor,
    TicketSummary, VcsProvider, WorkerBackend,
};
use serde::{Deserialize, Serialize};
mod ticket_picker;
pub use ticket_picker::AppTicketPickerProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub workspace: String,
    #[serde(default = "default_event_store_path")]
    pub event_store_path: String,
}

fn default_event_store_path() -> String {
    "./orchestrator-events.db".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupState {
    pub status: String,
    pub projection: ProjectionState,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            workspace: "./".to_owned(),
            event_store_path: default_event_store_path(),
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Result<Self, CoreError> {
        let path = config_path_from_env()?;
        load_or_create_config(&path)
    }
}

fn config_path_from_env() -> Result<std::path::PathBuf, CoreError> {
    match std::env::var("ORCHESTRATOR_CONFIG") {
        Ok(raw) => Ok(raw.into()),
        Err(std::env::VarError::NotPresent) => default_config_path(),
        Err(_) => Err(CoreError::Configuration(
            "ORCHESTRATOR_CONFIG contained invalid UTF-8".to_owned(),
        )),
    }
}

fn default_config_path() -> Result<std::path::PathBuf, CoreError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| {
            CoreError::Configuration(
                "Unable to resolve home directory from HOME or USERPROFILE".to_owned(),
            )
        })?;
    if home.trim().is_empty() {
        return Err(CoreError::Configuration(
            "HOME and USERPROFILE values are empty".to_owned(),
        ));
    }

    Ok(std::path::PathBuf::from(home)
        .join(".config")
        .join("orchestrator")
        .join("config.toml"))
}

fn load_or_create_config(path: &std::path::Path) -> Result<AppConfig, CoreError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|err| {
                        CoreError::Configuration(format!(
                            "Failed to create parent directory {} for ORCHESTRATOR_CONFIG: {err}",
                            parent.display()
                        ))
                    })?;
                }
            }

            let rendered = toml::to_string_pretty(&AppConfig::default()).map_err(|err| {
                CoreError::Configuration(format!(
                    "Failed to serialize default ORCHESTRATOR_CONFIG: {err}"
                ))
            })?;

            std::fs::write(path, rendered.as_bytes()).map_err(|err| {
                CoreError::Configuration(format!(
                    "Failed to write default ORCHESTRATOR_CONFIG to {}: {err}",
                    path.display()
                ))
            })?;

            rendered
        }
        Err(err) => {
            return Err(CoreError::Configuration(format!(
                "Failed to read ORCHESTRATOR_CONFIG from {}: {err}",
                path.display()
            )));
        }
    };

    toml::from_str(&raw).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to parse ORCHESTRATOR_CONFIG from {}: {err}",
            path.display()
        ))
    })
}

pub struct App<S: Supervisor, G: GithubClient> {
    pub config: AppConfig,
    pub supervisor: S,
    pub github: G,
}

impl<S: Supervisor, G: GithubClient> App<S, G> {
    pub async fn startup_state(&self) -> Result<StartupState, CoreError> {
        self.supervisor.health_check().await?;
        self.github.health_check().await?;

        let store = SqliteEventStore::open(&self.config.event_store_path)?;
        let events = store.read_ordered()?;
        let projection = rebuild_projection(&events);

        Ok(StartupState {
            status: format!("ready ({})", self.config.workspace),
            projection,
        })
    }

    pub async fn start_or_resume_selected_ticket(
        &self,
        selected_ticket: &TicketSummary,
        vcs: &dyn VcsProvider,
        worker_backend: &dyn WorkerBackend,
    ) -> Result<SelectedTicketFlowResult, CoreError> {
        let mut store = SqliteEventStore::open(&self.config.event_store_path)?;
        let flow_config = SelectedTicketFlowConfig::from_workspace_root(&self.config.workspace);

        orchestrator_core::start_or_resume_selected_ticket(
            &mut store,
            selected_ticket,
            &flow_config,
            vcs,
            worker_backend,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::test_support::{with_env_var, TestDbPath};
    use orchestrator_core::{
        BackendCapabilities, BackendKind, RuntimeResult, SessionHandle, SpawnSpec,
        TerminalSnapshot, TicketId, TicketProvider, TicketSummary, WorktreeStatus,
    };
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("orchestrator-app-config-{prefix}-{now}-{}", std::process::id()))
    }

    fn write_config_file(path: &Path, raw: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture config parent");
        }
        std::fs::write(path, raw.as_bytes()).expect("write fixture config");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = unique_temp_path(prefix);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn remove_temp_path(path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    struct Healthy;

    #[async_trait::async_trait]
    impl Supervisor for Healthy {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl GithubClient for Healthy {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[test]
    fn config_defaults_when_env_missing() {
        let home = unique_temp_dir("home");
        let expected = home.join(".config").join("orchestrator").join("config.toml");

        with_env_var("HOME", Some(home.to_str().unwrap()), || {
            with_env_var("USERPROFILE", None, || {
                with_env_var("ORCHESTRATOR_CONFIG", None, || {
                    let config = AppConfig::from_env().expect("default config");
                    assert_eq!(config, AppConfig::default());
                    assert_eq!(expected, default_config_path().expect("default config path"));
                    assert!(expected.exists());
                    let raw = std::fs::read_to_string(expected.clone()).unwrap();
                    let parsed: AppConfig = toml::from_str(&raw).unwrap();
                    assert_eq!(parsed, AppConfig::default());
                });
            });
        });

        remove_temp_path(&home);
    }

    #[test]
    fn config_creates_default_when_missing() {
        let home = unique_temp_dir("create");
        let expected = home.join(".config").join("orchestrator").join("config.toml");

        with_env_var("HOME", Some(home.to_str().unwrap()), || {
            with_env_var("USERPROFILE", None, || {
                with_env_var("ORCHESTRATOR_CONFIG", Some(expected.to_str().unwrap()), || {
                    let config = AppConfig::from_env().expect("bootstrap config");
                    assert_eq!(config, AppConfig::default());
                    assert!(expected.exists());
                    let contents = std::fs::read_to_string(expected.clone()).unwrap();
                    let parsed: AppConfig = toml::from_str(&contents).unwrap();
                    assert_eq!(parsed, AppConfig::default());
                });
            });
        });

        remove_temp_path(&home);
    }

    #[test]
    fn config_parses_from_toml_file() {
        let home = unique_temp_dir("parse");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = '/tmp/events.db'\n",
        );

        with_env_var("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap()), || {
            let config = AppConfig::from_env().expect("parse config");
            assert_eq!(config.workspace, "/tmp/work");
            assert_eq!(config.event_store_path, "/tmp/events.db");
        });

        remove_temp_path(&home);
    }

    #[test]
    fn config_defaults_event_store_path_when_missing_in_toml_file() {
        let home = unique_temp_dir("partial");
        let config_path = home.join("config.toml");
        write_config_file(&config_path, "workspace = '/tmp/work'\n");

        with_env_var("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap()), || {
            let config = AppConfig::from_env().expect("parse config");
            assert_eq!(config.workspace, "/tmp/work");
            assert_eq!(config.event_store_path, default_event_store_path());
        });

        remove_temp_path(&home);
    }

    #[test]
    fn config_rejects_invalid_toml_file() {
        let home = unique_temp_dir("invalid");
        let config_path = home.join("config.toml");
        write_config_file(&config_path, "workspace = '/tmp/work'\nevent_store_path = [\n");

        with_env_var("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap()), || {
            let err = AppConfig::from_env().expect_err("invalid toml should fail");
            let message = err.to_string();
            assert!(message.contains("Failed to parse ORCHESTRATOR_CONFIG"));
        });

        remove_temp_path(&home);
    }


    #[tokio::test]
    async fn startup_composition_succeeds_with_mock_adapters() {
        let temp_db = TestDbPath::new("app-startup-test");

        let app = App {
            config: AppConfig {
                workspace: "./".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: Healthy,
            github: Healthy,
        };

        let state = app.startup_state().await.expect("startup state");
        assert!(state.status.contains("ready"));
        assert!(state.projection.events.is_empty());
    }

    struct StubVcs {
        repository: orchestrator_core::RepositoryRef,
        create_calls: Mutex<Vec<orchestrator_core::CreateWorktreeRequest>>,
    }

    #[async_trait::async_trait]
    impl VcsProvider for StubVcs {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn discover_repositories(
            &self,
            _roots: &[PathBuf],
        ) -> Result<Vec<orchestrator_core::RepositoryRef>, CoreError> {
            Ok(vec![self.repository.clone()])
        }

        async fn create_worktree(
            &self,
            request: orchestrator_core::CreateWorktreeRequest,
        ) -> Result<orchestrator_core::WorktreeSummary, CoreError> {
            self.create_calls
                .lock()
                .expect("lock")
                .push(request.clone());
            Ok(orchestrator_core::WorktreeSummary {
                worktree_id: request.worktree_id,
                repository: request.repository,
                path: request.worktree_path,
                branch: request.branch,
                base_branch: request.base_branch,
            })
        }

        async fn delete_worktree(
            &self,
            _request: orchestrator_core::DeleteWorktreeRequest,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn worktree_status(
            &self,
            _worktree_path: &Path,
        ) -> Result<WorktreeStatus, CoreError> {
            Ok(WorktreeStatus {
                is_dirty: false,
                commits_ahead: 0,
                commits_behind: 0,
            })
        }
    }

    struct EmptyStream;

    #[async_trait::async_trait]
    impl orchestrator_core::WorkerEventSubscription for EmptyStream {
        async fn next_event(&mut self) -> RuntimeResult<Option<orchestrator_core::BackendEvent>> {
            Ok(None)
        }
    }

    struct StubBackend {
        spawn_calls: Mutex<Vec<SpawnSpec>>,
    }

    #[async_trait::async_trait]
    impl orchestrator_core::SessionLifecycle for StubBackend {
        async fn spawn(&self, spec: SpawnSpec) -> RuntimeResult<SessionHandle> {
            self.spawn_calls.lock().expect("lock").push(spec.clone());
            Ok(SessionHandle {
                session_id: spec.session_id,
                backend: BackendKind::OpenCode,
            })
        }

        async fn kill(&self, _session: &SessionHandle) -> RuntimeResult<()> {
            Ok(())
        }

        async fn send_input(&self, _session: &SessionHandle, _input: &[u8]) -> RuntimeResult<()> {
            Ok(())
        }

        async fn resize(
            &self,
            _session: &SessionHandle,
            _cols: u16,
            _rows: u16,
        ) -> RuntimeResult<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl WorkerBackend for StubBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::OpenCode
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        async fn health_check(&self) -> RuntimeResult<()> {
            Ok(())
        }

        async fn subscribe(
            &self,
            _session: &SessionHandle,
        ) -> RuntimeResult<orchestrator_core::WorkerEventStream> {
            Ok(Box::new(EmptyStream))
        }

        async fn snapshot(&self, _session: &SessionHandle) -> RuntimeResult<TerminalSnapshot> {
            Ok(TerminalSnapshot {
                cols: 80,
                rows: 24,
                cursor_col: 0,
                cursor_row: 0,
                lines: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn app_starts_selected_ticket_and_persists_runtime_mapping() {
        let temp_db = TestDbPath::new("app-ticket-selected-start");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: Healthy,
            github: Healthy,
        };
        let vcs = StubVcs {
            repository: orchestrator_core::RepositoryRef {
                id: "/workspace/repo".to_owned(),
                name: "repo".to_owned(),
                root: PathBuf::from("/workspace/repo"),
            },
            create_calls: Mutex::new(Vec::new()),
        };
        let backend = StubBackend {
            spawn_calls: Mutex::new(Vec::new()),
        };
        let ticket = TicketSummary {
            ticket_id: TicketId::from_provider_uuid(TicketProvider::Linear, "issue-126"),
            identifier: "AP-126".to_owned(),
            title: "Implement ticket selected start resume orchestration flow".to_owned(),
            project: None,
            state: "In Progress".to_owned(),
            url: "https://linear.app/acme/issue/AP-126".to_owned(),
            priority: Some(2),
            labels: vec!["orchestrator".to_owned()],
            updated_at: "2026-02-16T11:00:00Z".to_owned(),
        };

        let result = app
            .start_or_resume_selected_ticket(&ticket, &vcs, &backend)
            .await
            .expect("start selected ticket");

        assert_eq!(
            result.action,
            orchestrator_core::SelectedTicketFlowAction::Started
        );
        assert_eq!(vcs.create_calls.lock().expect("lock").len(), 1);
        assert_eq!(backend.spawn_calls.lock().expect("lock").len(), 1);
    }
}
