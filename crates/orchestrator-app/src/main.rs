use anyhow::Result;
use backend_codex::{CodexBackend, CodexBackendConfig};
use backend_opencode::{OpenCodeBackend, OpenCodeBackendConfig};
use integration_git::{GitCliVcsProvider, ProcessCommandRunner as GitProcessCommandRunner};
use integration_linear::{
    LinearConfig, LinearRuntimeSettings, LinearTicketingProvider, WorkflowStateMapSetting,
};
use integration_shortcut::{ShortcutConfig, ShortcutTicketingProvider};
use orchestrator_app::{App, AppConfig, AppTicketPickerProvider};
use orchestrator_core::{
    BackendKind, CoreError, EventPrunePolicy, OrchestrationEventPayload, SpawnSpec,
    SqliteEventStore, TicketProvider, TicketRecord, TicketingProvider, WorkItemId, WorkerBackend,
    WorkflowState,
};
use orchestrator_github::{GhCliClient, ProcessCommandRunner as GhProcessCommandRunner};
use orchestrator_supervisor::OpenRouterSupervisor;
use orchestrator_ui::Ui;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const ENV_HARNESS_SESSION_ID: &str = "ORCHESTRATOR_HARNESS_SESSION_ID";
const ENV_OPENROUTER_API_KEY: &str = "OPENROUTER_API_KEY";
const ENV_LINEAR_API_KEY: &str = "LINEAR_API_KEY";
const ENV_SHORTCUT_API_KEY: &str = "ORCHESTRATOR_SHORTCUT_API_KEY";
const STARTUP_RESUME_NUDGE: &str =
    "Sorry, we got interrupted, please continue from where we last left off.";

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = AppConfig::from_env()?;
    init_file_logging(config.event_store_path.as_str())?;
    let cli = parse_cli_flags()?;

    config.ticketing_provider = resolve_provider_name(
        cli.ticketing_provider.as_deref(),
        &config.ticketing_provider,
    )?;
    config.harness_provider =
        resolve_provider_name(cli.harness_provider.as_deref(), &config.harness_provider)?;

    orchestrator_app::set_supervisor_model_config(config.supervisor.model.clone());
    orchestrator_app::set_git_binary_config(config.git.binary.clone());
    orchestrator_ui::set_ui_runtime_config(
        config.ui.theme.clone(),
        config.ui.ticket_picker_priority_states.clone(),
        config.supervisor.model.clone(),
        config.ui.transcript_line_limit,
        config.ui.background_session_refresh_secs,
        config.ui.session_info_background_refresh_secs,
        config.ui.merge_poll_base_interval_secs,
        config.ui.merge_poll_max_backoff_secs,
        config.ui.merge_poll_backoff_multiplier,
        orchestrator_core::WorkflowInteractionProfilesConfig {
            default_profile: config.ui.default_workflow_profile.clone(),
            profiles: config.ui.workflow_interaction_profiles.clone(),
        },
    );

    let openrouter_api_key = required_env(ENV_OPENROUTER_API_KEY)?;
    let supervisor = OpenRouterSupervisor::with_base_url(
        openrouter_api_key,
        config.supervisor.openrouter_base_url.clone(),
    )?;
    let github = GhCliClient::new(
        GhProcessCommandRunner,
        PathBuf::from(config.github.binary.as_str()),
        config.runtime.allow_unsafe_command_paths,
    )?;
    let (ticketing, linear_ticketing) =
        build_ticketing_provider(&config, &config.ticketing_provider)?;
    let worker_backend = build_harness_provider(&config, &config.harness_provider)?;
    let vcs = Arc::new(GitCliVcsProvider::new(
        GitProcessCommandRunner,
        PathBuf::from(config.git.binary.as_str()),
        config.git.allow_destructive_automation,
        config.git.allow_force_push,
        config.git.allow_delete_unmerged_branches,
        config.runtime.allow_unsafe_command_paths,
    )?);

    ticketing.health_check().await?;
    worker_backend.health_check().await?;

    let app = Arc::new(App {
        config,
        ticketing: ticketing.clone(),
        supervisor,
        github,
    });
    if let Err(error) = run_event_prune_maintenance(&app.config) {
        tracing::warn!(error = %error, "event prune maintenance failed at startup");
    }
    let ticket_picker_provider = Arc::new(AppTicketPickerProvider::new(
        Arc::clone(&app),
        ticketing,
        vcs,
        worker_backend.clone(),
    ));

    let state = app.startup_state().await?;
    rehydrate_inflight_sessions(&app.config.event_store_path, worker_backend.as_ref()).await?;
    let supervisor_dispatcher: Arc<dyn orchestrator_ui::SupervisorCommandDispatcher> = app.clone();
    let mut ui = Ui::init()?
        .with_supervisor_command_dispatcher(supervisor_dispatcher)
        .with_ticket_picker_provider(ticket_picker_provider)
        .with_worker_backend(worker_backend.clone());
    app.start_linear_polling(linear_ticketing.as_deref())
        .await?;
    match ui.run(&state.status, &state.projection) {
        Ok(()) => {
            app.stop_linear_polling(linear_ticketing.as_deref()).await?;
        }
        Err(ui_error) => {
            if let Err(stop_error) = app.stop_linear_polling(linear_ticketing.as_deref()).await {
                tracing::warn!(error = %stop_error, "failed to stop linear polling during UI shutdown");
                return Err(anyhow::anyhow!(
                    "UI shutdown failed: {ui_error}; additionally, linear polling stop failed: {stop_error}"
                ));
            }
            return Err(ui_error.into());
        }
    }

    Ok(())
}

