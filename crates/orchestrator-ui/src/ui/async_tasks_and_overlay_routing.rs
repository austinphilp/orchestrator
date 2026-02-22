async fn run_supervisor_stream_task(
    provider: Arc<dyn LlmProvider>,
    request: LlmChatRequest,
    sender: mpsc::Sender<SupervisorStreamEvent>,
) {
    let (stream_id, stream) = match provider.stream_chat(request).await {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(SupervisorStreamEvent::Failed {
                    message: error.to_string(),
                })
                .await;
            return;
        }
    };

    relay_supervisor_stream(stream_id, stream, sender).await;
}

async fn run_supervisor_command_task(
    dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    invocation: UntypedCommandInvocation,
    context: SupervisorCommandContext,
    sender: mpsc::Sender<SupervisorStreamEvent>,
) {
    let (stream_id, stream) = match dispatcher
        .dispatch_supervisor_command(invocation, context)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(SupervisorStreamEvent::Failed {
                    message: error.to_string(),
                })
                .await;
            return;
        }
    };

    relay_supervisor_stream(stream_id, stream, sender).await;
}

async fn run_session_merge_finalize_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<MergeQueueEvent>,
) {
    match provider
        .complete_session_after_merge(session_id.clone())
        .await
    {
        Ok(outcome) => {
            let _ = sender
                .send(MergeQueueEvent::SessionFinalized {
                    session_id,
                    event: outcome.event,
                })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(MergeQueueEvent::SessionFinalizeFailed {
                    session_id,
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                })
                .await;
        }
    }
}

#[allow(dead_code)]
async fn run_session_archive_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.archive_session(session_id.clone()).await {
        Ok(outcome) => {
            let _ = sender
                .send(TicketPickerEvent::SessionArchived {
                    session_id,
                    warning: outcome.warning,
                    event: outcome.event,
                })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::SessionArchiveFailed {
                    session_id,
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                })
                .await;
        }
    }
}

async fn run_set_session_working_state_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    is_working: bool,
) {
    let _ = provider
        .set_session_working_state(session_id, is_working)
        .await;
}

async fn run_publish_inbox_item_task(
    provider: Arc<dyn TicketPickerProvider>,
    request: InboxPublishRequest,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.publish_inbox_item(request).await {
        Ok(event) => {
            let _ = sender
                .send(TicketPickerEvent::InboxItemPublished { event })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::InboxItemPublishFailed {
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                })
                .await;
        }
    }
}

