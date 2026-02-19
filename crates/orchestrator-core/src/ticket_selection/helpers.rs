async fn resolve_single_repository(
    vcs: &dyn VcsProvider,
    root: &Path,
    provider: &TicketProvider,
    project_id: &ProjectId,
) -> Result<RepositoryRef, CoreError> {
    let repositories = vcs.discover_repositories(&[root.to_path_buf()]).await?;
    if repositories.is_empty() {
        return Err(CoreError::InvalidMappedRepository {
            provider: ticket_provider_name(provider).to_owned(),
            project: project_id.as_str().to_owned(),
            repository_path: root.to_string_lossy().into_owned(),
            reason: "No git repositories were discovered under mapped path".to_owned(),
        });
    }
    if repositories.len() > 1 {
        return Err(CoreError::InvalidMappedRepository {
            provider: ticket_provider_name(provider).to_owned(),
            project: project_id.as_str().to_owned(),
            repository_path: root.to_string_lossy().into_owned(),
            reason: format!(
                "Multiple repositories were discovered; ticket-selected start/resume requires a single repository context (found: {}).",
                repositories
                    .iter()
                    .map(|repo| repo.root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    Ok(repositories[0].clone())
}

fn selected_ticket_project_id(
    selected_ticket: &TicketSummary,
    fallback_project_id: &ProjectId,
) -> ProjectId {
    selected_ticket
        .project
        .as_deref()
        .map(|project| project.trim().to_owned())
        .filter(|project| !project.is_empty())
        .map(ProjectId::new)
        .unwrap_or_else(|| fallback_project_id.clone())
}

fn mapping_worktree_exists(mapping: &RuntimeMappingRecord) -> bool {
    let primary = PathBuf::from(mapping.worktree.path.as_str());
    if primary.exists() {
        return true;
    }

    let session_workdir = PathBuf::from(mapping.session.workdir.as_str());
    session_workdir.exists()
}

fn ticket_provider_name(provider: &TicketProvider) -> &'static str {
    match provider {
        TicketProvider::Linear => "linear",
        TicketProvider::Shortcut => "shortcut",
    }
}

fn cleanup_completed_session_worktrees(mappings: &[RuntimeMappingRecord], worktrees_root: &Path) {
    for mapping in mappings {
        if mapping.session.status != WorkerSessionStatus::Done {
            continue;
        }

        let worktree_path = PathBuf::from(mapping.session.workdir.clone());
        if !worktree_path.exists() || !worktree_path.starts_with(worktrees_root) {
            continue;
        }

        let _ = fs::remove_dir_all(&worktree_path);
    }
}

async fn resolve_repository_for_ticket(
    store: &SqliteEventStore,
    vcs: &dyn VcsProvider,
    provider: &TicketProvider,
    project_id: &ProjectId,
    repository_override: Option<PathBuf>,
) -> Result<RepositoryRef, CoreError> {
    let repository_path = repository_override
        .or_else(|| {
            store
                .find_project_repository_mapping(provider, project_id)
                .ok()
                .flatten()
                .map(PathBuf::from)
        })
        .ok_or_else(|| CoreError::MissingProjectRepositoryMapping {
            provider: ticket_provider_name(provider).to_owned(),
            project: project_id.as_str().to_owned(),
        })?;
    let repository_path = expand_tilde_repository_path(repository_path, project_id.as_str())?;

    resolve_single_repository(vcs, &repository_path, provider, project_id).await
}

fn expand_tilde_repository_path(path: PathBuf, project_id: &str) -> Result<PathBuf, CoreError> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return resolve_core_home().map(PathBuf::from).ok_or_else(|| {
            CoreError::Configuration(format!(
                "Cannot resolve repository path for project '{project_id}': HOME is not set."
            ))
        });
    }
    if let Some(suffix) = raw.strip_prefix("~/") {
        return resolve_core_home()
            .map(PathBuf::from)
            .map(|home| home.join(suffix))
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "Cannot resolve repository path for project '{project_id}': HOME is not set."
                ))
            });
    }
    if let Some(suffix) = raw.strip_prefix("~\\") {
        return resolve_core_home()
            .map(PathBuf::from)
            .map(|home| home.join(suffix))
            .ok_or_else(|| {
                CoreError::Configuration(format!(
                    "Cannot resolve repository path for project '{project_id}': HOME is not set."
                ))
            });
    }

    Ok(path)
}

fn resolve_core_home() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .map(PathBuf::from)
}

fn validate_flow_config(config: &SelectedTicketFlowConfig) -> Result<(), CoreError> {
    if config.repository_roots.is_empty() {
        return Err(CoreError::Configuration(
            "SelectedTicketFlowConfig.repository_roots cannot be empty.".to_owned(),
        ));
    }
    if config.worktrees_root.as_os_str().is_empty() {
        return Err(CoreError::Configuration(
            "SelectedTicketFlowConfig.worktrees_root cannot be empty.".to_owned(),
        ));
    }
    if config.project_id.as_str().trim().is_empty() {
        return Err(CoreError::Configuration(
            "SelectedTicketFlowConfig.project_id cannot be empty.".to_owned(),
        ));
    }

    Ok(())
}

