pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    supervisor_provider: Option<Arc<dyn LlmProvider>>,
    supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
    ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
}

impl Ui {
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            supervisor_provider: None,
            supervisor_command_dispatcher: None,
            ticket_picker_provider: None,
            worker_backend: None,
        })
    }

    pub fn with_supervisor_provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.supervisor_provider = Some(provider);
        self
    }

    pub fn with_supervisor_command_dispatcher(
        mut self,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
    ) -> Self {
        self.supervisor_command_dispatcher = Some(dispatcher);
        self
    }

    pub fn with_ticket_picker_provider(mut self, provider: Arc<dyn TicketPickerProvider>) -> Self {
        self.ticket_picker_provider = Some(provider);
        self
    }

    pub fn with_worker_backend(mut self, backend: Arc<dyn WorkerBackend>) -> Self {
        self.worker_backend = Some(backend);
        self
    }

    pub fn run(&mut self, status: &str, projection: &ProjectionState) -> io::Result<()> {
        let mut shell_state = UiShellState::new_with_integrations(
            status.to_owned(),
            projection.clone(),
            self.supervisor_provider.clone(),
            self.supervisor_command_dispatcher.clone(),
            self.ticket_picker_provider.clone(),
            self.worker_backend.clone(),
        );
        loop {
            shell_state.tick_supervisor_stream();
            shell_state.tick_ticket_picker();
            shell_state.tick_terminal_view();
            shell_state.ensure_startup_session_feed_opened();
            let ui_state = shell_state.ui_state();
            self.terminal.draw(|frame| {
                let area = frame.area();
                let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
                let [main, footer] = layout.areas(area);
                let main_layout =
                    Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]);
                let [left_area, center_area] = main_layout.areas(main);
                let left_layout =
                    Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]);
                let [sessions_area, inbox_area] = left_layout.areas(left_area);

                let sessions_text = render_sessions_panel(
                    &shell_state.domain,
                    &shell_state.terminal_session_states,
                    shell_state.selected_session_id_for_panel().as_ref(),
                );
                frame.render_widget(
                    Paragraph::new(sessions_text)
                        .block(Block::default().title("sessions").borders(Borders::ALL)),
                    sessions_area,
                );

                let inbox_text = render_inbox_panel(&ui_state);
                frame.render_widget(
                    Paragraph::new(inbox_text)
                        .block(Block::default().title("inbox").borders(Borders::ALL)),
                    inbox_area,
                );

                let center_text = render_center_panel(&ui_state);
                if let Some(session_id) = shell_state.active_terminal_session_id().cloned() {
                    let center_layout = Layout::vertical([
                        Constraint::Length(3),
                        Constraint::Min(1),
                        Constraint::Length(6),
                    ]);
                    let [terminal_meta_area, terminal_output_area, terminal_input_area] =
                        center_layout.areas(center_area);
                    let meta_text = render_terminal_top_bar(&shell_state.domain, &session_id);
                    frame.render_widget(
                        Paragraph::new(meta_text)
                            .block(Block::default().title("terminal").borders(Borders::ALL)),
                        terminal_meta_area,
                    );

                    let output_text = render_terminal_output_panel(
                        &ui_state,
                        terminal_output_area.width.saturating_sub(2),
                    );
                    let output_text = append_terminal_loading_indicator(
                        output_text,
                        terminal_activity_indicator(
                            &shell_state.domain,
                            &shell_state.terminal_session_states,
                            &session_id,
                        ),
                    );
                    let content_height =
                        estimate_wrapped_line_count(&output_text, terminal_output_area.width);
                    let viewport_height = terminal_output_area.height.saturating_sub(2).max(1);
                    shell_state.sync_terminal_output_viewport(
                        output_text.lines.len(),
                        usize::from(viewport_height),
                    );
                    let (scroll_y, output_cursor_row, output_cursor_col) = shell_state
                        .terminal_session_states
                        .get(&session_id)
                        .map(|view| {
                            let max_scroll = content_height.saturating_sub(viewport_height);
                            let scroll = (view.output_scroll_line as u16).min(max_scroll);
                            let row = view
                                .output_cursor_line
                                .saturating_sub(view.output_scroll_line);
                            (scroll, row, view.output_cursor_col)
                        })
                        .unwrap_or((0, 0, 0));
                    frame.render_widget(
                        Paragraph::new(output_text)
                            .wrap(Wrap { trim: false })
                            .scroll((scroll_y, 0))
                            .block(Block::default().title("output").borders(Borders::ALL)),
                        terminal_output_area,
                    );
                    if shell_state.mode == UiMode::Normal
                        && output_cursor_row < usize::from(viewport_height)
                    {
                        let max_col = usize::from(terminal_output_area.width.saturating_sub(3));
                        let col = output_cursor_col.min(max_col);
                        let cursor_x = terminal_output_area
                            .x
                            .saturating_add(1)
                            .saturating_add(u16::try_from(col).unwrap_or(0));
                        let cursor_y = terminal_output_area
                            .y
                            .saturating_add(1)
                            .saturating_add(u16::try_from(output_cursor_row).unwrap_or(0));
                        frame.set_cursor_position((cursor_x, cursor_y));
                    }

                    let input_text =
                        render_terminal_input_panel(shell_state.terminal_compose_draft.as_str());
                    frame.render_widget(
                        Paragraph::new(input_text).wrap(Wrap { trim: false }).block(
                            Block::default()
                                .title("input (Enter send, Shift+Enter newline)")
                                .borders(Borders::ALL),
                        ),
                        terminal_input_area,
                    );
                    if shell_state.mode == UiMode::Terminal {
                        if let Some((cursor_x, cursor_y)) = terminal_input_cursor(
                            terminal_input_area,
                            shell_state.terminal_compose_draft.as_str(),
                        ) {
                            frame.set_cursor_position((cursor_x, cursor_y));
                        }
                    }
                } else {
                    frame.render_widget(
                        Paragraph::new(center_text).block(
                            Block::default()
                                .title(ui_state.center_pane.title.as_str())
                                .borders(Borders::ALL),
                        ),
                        center_area,
                    );
                }

                let footer_text = format!(
                    "status: {} | mode: {} | {}",
                    ui_state.status,
                    shell_state.mode.label(),
                    mode_help(shell_state.mode)
                );
                frame.render_widget(
                    Paragraph::new(footer_text)
                        .block(Block::default().title("shell").borders(Borders::ALL)),
                    footer,
                );

                if let Some(which_key) = shell_state.which_key_overlay.as_ref() {
                    render_which_key_overlay(frame, center_area, which_key);
                }
                if shell_state.ticket_picker_overlay.visible {
                    render_ticket_picker_overlay(frame, main, &shell_state.ticket_picker_overlay);
                }
                if let Some(modal) = shell_state.worktree_diff_modal.as_ref() {
                    render_worktree_diff_modal(frame, main, modal);
                }
                if let Some(session_id) = shell_state.review_merge_confirm_session.as_ref() {
                    render_review_merge_confirm_overlay(frame, main, session_id);
                }
            })?;

            if shell_state.ticket_picker_overlay.has_repository_prompt()
                || (shell_state.mode == UiMode::Terminal && shell_state.is_terminal_view_active())
            {
                let _ = io::stdout()
                    .execute(Show)
                    .and_then(|stdout| stdout.execute(SetCursorStyle::BlinkingBlock));
            } else {
                let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
            }

            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && handle_key_press(&mut shell_state, key) {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

