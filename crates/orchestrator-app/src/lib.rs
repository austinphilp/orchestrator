use orchestrator_core::{
    rebuild_projection, CoreError, EventStore, GithubClient, LlmProvider, ProjectionState,
    SelectedTicketFlowConfig, SelectedTicketFlowResult, SqliteEventStore, Supervisor,
    TicketSummary, UntypedCommandInvocation, VcsProvider, WorkerBackend,
};
use orchestrator_ui::{SupervisorCommandContext, SupervisorCommandDispatcher};
use serde::{Deserialize, Serialize};
mod command_dispatch;
mod ticket_picker;
pub use ticket_picker::AppTicketPickerProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub workspace: String,
    #[serde(default = "default_event_store_path")]
    pub event_store_path: String,
}

const LEGACY_DEFAULT_WORKSPACE_PATH: &str = "./";
const LEGACY_DEFAULT_EVENT_STORE_PATH: &str = "./orchestrator-events.db";

fn default_workspace_path() -> String {
    default_orchestrator_data_dir()
        .join("workspace")
        .to_string_lossy()
        .to_string()
}

fn default_event_store_path() -> String {
    default_orchestrator_data_dir()
        .join("orchestrator-events.db")
        .to_string_lossy()
        .to_string()
}

const DEFAULT_SUPERVISOR_MODEL: &str = "openai/gpt-4o-mini";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupState {
    pub status: String,
    pub projection: ProjectionState,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            workspace: default_workspace_path(),
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

fn default_orchestrator_data_dir() -> std::path::PathBuf {
    resolve_data_local_dir().join("orchestrator")
}

fn resolve_data_local_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(path) = std::env::var("LOCALAPPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(std::path::PathBuf::from(path));
            }
        }
        if let Ok(path) = std::env::var("APPDATA") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(std::path::PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join("AppData").join("Local");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = resolve_home_dir() {
            return home.join("Library").join("Application Support");
        }
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Ok(path) = std::env::var("XDG_DATA_HOME") {
            let path = path.trim();
            if !path.is_empty() {
                return absolutize_path(std::path::PathBuf::from(path));
            }
        }
        if let Some(home) = resolve_home_dir() {
            return home.join(".local").join("share");
        }
    }

    std::env::temp_dir()
}

fn resolve_home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
        })
}

fn absolutize_path(path: std::path::PathBuf) -> std::path::PathBuf {
    if path.is_absolute() {
        return path;
    }

    if let Ok(current) = std::env::current_dir() {
        return current.join(path);
    }

    std::env::temp_dir().join(path)
}

fn persist_config(path: &std::path::Path, config: &AppConfig) -> Result<(), CoreError> {
    let rendered = toml::to_string_pretty(config).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to serialize ORCHESTRATOR_CONFIG for {}: {err}",
            path.display()
        ))
    })?;

    std::fs::write(path, rendered.as_bytes()).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to write ORCHESTRATOR_CONFIG to {}: {err}",
            path.display()
        ))
    })
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

            let default_config = AppConfig::default();
            persist_config(path, &default_config)?;

            toml::to_string_pretty(&default_config).map_err(|err| {
                CoreError::Configuration(format!(
                    "Failed to serialize default ORCHESTRATOR_CONFIG: {err}"
                ))
            })?
        }
        Err(err) => {
            return Err(CoreError::Configuration(format!(
                "Failed to read ORCHESTRATOR_CONFIG from {}: {err}",
                path.display()
            )));
        }
    };

    let mut config: AppConfig = toml::from_str(&raw).map_err(|err| {
        CoreError::Configuration(format!(
            "Failed to parse ORCHESTRATOR_CONFIG from {}: {err}",
            path.display()
        ))
    })?;

    let mut changed = false;
    if config.workspace.trim() == LEGACY_DEFAULT_WORKSPACE_PATH {
        config.workspace = default_workspace_path();
        changed = true;
    }
    if config.event_store_path.trim() == LEGACY_DEFAULT_EVENT_STORE_PATH {
        config.event_store_path = default_event_store_path();
        changed = true;
    }

    if changed {
        persist_config(path, &config)?;
    }

    Ok(config)
}