fn init_file_logging(event_store_path: &str) -> Result<(), CoreError> {
    let log_path = log_file_path(event_store_path);
    if let Some(parent) = log_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CoreError::Configuration(format!(
                    "failed to create orchestrator log directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }
    }

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| {
            CoreError::Configuration(format!(
                "failed to open orchestrator log file '{}': {error}",
                log_path.display()
            ))
        })?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log_file))
        .init();

    Ok(())
}

fn log_file_path(event_store_path: &str) -> PathBuf {
    let event_store = Path::new(event_store_path);
    event_store
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("orchestrator.log")
}

#[derive(Debug, Default)]
struct CliFlags {
    ticketing_provider: Option<String>,
    harness_provider: Option<String>,
}

fn parse_cli_flags() -> Result<CliFlags, CoreError> {
    let mut flags = CliFlags::default();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ticketing-provider" => {
                flags.ticketing_provider = Some(read_cli_value(
                    &arg,
                    args.next().ok_or_else(|| {
                        CoreError::Configuration(
                            "Missing value after --ticketing-provider. Use --ticketing-provider <linear|shortcut>."
                                .to_owned(),
                        )
                    })?,
                )?);
            }
            "--harness-provider" => {
                flags.harness_provider = Some(read_cli_value(
                    &arg,
                    args.next().ok_or_else(|| {
                        CoreError::Configuration(
                            "Missing value after --harness-provider. Use --harness-provider <opencode|codex>."
                                .to_owned(),
                        )
                    })?,
                )?);
            }
            "--help" | "-h" => {
                print_cli_help();
                std::process::exit(0);
            }
            value if value.starts_with("--") => {
                return Err(CoreError::Configuration(format!(
                    "Unknown flag '{value}'. Run with --help for valid flags."
                )));
            }
            unknown => {
                return Err(CoreError::Configuration(format!(
                    "Unexpected argument '{unknown}'. Run with --help for valid flags."
                )));
            }
        }
    }

    Ok(flags)
}

fn print_cli_help() {
    println!("Usage: orchestrator-app [--ticketing-provider <linear|shortcut>] [--harness-provider <opencode|codex>]");
    println!();
    println!(
        "  --ticketing-provider <provider>   Configure ticketing provider (linear or shortcut)"
    );
    println!("  --harness-provider <provider>     Configure harness/backend provider (opencode or codex)");
    println!("  --help                            Show this help message");
}

fn read_cli_value(flag: &str, value: String) -> Result<String, CoreError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Flag '{flag}' requires a non-empty value."
        )));
    }
    Ok(value)
}