async fn run_resolve_inbox_item_task(
    provider: Arc<dyn TicketPickerProvider>,
    request: InboxResolveRequest,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.resolve_inbox_item(request).await {
        Ok(event) => {
            let _ = sender
                .send(TicketPickerEvent::InboxItemResolved { event })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::InboxItemResolveFailed {
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                })
                .await;
        }
    }
}
async fn relay_supervisor_stream(
    stream_id: String,
    mut stream: LlmResponseStream,
    sender: mpsc::Sender<SupervisorStreamEvent>,
) {
    if sender
        .send(SupervisorStreamEvent::Started { stream_id })
        .await
        .is_err()
    {
        return;
    }

    loop {
        match stream.next_chunk().await {
            Ok(Some(chunk)) => {
                let delta = chunk.delta;
                let finish_reason = chunk.finish_reason;
                let usage = chunk.usage;
                let rate_limit = chunk.rate_limit;

                if !delta.is_empty()
                    && sender
                        .send(SupervisorStreamEvent::Delta { text: delta })
                        .await
                        .is_err()
                {
                    return;
                }

                if let Some(rate_limit) = rate_limit {
                    if sender
                        .send(SupervisorStreamEvent::RateLimit { state: rate_limit })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }

                if finish_reason.is_none() {
                    if let Some(usage) = usage.clone() {
                        if sender
                            .send(SupervisorStreamEvent::Usage { usage })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }

                if let Some(reason) = finish_reason {
                    let _ = sender
                        .send(SupervisorStreamEvent::Finished { reason, usage })
                        .await;
                    return;
                }
            }
            Ok(None) => {
                let _ = sender
                    .send(SupervisorStreamEvent::Finished {
                        reason: LlmFinishReason::Stop,
                        usage: None,
                    })
                    .await;
                return;
            }
            Err(error) => {
                let _ = sender
                    .send(SupervisorStreamEvent::Failed {
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
        }
    }
}

#[allow(dead_code)]
async fn run_ticket_picker_load_task(
    provider: Arc<dyn TicketPickerProvider>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.list_unfinished_tickets().await {
        Ok(tickets) => {
            let ticket_ids = tickets.iter().map(|ticket| ticket.ticket_id.clone()).collect();
            let profile_overrides = provider
                .list_ticket_profile_overrides(ticket_ids)
                .await
                .unwrap_or_default();
            let projects = match provider.list_projects().await {
                Ok(projects) => projects,
                Err(_) => Vec::new(),
            };
            let _ = sender
                .send(TicketPickerEvent::TicketsLoaded {
                    tickets,
                    projects,
                    profile_overrides,
                })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::TicketsLoadFailed {
                    message: error.to_string(),
                })
                .await;
        }
    }
}

async fn run_session_workflow_advance_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.advance_session_workflow(session_id.clone()).await {
        Ok(outcome) => {
            let _ = sender
                .send(TicketPickerEvent::SessionWorkflowAdvanced { outcome })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::SessionWorkflowAdvanceFailed {
                    session_id,
                    message: error.to_string(),
                })
                .await;
        }
    }
}

async fn run_ticket_profile_override_update_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket_id: TicketId,
    profile_name: Option<String>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider
        .set_ticket_profile_override(ticket_id.clone(), profile_name.clone())
        .await
    {
        Ok(()) => {
            let _ = sender
                .send(TicketPickerEvent::TicketProfileOverrideUpdated {
                    ticket_id,
                    profile_name,
                })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::TicketProfileOverrideUpdateFailed {
                    ticket_id,
                    message: error.to_string(),
                })
                .await;
        }
    }
}

async fn run_workflow_profiles_save_task(
    provider: Arc<dyn TicketPickerProvider>,
    config: WorkflowInteractionProfilesConfig,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.save_workflow_interaction_profiles(config).await {
        Ok(config) => {
            let _ = sender
                .send(TicketPickerEvent::WorkflowProfilesSaved { config })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::WorkflowProfilesSaveFailed {
                    message: error.to_string(),
                })
                .await;
        }
    }
}

async fn run_session_diff_load_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.session_worktree_diff(session_id.clone()).await {
        Ok(diff) => {
            let _ = sender
                .send(TicketPickerEvent::SessionDiffLoaded { diff })
                .await;
        }
        Err(error) => {
            let _ = sender
                .send(TicketPickerEvent::SessionDiffFailed {
                    session_id,
                    message: error.to_string(),
                })
                .await;
        }
    }
}