fn ensure_event_store_parent_dir(path: &str) -> Result<(), CoreError> {
    let parent = std::path::PathBuf::from(path)
        .parent()
        .map(std::path::Path::to_path_buf);

    if let Some(parent) = parent {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(&parent).map_err(|err| {
                CoreError::Configuration(format!(
                    "Failed to create parent directory {} for event store: {err}",
                    parent.display()
                ))
            })?;
        }
    }

    Ok(())
}

fn open_event_store(path: &str) -> Result<SqliteEventStore, CoreError> {
    ensure_event_store_parent_dir(path)?;
    SqliteEventStore::open(path)
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

        let store = open_event_store(&self.config.event_store_path)?;
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
        let mut store = open_event_store(&self.config.event_store_path)?;
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

fn supervisor_model_from_env() -> String {
    std::env::var("ORCHESTRATOR_SUPERVISOR_MODEL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SUPERVISOR_MODEL.to_owned())
}

#[async_trait::async_trait]
impl<S, G> SupervisorCommandDispatcher for App<S, G>
where
    S: Supervisor + LlmProvider + Send + Sync,
    G: GithubClient + Send + Sync,
{
    async fn dispatch_supervisor_command(
        &self,
        invocation: UntypedCommandInvocation,
        context: SupervisorCommandContext,
    ) -> Result<(String, orchestrator_core::LlmResponseStream), CoreError> {
        command_dispatch::dispatch_supervisor_runtime_command(
            &self.supervisor,
            &self.config.event_store_path,
            invocation,
            context,
        )
        .await
    }

    async fn cancel_supervisor_command(&self, stream_id: &str) -> Result<(), CoreError> {
        command_dispatch::record_user_initiated_supervisor_cancel(
            &self.config.event_store_path,
            stream_id,
        );
        self.supervisor.cancel_stream(stream_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::test_support::{with_env_var, with_env_vars, TestDbPath};
    use orchestrator_core::{
        BackendCapabilities, BackendKind, Command, CommandRegistry, LlmChatRequest,
        LlmFinishReason, LlmProviderKind, LlmResponseStream, LlmResponseSubscription,
        LlmStreamChunk, OrchestrationEventPayload, RuntimeResult, SessionHandle, SpawnSpec,
        SupervisorQueryArgs, SupervisorQueryCancellationSource, SupervisorQueryContextArgs,
        TerminalSnapshot, TicketId, TicketProvider, TicketSummary, WorktreeStatus,
    };
    use std::collections::{BTreeMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "orchestrator-app-config-{prefix}-{now}-{}",
            std::process::id()
        ))
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

    fn expected_default_data_dir(home: &Path) -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            return home.join("AppData").join("Local").join("orchestrator");
        }

        #[cfg(target_os = "macos")]
        {
            return home
                .join("Library")
                .join("Application Support")
                .join("orchestrator");
        }

        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            home.join(".local").join("share").join("orchestrator")
        }
    }

    fn expected_default_workspace(home: &Path) -> PathBuf {
        expected_default_data_dir(home).join("workspace")
    }

    fn expected_default_event_store(home: &Path) -> PathBuf {
        expected_default_data_dir(home).join("orchestrator-events.db")
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

    #[derive(Clone, Default)]
    struct QueryingSupervisor {
        requests: Arc<Mutex<Vec<LlmChatRequest>>>,
        cancelled_streams: Arc<Mutex<Vec<String>>>,
        stream_chunks: Arc<Vec<LlmStreamChunk>>,
    }

    impl QueryingSupervisor {
        fn with_chunks(stream_chunks: Vec<LlmStreamChunk>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                cancelled_streams: Arc::new(Mutex::new(Vec::new())),
                stream_chunks: Arc::new(stream_chunks),
            }
        }

        fn requests(&self) -> Vec<LlmChatRequest> {
            self.requests.lock().expect("request lock").clone()
        }

        fn cancelled_streams(&self) -> Vec<String> {
            self.cancelled_streams.lock().expect("cancel lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl Supervisor for QueryingSupervisor {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct TestLlmStream {
        chunks: VecDeque<LlmStreamChunk>,
    }

    #[async_trait::async_trait]
    impl LlmResponseSubscription for TestLlmStream {
        async fn next_chunk(&mut self) -> Result<Option<LlmStreamChunk>, CoreError> {
            Ok(self.chunks.pop_front())
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for QueryingSupervisor {
        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::OpenRouter
        }

        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn stream_chat(
            &self,
            request: LlmChatRequest,
        ) -> Result<(String, LlmResponseStream), CoreError> {
            self.requests.lock().expect("request lock").push(request);
            Ok((
                "test-stream-1".to_owned(),
                Box::new(TestLlmStream {
                    chunks: self.stream_chunks.iter().cloned().collect(),
                }),
            ))
        }

        async fn cancel_stream(&self, stream_id: &str) -> Result<(), CoreError> {
            self.cancelled_streams
                .lock()
                .expect("cancel lock")
                .push(stream_id.to_owned());
            Ok(())
        }
    }

    fn template_query_invocation(template: &str) -> UntypedCommandInvocation {
        CommandRegistry::default()
            .to_untyped_invocation(&Command::SupervisorQuery(SupervisorQueryArgs::Template {
                template: template.to_owned(),
                variables: BTreeMap::new(),
                context: None,
            }))
            .expect("serialize supervisor.query")
    }

    fn template_query_invocation_with_context(
        template: &str,
        context: SupervisorQueryContextArgs,
    ) -> UntypedCommandInvocation {
        CommandRegistry::default()
            .to_untyped_invocation(&Command::SupervisorQuery(SupervisorQueryArgs::Template {
                template: template.to_owned(),
                variables: BTreeMap::new(),
                context: Some(context),
            }))
            .expect("serialize supervisor.query with context")
    }

    fn freeform_query_invocation_with_context(
        query: &str,
        context: Option<SupervisorQueryContextArgs>,
    ) -> UntypedCommandInvocation {
        CommandRegistry::default()
            .to_untyped_invocation(&Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query: query.to_owned(),
                context,
            }))
            .expect("serialize freeform supervisor.query")
    }

    fn read_events(path: &std::path::Path) -> Vec<orchestrator_core::StoredEventEnvelope> {
        let store = SqliteEventStore::open(path).expect("open event store");
        store.read_ordered().expect("read ordered events")
    }

    #[test]
    fn config_defaults_when_env_missing() {
        let home = unique_temp_dir("home");
        let expected = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");
        let expected_workspace = expected_default_workspace(&home);
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", None),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("default config");
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );
                assert!(Path::new(&config.workspace).is_absolute());
                assert!(Path::new(&config.event_store_path).is_absolute());
                assert_eq!(
                    expected,
                    default_config_path().expect("default config path")
                );
                assert!(expected.exists());
                let raw = std::fs::read_to_string(expected.clone()).unwrap();
                let parsed: AppConfig = toml::from_str(&raw).unwrap();
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    parsed.event_store_path,
                    expected_event_store.to_string_lossy()
                );
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_creates_default_when_missing() {
        let home = unique_temp_dir("create");
        let expected = home
            .join(".config")
            .join("orchestrator")
            .join("config.toml");
        let expected_workspace = expected_default_workspace(&home);
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(expected.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("bootstrap config");
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );
                assert!(expected.exists());
                let contents = std::fs::read_to_string(expected.clone()).unwrap();
                let parsed: AppConfig = toml::from_str(&contents).unwrap();
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    parsed.event_store_path,
                    expected_event_store.to_string_lossy()
                );
            },
        );

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

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let config = AppConfig::from_env().expect("parse config");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(config.event_store_path, "/tmp/events.db");
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_defaults_event_store_path_when_missing_in_toml_file() {
        let home = unique_temp_dir("partial");
        let config_path = home.join("config.toml");
        write_config_file(&config_path, "workspace = '/tmp/work'\n");
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("parse config");
                assert_eq!(config.workspace, "/tmp/work");
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_upgrades_legacy_relative_defaults() {
        let home = unique_temp_dir("legacy-upgrade");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = './'\nevent_store_path = './orchestrator-events.db'\n",
        );
        let expected_workspace = expected_default_workspace(&home);
        let expected_event_store = expected_default_event_store(&home);

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("config should load");
                assert_eq!(config.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    config.event_store_path,
                    expected_event_store.to_string_lossy()
                );

                let rewritten = std::fs::read_to_string(&config_path).expect("read rewritten");
                let parsed: AppConfig = toml::from_str(&rewritten).expect("parse rewritten");
                assert_eq!(parsed.workspace, expected_workspace.to_string_lossy());
                assert_eq!(
                    parsed.event_store_path,
                    expected_event_store.to_string_lossy()
                );
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_preserves_non_legacy_relative_paths() {
        let home = unique_temp_dir("relative-preserved");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = './custom-workspace'\nevent_store_path = './custom-events.db'\n",
        );

        with_env_vars(
            &[
                ("HOME", Some(home.to_str().unwrap())),
                ("USERPROFILE", None),
                ("ORCHESTRATOR_CONFIG", Some(config_path.to_str().unwrap())),
                ("XDG_DATA_HOME", None),
                ("LOCALAPPDATA", None),
                ("APPDATA", None),
            ],
            || {
                let config = AppConfig::from_env().expect("config should load");
                assert_eq!(config.workspace, "./custom-workspace");
                assert_eq!(config.event_store_path, "./custom-events.db");
            },
        );

        remove_temp_path(&home);
    }

    #[test]
    fn config_rejects_invalid_toml_file() {
        let home = unique_temp_dir("invalid");
        let config_path = home.join("config.toml");
        write_config_file(
            &config_path,
            "workspace = '/tmp/work'\nevent_store_path = [\n",
        );

        with_env_var(
            "ORCHESTRATOR_CONFIG",
            Some(config_path.to_str().unwrap()),
            || {
                let err = AppConfig::from_env().expect_err("invalid toml should fail");
                let message = err.to_string();
                assert!(message.contains("Failed to parse ORCHESTRATOR_CONFIG"));
            },
        );

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
            assignee: None,
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

    #[tokio::test]
    async fn supervisor_query_dispatch_streams_from_runtime_handler() {
        let temp_db = TestDbPath::new("app-supervisor-query-dispatch");
        let supervisor = QueryingSupervisor::with_chunks(vec![
            LlmStreamChunk {
                delta: "Current activity: worker is implementing AP-180".to_owned(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            },
            LlmStreamChunk {
                delta: String::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            },
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status_current_session"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        assert_eq!(stream_id, "test-stream-1");

        let first = stream
            .next_chunk()
            .await
            .expect("poll first chunk")
            .expect("first chunk");
        assert!(first.delta.contains("Current activity"));
        assert_eq!(first.finish_reason, None);

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Template: Current activity"));
        assert!(requests[0].messages[1]
            .content
            .contains("Scope: session:sess-query"));
        assert!(requests[0].messages[1].content.contains("Context pack:"));
        assert!(requests[0].messages[1]
            .content
            .contains("Ticket status context:"));
        assert!(requests[0].messages[1]
            .content
            .contains("Ticket status fallback:"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_persists_lifecycle_events() {
        let temp_db = TestDbPath::new("app-supervisor-query-lifecycle");
        let supervisor = QueryingSupervisor::with_chunks(vec![
            LlmStreamChunk {
                delta: "Current activity: reviewing build output.".to_owned(),
                finish_reason: None,
                usage: None,
                rate_limit: None,
            },
            LlmStreamChunk {
                delta: String::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: Some(orchestrator_core::LlmTokenUsage {
                    input_tokens: 64,
                    output_tokens: 19,
                    total_tokens: 83,
                }),
                rate_limit: None,
            },
        ]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status_current_session"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        while stream
            .next_chunk()
            .await
            .expect("poll lifecycle stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let started_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    OrchestrationEventPayload::SupervisorQueryStarted(_)
                )
            })
            .count();
        let chunk_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    OrchestrationEventPayload::SupervisorQueryChunk(_)
                )
            })
            .count();
        let finished = events.iter().find_map(|event| match &event.payload {
            OrchestrationEventPayload::SupervisorQueryFinished(payload) => Some(payload.clone()),
            _ => None,
        });

        assert_eq!(started_count, 1);
        assert_eq!(chunk_count, 2);
        let finished = finished.expect("finished lifecycle event");
        assert_eq!(finished.finish_reason, LlmFinishReason::Stop);
        assert_eq!(finished.chunk_count, 2);
        assert_eq!(finished.usage.map(|usage| usage.total_tokens), Some(83));
        assert!(
            events.iter().any(|event| {
                event
                    .work_item_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "wi-query")
                    && matches!(
                        event.payload,
                        OrchestrationEventPayload::SupervisorQueryStarted(_)
                    )
            }),
            "lifecycle events should carry work item identity for projection views"
        );
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_uses_invocation_context_when_present() {
        let temp_db = TestDbPath::new("app-supervisor-query-command-context");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                template_query_invocation_with_context(
                    "status",
                    SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: Some("sess-from-command".to_owned()),
                        scope: Some("session:sess-from-command".to_owned()),
                    },
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-fallback".to_owned()),
                    selected_session_id: None,
                    scope: Some("work_item:wi-fallback".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0]
            .messages
            .iter()
            .any(|message| message.content.contains("Scope: session:sess-from-command")));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_partial_invocation_context_overlays_fallback() {
        let temp_db = TestDbPath::new("app-supervisor-query-partial-context-overlay");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What should I do next?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: None,
                        selected_session_id: Some("sess-from-command".to_owned()),
                        scope: None,
                    }),
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-fallback".to_owned()),
                    selected_session_id: Some("sess-fallback".to_owned()),
                    scope: Some("session:sess-fallback".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Scope: session:sess-from-command"));
        assert!(requests[0].messages[1]
            .content
            .contains("- selected_work_item_id: wi-fallback"));
        assert!(requests[0].messages[1]
            .content
            .contains("No local ticket metadata is mapped for work item 'wi-fallback'"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_auto_resolves_blocking_intent_to_template() {
        let temp_db = TestDbPath::new("app-supervisor-query-freeform-blocking-template");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What is blocking this ticket?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: None,
                        scope: None,
                    }),
                ),
                SupervisorCommandContext::default(),
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Template: What is blocking (what_is_blocking)"));
        assert!(requests[0].messages[1]
            .content
            .contains("Template variables:"));
        assert!(requests[0].messages[1]
            .content
            .contains("- operator_question=What is blocking this ticket?"));
        assert!(requests[0].messages[1].content.contains("Context pack:"));
        assert!(!requests[0].messages[1]
            .content
            .contains("Operator question:"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_auto_resolves_status_and_planning_intents_to_templates() {
        let temp_db = TestDbPath::new("app-supervisor-query-freeform-intent-templates");

        let cases = [
            (
                "What's the status right now?",
                "Template: Ticket status (ticket_status)",
            ),
            (
                "What should we do next to unblock this?",
                "Template: Next actions (next_actions)",
            ),
        ];

        for (query, expected_template) in cases {
            let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
                delta: String::new(),
                finish_reason: Some(LlmFinishReason::Stop),
                usage: None,
                rate_limit: None,
            }]);

            let app = App {
                config: AppConfig {
                    workspace: "/workspace".to_owned(),
                    event_store_path: temp_db.path().to_string_lossy().to_string(),
                },
                supervisor: supervisor.clone(),
                github: Healthy,
            };

            let _ = app
                .dispatch_supervisor_command(
                    freeform_query_invocation_with_context(
                        query,
                        Some(SupervisorQueryContextArgs {
                            selected_work_item_id: Some("wi-from-command".to_owned()),
                            selected_session_id: None,
                            scope: None,
                        }),
                    ),
                    SupervisorCommandContext::default(),
                )
                .await
                .expect("dispatch should stream");

            let requests = supervisor.requests();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].messages[1].content.contains(expected_template));
            assert!(requests[0].messages[1]
                .content
                .contains("- operator_question="));
            assert!(!requests[0].messages[1]
                .content
                .contains("Operator question:"));
        }
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_preserves_freeform_for_unmatched_questions() {
        let temp_db = TestDbPath::new("app-supervisor-query-freeform-fallback");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);

        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "Compare AP-101 and AP-102 delivery risk by owner.",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: None,
                        scope: None,
                    }),
                ),
                SupervisorCommandContext::default(),
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Operator question:\nCompare AP-101 and AP-102 delivery risk by owner."));
        assert!(!requests[0].messages[1].content.contains("Template:"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_rejects_unknown_template_with_guidance() {
        let temp_db = TestDbPath::new("app-supervisor-query-template-error");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };

        let err = match app
            .dispatch_supervisor_command(
                template_query_invocation("not-a-template"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: None,
                    scope: Some("work_item:wi-query".to_owned()),
                },
            )
            .await
        {
            Ok(_) => panic!("unknown template should fail"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(message.contains("unknown supervisor template"));
        assert!(message.contains("current_activity"));
        assert!(message.contains("recommended_response"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_rejects_malformed_context_scope() {
        let temp_db = TestDbPath::new("app-supervisor-query-context-error");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };

        let err = match app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: None,
                    scope: Some("session:  ".to_owned()),
                },
            )
            .await
        {
            Ok(_) => panic!("malformed scope should fail"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("malformed context"));
        assert!(err.to_string().contains("scope identifier"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_context_from_invocation_replaces_fallback_context() {
        let temp_db = TestDbPath::new("app-supervisor-query-command-context-precedence");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Stop),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let _ = app
            .dispatch_supervisor_command(
                freeform_query_invocation_with_context(
                    "What changed for AP-180?",
                    Some(SupervisorQueryContextArgs {
                        selected_work_item_id: Some("wi-from-command".to_owned()),
                        selected_session_id: None,
                        scope: None,
                    }),
                ),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-fallback".to_owned()),
                    selected_session_id: Some("sess-fallback".to_owned()),
                    scope: Some("session:sess-fallback".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        let requests = supervisor.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages[1]
            .content
            .contains("Operator question:\nWhat changed for AP-180?"));
        assert!(requests[0].messages[1]
            .content
            .contains("Ticket status context:"));
        assert!(requests[0].messages[1]
            .content
            .contains("Scope: work_item:wi-from-command"));
        assert!(
            !requests[0].messages[1]
                .content
                .contains("Scope: session:sess-fallback"),
            "fallback scope should not leak when invocation context is present"
        );
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_rejects_non_supervisor_commands() {
        let temp_db = TestDbPath::new("app-supervisor-query-unsupported-command");
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: QueryingSupervisor::default(),
            github: Healthy,
        };
        let invocation = CommandRegistry::default()
            .to_untyped_invocation(&Command::UiFocusNextInbox)
            .expect("serialize zero-arg command");

        let err = match app
            .dispatch_supervisor_command(invocation, SupervisorCommandContext::default())
            .await
        {
            Ok(_) => panic!("unsupported command should fail"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(message.contains("unsupported command"));
        assert!(message.contains("ui.focus_next_inbox"));
        assert!(message.contains("supervisor.query"));
    }

    #[tokio::test]
    async fn supervisor_query_dispatch_forwards_cancel_to_provider() {
        let temp_db = TestDbPath::new("app-supervisor-query-cancel");
        let supervisor = QueryingSupervisor::default();
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        app.cancel_supervisor_command("stream-cancel-7")
            .await
            .expect("cancel should succeed");

        assert_eq!(
            supervisor.cancelled_streams(),
            vec!["stream-cancel-7".to_owned()]
        );
    }

    #[tokio::test]
    async fn supervisor_query_cancel_records_user_initiated_cancellation() {
        let temp_db = TestDbPath::new("app-supervisor-query-user-cancel-record");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Cancelled),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        app.cancel_supervisor_command(stream_id.as_str())
            .await
            .expect("cancel should succeed");

        while stream
            .next_chunk()
            .await
            .expect("poll cancelled stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let saw_user_cancelled = events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::SupervisorQueryCancelled(payload)
                    if payload.source == SupervisorQueryCancellationSource::UserInitiated
            )
        });
        let saw_finished_user_cancelled = events.iter().any(|event| {
            matches!(
                &event.payload,
                OrchestrationEventPayload::SupervisorQueryFinished(payload)
                    if payload.finish_reason == LlmFinishReason::Cancelled
                        && payload.cancellation_source
                            == Some(SupervisorQueryCancellationSource::UserInitiated)
            )
        });

        assert!(saw_user_cancelled);
        assert!(saw_finished_user_cancelled);
    }

    #[tokio::test]
    async fn supervisor_query_cancel_records_single_user_cancel_event_for_retries() {
        let temp_db = TestDbPath::new("app-supervisor-query-user-cancel-deduped");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Cancelled),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (stream_id, mut stream) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-query".to_owned()),
                    selected_session_id: Some("sess-query".to_owned()),
                    scope: Some("session:sess-query".to_owned()),
                },
            )
            .await
            .expect("dispatch should stream");

        app.cancel_supervisor_command(stream_id.as_str())
            .await
            .expect("first cancel should succeed");
        app.cancel_supervisor_command(stream_id.as_str())
            .await
            .expect("second cancel should succeed");

        while stream
            .next_chunk()
            .await
            .expect("poll cancelled stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let user_cancelled_count = events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    OrchestrationEventPayload::SupervisorQueryCancelled(payload)
                        if payload.source == SupervisorQueryCancellationSource::UserInitiated
                )
            })
            .count();

        assert_eq!(user_cancelled_count, 1);
    }

    #[tokio::test]
    async fn supervisor_query_cancel_targets_latest_active_query_when_stream_ids_repeat() {
        let temp_db = TestDbPath::new("app-supervisor-query-stream-id-reuse");
        let supervisor = QueryingSupervisor::with_chunks(vec![LlmStreamChunk {
            delta: String::new(),
            finish_reason: Some(LlmFinishReason::Cancelled),
            usage: None,
            rate_limit: None,
        }]);
        let app = App {
            config: AppConfig {
                workspace: "/workspace".to_owned(),
                event_store_path: temp_db.path().to_string_lossy().to_string(),
            },
            supervisor: supervisor.clone(),
            github: Healthy,
        };

        let (_stream_id_first, mut stream_first) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-first".to_owned()),
                    selected_session_id: Some("sess-first".to_owned()),
                    scope: Some("session:sess-first".to_owned()),
                },
            )
            .await
            .expect("first dispatch should stream");

        let (stream_id_second, mut stream_second) = app
            .dispatch_supervisor_command(
                template_query_invocation("status"),
                SupervisorCommandContext {
                    selected_work_item_id: Some("wi-second".to_owned()),
                    selected_session_id: Some("sess-second".to_owned()),
                    scope: Some("session:sess-second".to_owned()),
                },
            )
            .await
            .expect("second dispatch should stream");

        while stream_first
            .next_chunk()
            .await
            .expect("poll first stream")
            .is_some()
        {}

        app.cancel_supervisor_command(stream_id_second.as_str())
            .await
            .expect("cancel should target second active stream");

        while stream_second
            .next_chunk()
            .await
            .expect("poll second stream")
            .is_some()
        {}

        let events = read_events(temp_db.path());
        let saw_second_user_cancel = events.iter().any(|event| {
            event
                .work_item_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "wi-second")
                && matches!(
                    &event.payload,
                    OrchestrationEventPayload::SupervisorQueryCancelled(payload)
                        if payload.source == SupervisorQueryCancellationSource::UserInitiated
                )
        });

        assert!(saw_second_user_cancel);
    }
}
