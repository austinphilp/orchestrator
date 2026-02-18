use anyhow::Result;
use backend_codex::CodexBackend;
use backend_opencode::{OpenCodeBackend, OpenCodeBackendConfig};
use integration_git::{GitCliVcsProvider, ProcessCommandRunner as GitProcessCommandRunner};
use integration_linear::LinearTicketingProvider;
use integration_shortcut::ShortcutTicketingProvider;
use orchestrator_app::{App, AppConfig, AppTicketPickerProvider};
use orchestrator_core::{CoreError, TicketingProvider, WorkerBackend};
use orchestrator_github::{GhCliClient, ProcessCommandRunner as GhProcessCommandRunner};
use orchestrator_supervisor::OpenRouterSupervisor;
use orchestrator_ui::Ui;
use std::sync::Arc;

const ENV_TICKETING_PROVIDER: &str = "ORCHESTRATOR_TICKETING_PROVIDER";
const ENV_HARNESS_PROVIDER: &str = "ORCHESTRATOR_HARNESS_PROVIDER";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut config = AppConfig::from_env()?;
    let cli = parse_cli_flags()?;

    config.ticketing_provider = resolve_provider_name(
        cli.ticketing_provider.as_deref(),
        ENV_TICKETING_PROVIDER,
        &config.ticketing_provider,
    )?;
    config.harness_provider = resolve_provider_name(
        cli.harness_provider.as_deref(),
        ENV_HARNESS_PROVIDER,
        &config.harness_provider,
    )?;

    let supervisor = OpenRouterSupervisor::from_env()?;
    let github = GhCliClient::new(GhProcessCommandRunner)?;
    let (ticketing, linear_ticketing) = build_ticketing_provider(&config.ticketing_provider)?;
    let worker_backend = build_harness_provider(&config.harness_provider)?;
    let vcs = Arc::new(GitCliVcsProvider::new(GitProcessCommandRunner)?);

    ticketing.health_check().await?;
    worker_backend.health_check().await?;

    let app = Arc::new(App {
        config,
        ticketing: ticketing.clone(),
        supervisor,
        github,
    });
    let ticket_picker_provider = Arc::new(AppTicketPickerProvider::new(
        Arc::clone(&app),
        ticketing,
        vcs,
        worker_backend.clone(),
    ));

    let state = app.startup_state().await?;
    let supervisor_dispatcher: Arc<dyn orchestrator_ui::SupervisorCommandDispatcher> = app.clone();
    let mut ui = Ui::init()?
        .with_supervisor_command_dispatcher(supervisor_dispatcher)
        .with_ticket_picker_provider(ticket_picker_provider)
        .with_worker_backend(worker_backend.clone());
    app.start_linear_polling(linear_ticketing.as_deref()).await?;
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
    println!("  --ticketing-provider <provider>   Configure ticketing provider (linear or shortcut)");
    println!("  --harness-provider <provider>     Configure harness/backend provider (opencode or codex)");
    println!("  --help                            Show this help message");
}

fn read_cli_value(
    flag: &str,
    value: String,
) -> Result<String, CoreError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Flag '{flag}' requires a non-empty value."
        )));
    }
    Ok(value)
}

fn resolve_provider_name(
    cli: Option<&str>,
    env_var: &str,
    config_value: &str,
) -> Result<String, CoreError> {
    let raw = cli
        .map(|value| value.to_owned())
        .or_else(|| std::env::var(env_var).ok())
        .unwrap_or_else(|| config_value.to_owned());

    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        Ok(config_value.to_owned())
    } else {
        Ok(normalized)
    }
}

fn build_ticketing_provider(
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
            let provider = Arc::new(LinearTicketingProvider::from_env()?);
            let linear_ticketing = Arc::clone(&provider);
            let ticketing: Arc<dyn TicketingProvider + Send + Sync> = provider;
            Ok((ticketing, Some(linear_ticketing)))
        }
        "shortcut" => Ok((
            Arc::new(ShortcutTicketingProvider::from_env()?),
            None,
        )),
        other => Err(CoreError::Configuration(format!(
            "Unknown ticketing provider '{other}'. Expected 'linear' or 'shortcut'."
        ))),
    }
}

fn build_harness_provider(provider: &str) -> Result<Arc<dyn WorkerBackend + Send + Sync>, CoreError> {
    match provider {
        "opencode" => Ok(Arc::new(OpenCodeBackend::new(OpenCodeBackendConfig::default()))),
        "codex" => Ok(Arc::new(CodexBackend::from_env())),
        other => Err(CoreError::Configuration(format!(
            "Unknown harness provider '{other}'. Expected 'opencode' or 'codex'."
        ))),
    }
}