fn parse_ticket_provider_identity(
    ticket_id: &TicketId,
) -> Result<(TicketProvider, String), CoreError> {
    let raw = ticket_id.as_str();
    let (provider, provider_ticket_id) = raw.split_once(':').ok_or_else(|| {
        CoreError::Configuration(format!(
            "Ticket id '{}' is not in '<provider>:<id>' format.",
            raw
        ))
    })?;

    let provider = match provider {
        "linear" => TicketProvider::Linear,
        "shortcut" => TicketProvider::Shortcut,
        other => {
            return Err(CoreError::Configuration(format!(
                "Unsupported ticket provider '{}' in ticket id '{}'.",
                other, raw
            )))
        }
    };

    let provider_ticket_id = provider_ticket_id.trim();
    if provider_ticket_id.is_empty() {
        return Err(CoreError::Configuration(format!(
            "Ticket id '{}' has an empty provider-specific id.",
            raw
        )));
    }

    Ok((provider, provider_ticket_id.to_owned()))
}

fn normalize_issue_key(identifier: &str) -> Result<String, CoreError> {
    let token = identifier
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'));

    if token.is_empty() {
        return Err(CoreError::Configuration(
            "Selected ticket identifier is empty; cannot derive managed branch name.".to_owned(),
        ));
    }

    let normalized = token.to_ascii_uppercase();
    let Some((prefix, number)) = normalized.split_once('-') else {
        return Err(CoreError::Configuration(format!(
            "Ticket identifier '{}' must include an issue key and number (e.g. AP-126).",
            identifier
        )));
    };

    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(CoreError::Configuration(format!(
            "Ticket identifier '{}' has an invalid issue-key prefix '{}'.",
            identifier, prefix
        )));
    }
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CoreError::Configuration(format!(
            "Ticket identifier '{}' has an invalid issue number '{}'.",
            identifier, number
        )));
    }

    Ok(format!("{prefix}-{number}"))
}

fn title_slug(title: &str, max_len: usize) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            if slug.len() >= max_len {
                break;
            }
            slug.push(ch.to_ascii_lowercase());
            previous_was_dash = false;
            continue;
        }

        if !previous_was_dash && !slug.is_empty() {
            if slug.len() >= max_len {
                break;
            }
            slug.push('-');
            previous_was_dash = true;
        }
    }

    let trimmed = slug.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        DEFAULT_WORKTREE_SLUG.to_owned()
    } else {
        trimmed
    }
}

fn stable_component(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            previous_was_dash = false;
        } else if !previous_was_dash {
            normalized.push('-');
            previous_was_dash = true;
        }
    }

    let normalized = normalized.trim_matches('-').to_owned();
    if normalized.is_empty() {
        "ticket".to_owned()
    } else {
        normalized
    }
}

fn normalize_base_branch(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        DEFAULT_BASE_BRANCH.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn start_instruction(provider: &TicketProvider, ticket: &TicketSummary) -> String {
    let provider_name = ticket_provider_name(provider);
    format!(
        "Ticket provider: {provider_name}. Begin planning for {}: {}. Stay in planning mode and do not begin implementation until an explicit workflow transition command is provided. For ticket operations, use the {provider_name} ticketing integration/skill.",
        ticket.identifier, ticket.title
    )
}

fn start_turn_instruction(provider: &TicketProvider, ticket: &TicketSummary) -> String {
    let provider_name = ticket_provider_name(provider);
    format!(
        "Begin work on {}: {} in Planning mode only. Produce a concrete implementation plan and wait for an explicit workflow transition before starting implementation. For ticket operations, use the {provider_name} ticketing integration/skill.",
        ticket.identifier, ticket.title
    )
}

fn resume_instruction(provider: &TicketProvider, ticket: &TicketSummary) -> String {
    let provider_name = ticket_provider_name(provider);
    format!(
        "Ticket provider: {provider_name}. Resume work on {}: {} in Planning mode. Reconcile the current state, refresh the plan, and wait for an explicit workflow transition command before implementation. For ticket operations, use the {provider_name} ticketing integration/skill.",
        ticket.identifier, ticket.title
    )
}

fn ticket_record_from_summary(
    summary: &TicketSummary,
    provider: TicketProvider,
    provider_ticket_id: String,
    fallback_updated_at: &str,
) -> TicketRecord {
    let updated_at = if summary.updated_at.trim().is_empty() {
        fallback_updated_at.to_owned()
    } else {
        summary.updated_at.clone()
    };

    TicketRecord {
        ticket_id: summary.ticket_id.clone(),
        provider,
        provider_ticket_id,
        identifier: summary.identifier.clone(),
        title: summary.title.clone(),
        state: summary.state.clone(),
        updated_at,
    }
}

fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", now.as_secs(), now.subsec_nanos())
}

fn next_event_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = EVENT_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("evt-{prefix}-{now}-{count}")
}