fn resolve_provider_name(cli: Option<&str>, config_value: &str) -> Result<String, CoreError> {
    let raw = cli
        .map(|value| value.to_owned())
        .unwrap_or_else(|| config_value.to_owned());

    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        Ok(config_value.to_owned())
    } else {
        Ok(normalized)
    }
}

fn build_ticketing_provider(
    config: &AppConfig,
    provider: &str,
) -> Result<
    (
        Arc<dyn TicketingProvider + Send + Sync>,
        Option<Arc<LinearTicketingProvider>>,
    ),
    CoreError,
> {
    match provider {
        "linear" => {
            let api_key = required_env(ENV_LINEAR_API_KEY)?;
            let settings = LinearRuntimeSettings {
                api_url: config.linear.api_url.clone(),
                sync_interval_secs: config.linear.sync_interval_secs,
                fetch_limit: config.linear.fetch_limit,
                sync_assigned_to_me: config.linear.sync_assigned_to_me,
                sync_states: config.linear.sync_states.clone(),
                workflow_state_map: config
                    .linear
                    .workflow_state_map
                    .iter()
                    .map(|entry| WorkflowStateMapSetting {
                        workflow_state: entry.workflow_state.clone(),
                        linear_state: entry.linear_state.clone(),
                    })
                    .collect(),
                workflow_comment_summaries: config.linear.workflow_comment_summaries,
                workflow_attach_pr_links: config.linear.workflow_attach_pr_links,
            };
            let linear_config = LinearConfig::from_settings(api_key, settings)?;
            let provider = Arc::new(LinearTicketingProvider::new(linear_config)?);
            let linear_ticketing = Arc::clone(&provider);
            let ticketing: Arc<dyn TicketingProvider + Send + Sync> = provider;
            Ok((ticketing, Some(linear_ticketing)))
        }
        "shortcut" => {
            let api_key = required_env(ENV_SHORTCUT_API_KEY)?;
            let shortcut_config = ShortcutConfig::from_settings(
                api_key,
                config.shortcut.api_url.clone(),
                config.shortcut.fetch_limit,
            )?;
            Ok((
                Arc::new(ShortcutTicketingProvider::new(shortcut_config)?),
                None,
            ))
        }
        other => Err(CoreError::Configuration(format!(
            "Unknown ticketing provider '{other}'. Expected 'linear' or 'shortcut'."
        ))),
    }
}

fn build_harness_provider(
    config: &AppConfig,
    provider: &str,
) -> Result<Arc<dyn WorkerBackend + Send + Sync>, CoreError> {
    match provider {
        "opencode" => Ok(Arc::new(OpenCodeBackend::new(OpenCodeBackendConfig {
            binary: PathBuf::from(config.runtime.opencode_binary.as_str()),
            base_args: Vec::new(),
            output_buffer: 256,
            server_base_url: Some(config.runtime.opencode_server_base_url.clone()),
            server_startup_timeout: std::time::Duration::from_secs(
                config.runtime.harness_server_startup_timeout_secs,
            ),
            allow_unsafe_command_paths: config.runtime.allow_unsafe_command_paths,
            harness_log_raw_events: config.runtime.harness_log_raw_events,
            harness_log_normalized_events: config.runtime.harness_log_normalized_events,
        }))),
        "codex" => Ok(Arc::new(CodexBackend::new(CodexBackendConfig {
            binary: PathBuf::from(config.runtime.codex_binary.as_str()),
            base_args: Vec::new(),
            server_startup_timeout: std::time::Duration::from_secs(
                config.runtime.harness_server_startup_timeout_secs,
            ),
            legacy_server_base_url: None,
            harness_log_raw_events: config.runtime.harness_log_raw_events,
            harness_log_normalized_events: config.runtime.harness_log_normalized_events,
        }))),
        other => Err(CoreError::Configuration(format!(
            "Unknown harness provider '{other}'. Expected 'opencode' or 'codex'."
        ))),
    }
}

