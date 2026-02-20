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

async fn run_merge_queue_command_task(
    dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    request: MergeQueueRequest,
    sender: mpsc::Sender<MergeQueueEvent>,
) {
    let command = match request.kind {
        MergeQueueCommandKind::Reconcile => Command::WorkflowReconcilePrMerge,
        MergeQueueCommandKind::Merge => Command::WorkflowMergePr,
    };
    let invocation = match CommandRegistry::default().to_untyped_invocation(&command) {
        Ok(invocation) => invocation,
        Err(error) => {
            let _ = sender
                .send(MergeQueueEvent::Completed {
                    session_id: request.session_id,
                    kind: request.kind,
                    completed: false,
                    merge_conflict: false,
                    base_branch: None,
                    head_branch: None,
                    error: Some(sanitize_terminal_display_text(error.to_string().as_str())),
                })
                .await;
            return;
        }
    };

    let (_, mut stream) = match dispatcher
        .dispatch_supervisor_command(invocation, request.context)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = sender
                .send(MergeQueueEvent::Completed {
                    session_id: request.session_id,
                    kind: request.kind,
                    completed: false,
                    merge_conflict: false,
                    base_branch: None,
                    head_branch: None,
                    error: Some(sanitize_terminal_display_text(error.to_string().as_str())),
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
                    .send(MergeQueueEvent::Completed {
                        session_id: request.session_id,
                        kind: request.kind,
                        completed: false,
                        merge_conflict: false,
                        base_branch: None,
                        head_branch: None,
                        error: Some(sanitize_terminal_display_text(error.to_string().as_str())),
                    })
                    .await;
                return;
            }
        }
    }

    let parsed = parse_merge_queue_response(output.as_str());
    let _ = sender
        .send(MergeQueueEvent::Completed {
            session_id: request.session_id,
            kind: request.kind,
            completed: parsed.completed,
            merge_conflict: parsed.merge_conflict,
            base_branch: parsed.base_branch,
            head_branch: parsed.head_branch,
            error: None,
        })
        .await;
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
        Ok(()) => {
            let _ = sender
                .send(MergeQueueEvent::SessionFinalized { session_id })
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

async fn run_session_archive_task(
    provider: Arc<dyn TicketPickerProvider>,
    session_id: WorkerSessionId,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.archive_session(session_id.clone()).await {
        Ok(warning) => {
            let _ = sender
                .send(TicketPickerEvent::SessionArchived {
                    session_id,
                    warning,
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

#[derive(Debug, Default)]
struct MergeQueueResponse {
    completed: bool,
    merge_conflict: bool,
    base_branch: Option<String>,
    head_branch: Option<String>,
}

fn parse_merge_queue_response(output: &str) -> MergeQueueResponse {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return MergeQueueResponse::default();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return MergeQueueResponse::default();
    };

    MergeQueueResponse {
        completed: value
            .get("completed")
            .and_then(|entry| entry.as_bool())
            .unwrap_or(false),
        merge_conflict: value
            .get("merge_conflict")
            .and_then(|entry| entry.as_bool())
            .unwrap_or(false),
        base_branch: value
            .get("base_branch")
            .and_then(|entry| entry.as_str())
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty()),
        head_branch: value
            .get("head_branch")
            .and_then(|entry| entry.as_str())
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty()),
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

async fn run_ticket_picker_load_task(
    provider: Arc<dyn TicketPickerProvider>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    match provider.list_unfinished_tickets().await {
        Ok(tickets) => {
            let projects = match provider.list_projects().await {
                Ok(projects) => projects,
                Err(_) => Vec::new(),
            };
            let _ = sender
                .send(TicketPickerEvent::TicketsLoaded {
                    tickets,
                    projects,
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
            let projection = provider.reload_projection().await.ok();
            let _ = sender
                .send(TicketPickerEvent::SessionWorkflowAdvanced {
                    outcome,
                    projection,
                })
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

async fn run_ticket_picker_start_task(
    provider: Arc<dyn TicketPickerProvider>,
    ticket: TicketSummary,
    repository_override: Option<PathBuf>,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let started_ticket = ticket.clone();

    let result = match provider
        .start_or_resume_ticket(ticket, repository_override)
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

    let _ = sender
        .send(TicketPickerEvent::TicketStarted {
            started_session_id: result.mapping.session.session_id,
            projection,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

async fn run_ticket_picker_create_task(
    provider: Arc<dyn TicketPickerProvider>,
    brief: String,
    sender: mpsc::Sender<TicketPickerEvent>,
) {
    let created_ticket = match provider.create_and_start_ticket_from_brief(brief).await {
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

    let _ = sender
        .send(TicketPickerEvent::TicketCreatedAndStarted {
            created_ticket,
            projection,
            tickets,
            warning: (!warning.is_empty()).then(|| warning.join("; ")),
        })
        .await;
}

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
    let mut lines = if overlay.new_ticket_mode {
        vec![
            "Type brief | Enter: create + start | Backspace: edit | Esc: cancel".to_owned(),
        ]
    } else {
        vec![
            "j/k or arrows: move | h/Left: fold | l/Right: unfold | Enter: start | x: archive | n: new ticket | Esc: close"
                .to_owned(),
        ]
    };

    if overlay.loading {
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
                    "{selected_prefix}{starting_prefix} {archive_marker} {}: {}",
                    ticket.identifier,
                    compact_focus_card_text(ticket.title.as_str())
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
    if shell_state.apply_needs_input_note_key(key) {
        return RoutedInput::Ignore;
    }

    if is_escape_to_normal(key) {
        shell_state.close_needs_input_modal();
        return RoutedInput::Ignore;
    }
    if !key.modifiers.is_empty() {
        if key.modifiers == KeyModifiers::SHIFT && matches!(key.code, KeyCode::BackTab) {
            shell_state.move_needs_input_question(-1);
            return RoutedInput::Ignore;
        }
        return RoutedInput::Ignore;
    }

    match key.code {
        KeyCode::Char('q') => {
            shell_state.close_needs_input_modal();
        }
        KeyCode::Char('i') => {
            shell_state.toggle_needs_input_note_insert_mode(true);
        }
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
            shell_state.move_needs_input_question(1);
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
            shell_state.move_needs_input_question(-1);
        }
        KeyCode::Enter => {
            let should_advance = shell_state
                .needs_input_modal
                .as_mut()
                .map(|modal| {
                    if modal.current_question_requires_option_selection() {
                        if modal.select_state.total_options == 0 {
                            return false;
                        }
                        let selected = modal
                            .select_state
                            .highlighted_index
                            .min(modal.select_state.total_options.saturating_sub(1));
                        modal.select_state.selected_index = Some(selected);
                    }
                    true
                })
                .unwrap_or(false);
            if !should_advance {
                return RoutedInput::Ignore;
            }

            let should_submit = shell_state
                .needs_input_modal
                .as_ref()
                .map(|modal| !modal.has_next_question())
                .unwrap_or(false);
            if should_submit {
                shell_state.submit_needs_input_modal();
            } else {
                shell_state.move_needs_input_question(1);
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
            if let Some(modal) = shell_state.needs_input_modal.as_mut() {
                let select_code = match key.code {
                    KeyCode::Char('j') => KeyCode::Down,
                    KeyCode::Char('k') => KeyCode::Up,
                    other => other,
                };
                let select_state = &mut modal.select_state;
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
            shell_state.cancel_create_ticket_from_picker();
            return RoutedInput::Ignore;
        }

        if key.modifiers == KeyModifiers::CONTROL {
            return RoutedInput::Ignore;
        }

        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                shell_state.submit_created_ticket_from_picker();
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                shell_state.pop_create_ticket_brief_char();
            }
            KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
                shell_state.append_create_ticket_brief_char(ch);
            }
            _ => {}
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