#[allow(dead_code)]
async fn run_session_info_summary_task(
    provider: Arc<dyn LlmProvider>,
    context: SessionInfoContext,
    sender: mpsc::Sender<SessionInfoSummaryEvent>,
) {
    let request = summarize_text_output_request(context.prompt.as_str());
    let (_, mut stream) = match provider.stream_chat(request).await {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(SessionInfoSummaryEvent::Failed {
                    session_id: context.session_id,
                    message: sanitize_terminal_display_text(error.to_string().as_str()),
                    context_fingerprint: context.context_fingerprint,
                })
                .await;
            return;
        }
    };

    let mut output = String::new();
    loop {
        match stream.next_chunk().await {
            Ok(Some(chunk)) => output.push_str(chunk.delta.as_str()),
            Ok(None) => break,
            Err(error) => {
                let _ = sender
                    .send(SessionInfoSummaryEvent::Failed {
                        session_id: context.session_id,
                        message: sanitize_terminal_display_text(error.to_string().as_str()),
                        context_fingerprint: context.context_fingerprint,
                    })
                    .await;
                return;
            }
        }
    }

    let summary = clamp_summary_text(output.as_str(), 200);
    let _ = sender
        .send(SessionInfoSummaryEvent::Completed {
            session_id: context.session_id,
            summary,
            context_fingerprint: context.context_fingerprint,
        })
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_start_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket: TicketSummary,
    repository_override: Option<PathBuf>,
    profile_override: Option<String>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let started_ticket = ticket.clone();

    let result = match provider
        .start_or_resume_ticket(ticket, repository_override, profile_override)
        .await
    {
        Ok(result) => result,
        Err(error) => match &error {
            CoreError::MissingProjectRepositoryMapping { project, .. } => {
                let _ = sender
                    .send(TicketPickerEvent::TicketStartRequiresRepository {
                        ticket: started_ticket,
                        project_id: project.clone(),
                        repository_path_hint: None,
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
            CoreError::InvalidMappedRepository {
                project,
                repository_path,
                ..
            } => {
                let _ = sender
                    .send(TicketPickerEvent::TicketStartRequiresRepository {
                        ticket: started_ticket,
                        project_id: project.clone(),
                        repository_path_hint: Some(repository_path.clone()),
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
            error => {
                let _ = sender
                    .send(TicketPickerEvent::TicketStartFailed {
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
        },
    };

    let mut warning = Vec::new();
    let projection = match provider.reload_projection().await {
        Ok(projection) => Some(projection),
        Err(error) => {
            warning.push(format!("failed to reload projection: {error}"));
            None
        }
    };
    let tickets = match provider.list_unfinished_tickets().await {
        Ok(tickets) => Some(tickets),
        Err(error) => {
            warning.push(format!("failed to refresh tickets: {error}"));
            None
        }
    };
    let profile_overrides = match tickets.as_ref() {
        Some(tickets) => {
            let ticket_ids = tickets.iter().map(|ticket| ticket.ticket_id.clone()).collect();
            provider
                .list_ticket_profile_overrides(ticket_ids)
                .await
                .ok()
        }
        None => None,
    };

    let _ = sender
        .send(TicketPickerEvent::TicketStarted {
            started_session_id: result.mapping.session.session_id,
            projection,
            tickets,
            profile_overrides,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_create_task(
    provider: Arc<dyn TicketPickerProvider>,
    request: CreateTicketFromPickerRequest,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let submit_mode = request.submit_mode.clone();
    let created_ticket = match provider.create_ticket_from_brief(request).await {
        Ok(ticket) => ticket,
        Err(error) => {
            let tickets = match provider.list_unfinished_tickets().await {
                Ok(tickets) => Some(tickets),
                Err(_) => None,
            };
            let _ = sender
                .send(TicketPickerEvent::TicketCreateFailed {
                    message: error.to_string(),
                    tickets,
                    warning: None,
                })
                .await;
            return;
        }
    };

    let mut warning = Vec::new();
    let tickets = match provider.list_unfinished_tickets().await {
        Ok(tickets) => Some(tickets),
        Err(error) => {
            warning.push(format!("failed to refresh tickets: {error}"));
            None
        }
    };
    if created_ticket.assignee.is_none() {
        warning.push(
            "ticket may be unassigned: could not confirm assignment to API-key user".to_owned(),
        );
    }

    let _ = sender
        .send(TicketPickerEvent::TicketCreated {
            created_ticket,
            submit_mode,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

#[allow(dead_code)]
async fn run_ticket_picker_archive_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket: TicketSummary,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let archived_ticket = ticket.clone();
    if let Err(error) = provider.archive_ticket(ticket).await {
        let tickets = match provider.list_unfinished_tickets().await {
            Ok(tickets) => Some(tickets),
            Err(_) => None,
        };
        let _ = sender
            .send(TicketPickerEvent::TicketArchiveFailed {
                ticket: archived_ticket,
                message: error.to_string(),
                tickets,
            })
            .await;
        return;
    }

    let mut warning = Vec::new();
    let tickets = match provider.list_unfinished_tickets().await {
        Ok(tickets) => Some(tickets),
        Err(error) => {
            warning.push(format!("failed to refresh tickets: {error}"));
            None
        }
    };

    let _ = sender
        .send(TicketPickerEvent::TicketArchived {
            archived_ticket,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

fn expand_tilde_path(raw: &str) -> Option<PathBuf> {
    if raw == "~" {
        return resolve_shell_home().map(PathBuf::from);
    }

    if let Some(suffix) = raw.strip_prefix("~/") {
        return resolve_shell_home().map(|home| PathBuf::from(home).join(suffix));
    }

    if let Some(suffix) = raw.strip_prefix("~\\") {
        return resolve_shell_home().map(|home| PathBuf::from(home).join(suffix));
    }

    Some(PathBuf::from(raw))
}

fn resolve_shell_home() -> Option<String> {
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
}

fn render_ticket_picker_overlay_text(overlay: &TicketPickerOverlayState) -> String {
    let default_profile = workflow_profiles_config_value().default_profile;
    let mut lines = if overlay.new_ticket_mode {
        vec![
            "Describe ticket | Vim editor (i/Esc) | Enter: create | Shift+Enter: create + start | Esc: cancel"
                .to_owned(),
        ]
    } else {
        vec![
            "j/k or arrows: move | h/Left: fold | l/Right: unfold | Enter: start | p: cycle profile | x: archive | n: new ticket | `: profiles | Esc: close"
                .to_owned(),
        ]
    };

    if overlay.loading && !overlay.creating {
        lines.push("Loading unfinished tickets...".to_owned());
    }
    if let Some(starting_ticket_id) = overlay.starting_ticket_id.as_ref() {
        lines.push(format!("Starting {}...", starting_ticket_id.as_str()));
    }
    if let Some(archiving_ticket_id) = overlay.archiving_ticket_id.as_ref() {
        lines.push(format!("Archiving {}...", archiving_ticket_id.as_str()));
    }
    if overlay.creating {
        lines.push("Creating ticket...".to_owned());
    }
    if let Some(error) = overlay.error.as_ref() {
        lines.push(format!(
            "Error: {}",
            compact_focus_card_text(error.as_str())
        ));
    }

    if overlay.has_repository_prompt() {
        lines.push(String::new());
        let project_id = overlay
            .repository_prompt_project_id
            .as_deref()
            .unwrap_or("selected project");
        if overlay.repository_prompt_missing_mapping {
            lines.push(format!(
                "Repository mapping missing for project '{project_id}'.",
            ));
        } else {
            lines.push(format!(
                "Repository path could not be resolved for project '{project_id}'.",
            ));
        }
        lines.push("Enter local repository path, then press Enter. Esc to cancel.".to_owned());
        lines.push(format!("Path: {}", overlay.repository_prompt_input.text()));
        lines.push(String::new());
    }

    if overlay.project_groups.is_empty() {
        lines.push("No unfinished tickets found.".to_owned());
        return lines.join("\n");
    }

    for (project_index, project_group) in overlay.project_groups.iter().enumerate() {
        let project_selected = overlay
            .selected_row()
            .map(|row| {
                matches!(
                    row,
                    TicketPickerRowRef::ProjectHeader {
                        project_index: selected_project_index,
                    } if *selected_project_index == project_index
                )
            })
            .unwrap_or_default();
        let selected_prefix = if project_selected { ">" } else { " " };
        let fold_marker = if project_group.collapsed {
            "[+]"
        } else {
            "[-]"
        };
        lines.push(format!(
            "{selected_prefix}{fold_marker} {} ({})",
            project_group.project,
            project_group.ticket_count()
        ));

        if project_group.collapsed {
            lines.push(String::new());
            continue;
        }

        for (status_index, status_group) in project_group.status_groups.iter().enumerate() {
            lines.push(format!(
                "   {} ({})",
                status_group.status,
                status_group.tickets.len()
            ));
            for (ticket_index, ticket) in status_group.tickets.iter().enumerate() {
                let is_selected = overlay
                    .selected_row()
                    .map(|row| {
                        matches!(
                            row,
                            TicketPickerRowRef::Ticket {
                                project_index: selected_project_index,
                                status_index: selected_status_index,
                                ticket_index: selected_ticket_index,
                            } if *selected_project_index == project_index
                                && *selected_status_index == status_index
                                && *selected_ticket_index == ticket_index
                        )
                    })
                    .unwrap_or_default();
                let selected_prefix = if is_selected { ">" } else { " " };
                let starting_prefix = if overlay
                    .starting_ticket_id
                    .as_ref()
                    .map(|id| id == &ticket.ticket_id)
                    .unwrap_or_default()
                {
                    "*"
                } else {
                    " "
                };
                let archive_marker = if overlay
                    .archive_confirm_ticket
                    .as_ref()
                    .map(|selected| selected.ticket_id == ticket.ticket_id)
                    .unwrap_or_default()
                {
                    "[X]"
                } else {
                    "[x]"
                };
                lines.push(format!(
                    "{selected_prefix}{starting_prefix} {archive_marker} {}: {}{}",
                    ticket.identifier,
                    compact_focus_card_text(ticket.title.as_str()),
                    overlay.ticket_override_for(&ticket.ticket_id).map_or_else(
                        || format!(" [profile: {}]", default_profile),
                        |profile| format!(" [profile: {profile}]"),
                    )
                ));
            }
        }
        lines.push(String::new());
    }

    if lines.last().map(|line| line.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    lines.join("\n")
}

fn route_needs_input_modal_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key)
        && shell_state.is_terminal_view_active()
        && shell_state.active_terminal_session_requires_manual_needs_input_activation()
    {
        let _ = shell_state.deactivate_terminal_needs_input_interaction();
        shell_state.enter_normal_mode();
        return RoutedInput::Ignore;
    }

    if shell_state.apply_terminal_needs_input_note_key(key) {
        return RoutedInput::Ignore;
    }

    if !shell_state.is_terminal_view_active() {
        return RoutedInput::Ignore;
    }
    if is_escape_to_normal(key) {
        return RoutedInput::Ignore;
    }
    if !key.modifiers.is_empty() {
        return RoutedInput::Ignore;
    }

    match key.code {
        KeyCode::Char('i') => {
            shell_state.toggle_terminal_needs_input_note_insert_mode(true);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            shell_state.move_terminal_needs_input_question(1);
        }
        KeyCode::Left | KeyCode::Char('h') => {
            shell_state.move_terminal_needs_input_question(-1);
        }
        KeyCode::Enter => {
            let should_advance = shell_state
                .active_terminal_needs_input_mut()
                .map(|prompt| {
                    if prompt.current_question_requires_option_selection() {
                        if prompt.select_state.total_options == 0 {
                            return false;
                        }
                        let selected = prompt
                            .select_state
                            .highlighted_index
                            .min(prompt.select_state.total_options.saturating_sub(1));
                        prompt.select_state.selected_index = Some(selected);
                    }
                    true
                })
                .unwrap_or(false);
            if !should_advance {
                return RoutedInput::Ignore;
            }

            let should_submit = shell_state
                .active_terminal_needs_input()
                .map(|prompt| !prompt.has_next_question())
                .unwrap_or(false);
            if should_submit {
                shell_state.submit_terminal_needs_input_response();
            } else {
                shell_state.move_terminal_needs_input_question(1);
            }
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Char(' ')
        | KeyCode::Char('j')
        | KeyCode::Char('k') => {
            if let Some(prompt) = shell_state.active_terminal_needs_input_mut() {
                let select_code = match key.code {
                    KeyCode::Char('j') => KeyCode::Down,
                    KeyCode::Char('k') => KeyCode::Up,
                    other => other,
                };
                let select_state = &mut prompt.select_state;
                if select_state.enabled && select_state.focused && select_state.total_options > 0 {
                    match select_code {
                        KeyCode::Char(' ') => {
                            let selected = select_state
                                .highlighted_index
                                .min(select_state.total_options.saturating_sub(1));
                            select_state.selected_index = Some(selected);
                        }
                        KeyCode::Up => {
                            select_state.highlight_prev();
                        }
                        KeyCode::Down => {
                            select_state.highlight_next();
                        }
                        KeyCode::Home => {
                            select_state.highlight_first();
                        }
                        KeyCode::End => {
                            select_state.highlight_last();
                        }
                        KeyCode::PageUp => {
                            for _ in 0..5 {
                                select_state.highlight_prev();
                            }
                        }
                        KeyCode::PageDown => {
                            for _ in 0..5 {
                                select_state.highlight_next();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    RoutedInput::Ignore
}

fn route_ticket_picker_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.ticket_picker_overlay.has_repository_prompt() {
        return route_ticket_picker_repository_prompt_key(shell_state, key);
    }

    if shell_state.ticket_picker_overlay.archive_confirm_ticket.is_some() {
        if is_escape_to_normal(key) {
            shell_state.cancel_ticket_picker_archive_confirmation();
            return RoutedInput::Ignore;
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    shell_state.submit_ticket_picker_archive_confirmation();
                    return RoutedInput::Ignore;
                }
                KeyCode::Char('n') => {
                    shell_state.cancel_ticket_picker_archive_confirmation();
                    return RoutedInput::Ignore;
                }
                _ => {}
            }
        }
        return RoutedInput::Ignore;
    }

    if shell_state.ticket_picker_overlay.new_ticket_mode {
        if is_escape_to_normal(key) {
            if shell_state.ticket_picker_overlay.new_ticket_brief_editor.mode != EditorMode::Normal
            {
                if let Some(key_input) = map_edtui_key_input(key) {
                    shell_state
                        .ticket_picker_overlay
                        .new_ticket_brief_event_handler
                        .on_key_event(
                            key_input,
                            &mut shell_state.ticket_picker_overlay.new_ticket_brief_editor,
                        );
                }
            } else {
                shell_state.cancel_create_ticket_from_picker();
            }
            return RoutedInput::Ignore;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return RoutedInput::Ignore;
        }

        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                shell_state.submit_created_ticket_from_picker(TicketCreateSubmitMode::CreateOnly);
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                shell_state.submit_created_ticket_from_picker(TicketCreateSubmitMode::CreateAndStart);
            }
            _ => {
                if let Some(key_input) = map_edtui_key_input(key) {
                    shell_state
                        .ticket_picker_overlay
                        .new_ticket_brief_event_handler
                        .on_key_event(
                            key_input,
                            &mut shell_state.ticket_picker_overlay.new_ticket_brief_editor,
                        );
                }
            }
        }
        return RoutedInput::Ignore;
    }

    if is_escape_to_normal(key) {
        return RoutedInput::Command(UiCommand::CloseTicketPicker);
    }

    if !key.modifiers.is_empty() {
        return RoutedInput::Ignore;
    }

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => RoutedInput::Command(UiCommand::TicketPickerMoveNext),
        KeyCode::Char('k') | KeyCode::Up => {
            RoutedInput::Command(UiCommand::TicketPickerMovePrevious)
        }
        KeyCode::Char('h') | KeyCode::Left => {
            RoutedInput::Command(UiCommand::TicketPickerFoldProject)
        }
        KeyCode::Char('l') | KeyCode::Right => {
            RoutedInput::Command(UiCommand::TicketPickerUnfoldProject)
        }
        KeyCode::Enter => RoutedInput::Command(UiCommand::TicketPickerStartSelected),
        KeyCode::Char('x') => {
            shell_state.begin_archive_selected_ticket_from_picker();
            RoutedInput::Ignore
        }
        KeyCode::Char('n') => {
            shell_state.begin_create_ticket_from_picker();
            RoutedInput::Ignore
        }
        KeyCode::Char('`') => {
            shell_state.open_workflow_profiles_modal();
            RoutedInput::Ignore
        }
        KeyCode::Char('p') => {
            shell_state.cycle_selected_ticket_profile_override();
            RoutedInput::Ignore
        }
        _ => RoutedInput::Ignore,
    }
}

fn route_review_merge_confirm_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.cancel_review_merge_confirmation();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                shell_state.confirm_review_merge();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('n') => {
                shell_state.cancel_review_merge_confirmation();
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
}

fn route_archive_session_confirm_key(
    shell_state: &mut UiShellState,
    key: KeyEvent,
) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.cancel_archive_selected_session_confirmation();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                shell_state.confirm_archive_selected_session();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('n') => {
                shell_state.cancel_archive_selected_session_confirmation();
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
}

fn route_ticket_picker_repository_prompt_key(
    shell_state: &mut UiShellState,
    key: KeyEvent,
) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.cancel_ticket_picker_repository_prompt();
        return RoutedInput::Ignore;
    }

    if matches!(key.code, KeyCode::Enter) {
        shell_state.submit_ticket_picker_repository_prompt();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Backspace | KeyCode::Delete => {
                shell_state.pop_repository_prompt_char();
                return RoutedInput::Ignore;
            }
            KeyCode::Char(ch) => {
                shell_state.append_repository_prompt_char(ch);
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
}