fn required_env(name: &str) -> Result<String, CoreError> {
    let value = std::env::var(name).map_err(|_| {
        CoreError::Configuration(format!(
            "{name} is not set. Export a valid value before starting orchestrator-app."
        ))
    })?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(CoreError::Configuration(format!(
            "{name} is empty. Provide a non-empty value."
        )));
    }
    Ok(value)
}

fn run_event_prune_maintenance(config: &AppConfig) -> Result<(), CoreError> {
    if !config.runtime.event_prune_enabled {
        return Ok(());
    }

    let mut store = SqliteEventStore::open(config.event_store_path.as_str())?;
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let report = store.prune_completed_session_events(
        EventPrunePolicy {
            retention_days: config.runtime.event_retention_days,
        },
        now_unix_seconds,
    )?;

    tracing::info!(
        retention_days = config.runtime.event_retention_days,
        cutoff_unix_seconds = report.cutoff_unix_seconds,
        candidate_sessions = report.candidate_sessions,
        eligible_sessions = report.eligible_sessions,
        pruned_work_items = report.pruned_work_items,
        deleted_events = report.deleted_events,
        deleted_event_artifact_refs = report.deleted_event_artifact_refs,
        skipped_invalid_timestamps = report.skipped_invalid_timestamps,
        "event prune maintenance completed"
    );

    Ok(())
}

async fn rehydrate_inflight_sessions(
    event_store_path: &str,
    worker_backend: &dyn WorkerBackend,
) -> Result<(), CoreError> {
    let store = SqliteEventStore::open(event_store_path)?;
    let mappings = store.list_inflight_runtime_mappings()?;
    let active_backend_kind = worker_backend.kind();

    for mapping in mappings {
        if mapping.session.backend_kind != active_backend_kind {
            continue;
        }
        let workflow_state = latest_workflow_state_for_work_item(&store, &mapping.work_item_id)?;
        let was_working = store.is_session_working(&mapping.session.session_id)?;

        let mut environment = Vec::new();
        if mapping.session.backend_kind == BackendKind::Codex {
            if let Some(harness_session_id) = store.find_harness_session_binding(
                &mapping.session.session_id,
                &mapping.session.backend_kind,
            )? {
                environment.push((ENV_HARNESS_SESSION_ID.to_owned(), harness_session_id));
            }
        }

        let instruction = resume_instruction_from_ticket(&mapping.ticket, &workflow_state);
        let spawn_result = worker_backend
            .spawn(SpawnSpec {
                session_id: mapping.session.session_id.clone().into(),
                workdir: PathBuf::from(mapping.session.workdir.as_str()),
                model: mapping.session.model.clone(),
                instruction_prelude: Some(instruction),
                environment,
            })
            .await;

        match spawn_result {
            Ok(handle) => {
                if let Ok(Some(harness_session_id)) =
                    worker_backend.harness_session_id(&handle).await
                {
                    if let Err(error) = store.upsert_harness_session_binding(
                        &mapping.session.session_id,
                        &mapping.session.backend_kind,
                        harness_session_id.as_str(),
                    ) {
                        tracing::warn!(
                            session_id = mapping.session.session_id.as_str(),
                            error = %error,
                            "failed to persist harness session binding during startup rehydrate"
                        );
                    }
                }

                let nudge = startup_rehydrate_nudge(&workflow_state, was_working);
                let mut nudge_bytes = nudge.as_bytes().to_vec();
                if !nudge_bytes.ends_with(b"\n") {
                    nudge_bytes.push(b'\n');
                }
                if let Err(error) = worker_backend.send_input(&handle, &nudge_bytes).await {
                    tracing::warn!(
                        session_id = mapping.session.session_id.as_str(),
                        backend = ?mapping.session.backend_kind,
                        state = ?workflow_state,
                        error = %error,
                        "failed to send startup resume nudge to rehydrated session"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    session_id = mapping.session.session_id.as_str(),
                    backend = ?mapping.session.backend_kind,
                    error = %error,
                    "failed to rehydrate inflight session on startup"
                );
            }
        }
    }

    Ok(())
}

fn latest_workflow_state_for_work_item(
    store: &SqliteEventStore,
    work_item_id: &WorkItemId,
) -> Result<WorkflowState, CoreError> {
    let mut current = WorkflowState::New;
    let events = store.read_events_for_work_item(work_item_id)?;
    for event in events {
        if let OrchestrationEventPayload::WorkflowTransition(payload) = event.payload {
            current = payload.to;
        }
    }
    Ok(current)
}

fn is_past_planning_state(state: &WorkflowState) -> bool {
    !matches!(state, WorkflowState::New | WorkflowState::Planning)
}

fn resume_instruction_from_ticket(ticket: &TicketRecord, workflow_state: &WorkflowState) -> String {
    let provider_name = match ticket.provider {
        TicketProvider::Linear => "linear",
        TicketProvider::Shortcut => "shortcut",
    };
    if is_past_planning_state(workflow_state) {
        return format!(
            "Ticket provider: {provider_name}. Resume work on {}: {}. Current workflow state is {:?}. Planning is already complete for this ticket, so continue from the current workflow state. For ticket operations, use the {provider_name} ticketing integration/skill.",
            ticket.identifier, ticket.title, workflow_state
        );
    }
    format!(
        "Ticket provider: {provider_name}. Resume work on {}: {} in Planning mode. Reconcile the current state, refresh the plan, and wait for an explicit workflow transition command before implementation. For ticket operations, use the {provider_name} ticketing integration/skill.",
        ticket.identifier, ticket.title
    )
}

fn startup_rehydrate_nudge(workflow_state: &WorkflowState, was_working: bool) -> String {
    let interruption_suffix = if was_working {
        format!(" {STARTUP_RESUME_NUDGE}")
    } else {
        String::new()
    };

    if is_past_planning_state(workflow_state) {
        return format!(
            "Planning is already complete for this ticket. End planning mode now. Current workflow state: {:?}.{}",
            workflow_state, interruption_suffix
        );
    }

    format!(
        "Current workflow state: {:?}.{}",
        workflow_state, interruption_suffix
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ticket() -> TicketRecord {
        TicketRecord {
            ticket_id: "linear-1".into(),
            provider: TicketProvider::Linear,
            provider_ticket_id: "1".to_owned(),
            identifier: "AP-1".to_owned(),
            title: "Implement startup rehydrate resume logic".to_owned(),
            state: "In Progress".to_owned(),
            updated_at: "2026-02-20T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn past_planning_state_detection_is_correct() {
        assert!(!is_past_planning_state(&WorkflowState::New));
        assert!(!is_past_planning_state(&WorkflowState::Planning));
        assert!(is_past_planning_state(&WorkflowState::Implementing));
        assert!(is_past_planning_state(&WorkflowState::PRDrafted));
    }

    #[test]
    fn post_planning_resume_instruction_does_not_force_planning_mode() {
        let instruction =
            resume_instruction_from_ticket(&sample_ticket(), &WorkflowState::Implementing);
        assert!(instruction.contains("Planning is already complete"));
        assert!(!instruction.contains("in Planning mode"));
    }

    #[test]
    fn post_planning_nudge_requests_exit_from_planning_mode_when_interrupted() {
        let nudge = startup_rehydrate_nudge(&WorkflowState::Implementing, true);
        assert!(nudge.contains("End planning mode now."));
        assert!(nudge.contains(STARTUP_RESUME_NUDGE));
    }

    #[test]
    fn planning_nudge_keeps_planning_mode_active_when_interrupted() {
        let nudge = startup_rehydrate_nudge(&WorkflowState::Planning, true);
        assert!(!nudge.contains("End planning mode now."));
        assert!(nudge.contains(STARTUP_RESUME_NUDGE));
    }

    #[test]
    fn startup_rehydrate_nudge_omits_interruption_message_when_not_working() {
        let post_planning = startup_rehydrate_nudge(&WorkflowState::Implementing, false);
        assert!(post_planning.contains("End planning mode now."));
        assert!(!post_planning.contains(STARTUP_RESUME_NUDGE));

        let planning = startup_rehydrate_nudge(&WorkflowState::Planning, false);
        assert!(!planning.contains("End planning mode now."));
        assert!(!planning.contains(STARTUP_RESUME_NUDGE));
    }
}
