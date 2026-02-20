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
        let mut force_draw = true;
        let mut last_animation_frame = Instant::now();
        let mut cached_ui_state: Option<UiState> = None;
        loop {
            let mut changed = false;
            changed |= shell_state.tick_supervisor_stream_and_report();
            changed |= shell_state.tick_ticket_picker_and_report();
            changed |= shell_state.tick_terminal_view_and_report();
            changed |= shell_state.ensure_startup_session_feed_opened_and_report();

            let animation_active = shell_state.has_active_animated_indicator();
            let now = Instant::now();
            let animation_frame_ready =
                now.duration_since(last_animation_frame) >= Duration::from_millis(200);
            let should_draw = force_draw || changed || (animation_active && animation_frame_ready);

            if should_draw {
                if animation_active && animation_frame_ready {
                    last_animation_frame = now;
                }
                let ui_state = if changed || cached_ui_state.is_none() {
                    let state = shell_state.ui_state();
                    cached_ui_state = Some(state.clone());
                    state
                } else {
                    cached_ui_state
                        .clone()
                        .expect("cached UI state should exist for redraw")
                };
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
                    let sessions_title = if shell_state.is_sessions_sidebar_focused() {
                        "sessions *"
                    } else {
                        "sessions"
                    };
                    let mut sessions_block = Block::default().title(sessions_title).borders(Borders::ALL);
                    if shell_state.is_sessions_sidebar_focused() {
                        sessions_block = sessions_block.border_style(Style::default().fg(Color::LightBlue));
                    }
                    frame.render_widget(
                        Paragraph::new(sessions_text).block(sessions_block),
                        sessions_area,
                    );

                    let inbox_text = render_inbox_panel(&ui_state);
                    let inbox_title = if shell_state.is_inbox_sidebar_focused() {
                        "inbox *"
                    } else {
                        "inbox"
                    };
                    let mut inbox_block = Block::default().title(inbox_title).borders(Borders::ALL);
                    if shell_state.is_inbox_sidebar_focused() {
                        inbox_block = inbox_block.border_style(Style::default().fg(Color::LightBlue));
                    }
                    frame.render_widget(
                        Paragraph::new(inbox_text).block(inbox_block),
                        inbox_area,
                    );

                    let center_text = render_center_panel(&ui_state);
                    let center_focused_style = shell_state
                        .is_right_pane_focused()
                        .then_some(Style::default().fg(Color::LightBlue));
                    if let Some(session_id) = shell_state.active_terminal_session_id().cloned() {
                        let active_needs_input = shell_state.active_terminal_needs_input().cloned();
                        let terminal_input_height = terminal_input_pane_height(
                            center_area.height,
                            center_area.width,
                            active_needs_input.as_ref(),
                        );
                        let center_layout = Layout::vertical([
                            Constraint::Length(3),
                            Constraint::Min(1),
                            Constraint::Length(terminal_input_height),
                        ]);
                        let [terminal_meta_area, terminal_output_area, terminal_input_area] =
                            center_layout.areas(center_area);
                        let meta_text = render_terminal_top_bar(&shell_state.domain, &session_id);
                        let mut terminal_meta_block = Block::default().title("terminal").borders(Borders::ALL);
                        if let Some(style) = center_focused_style {
                            terminal_meta_block = terminal_meta_block.border_style(style);
                        }
                        frame.render_widget(
                            Paragraph::new(meta_text).block(terminal_meta_block),
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
                        let scroll_y = shell_state
                            .terminal_session_states
                            .get(&session_id)
                            .map(|view| {
                                let max_scroll = content_height.saturating_sub(viewport_height);
                                (view.output_scroll_line as u16).min(max_scroll)
                            })
                            .unwrap_or(0);
                        let mut terminal_output_block = Block::default().title("output").borders(Borders::ALL);
                        if let Some(style) = center_focused_style {
                            terminal_output_block = terminal_output_block.border_style(style);
                        }
                        frame.render_widget(
                            Paragraph::new(output_text)
                                .wrap(Wrap { trim: false })
                                .scroll((scroll_y, 0))
                                .block(terminal_output_block),
                            terminal_output_area,
                        );

                        if let Some(prompt) = active_needs_input.as_ref() {
                            render_terminal_needs_input_panel(
                                frame,
                                terminal_input_area,
                                &shell_state.domain,
                                &session_id,
                                prompt,
                                shell_state.mode == UiMode::Terminal,
                            );
                        } else {
                            shell_state.terminal_compose_input.focused =
                                shell_state.mode == UiMode::Terminal;
                            TextArea::new()
                                .label("input (Enter send, Shift+Enter newline)")
                                .placeholder("Type a message to the harness here.\nPress Enter to send.")
                                .wrap_mode(WrapMode::Soft)
                                .render_stateful(
                                    frame,
                                    terminal_input_area,
                                    &mut shell_state.terminal_compose_input,
                                );
                        }
                    } else {
                        if shell_state.is_global_supervisor_chat_active() {
                            let [chat_output_area, chat_input_area] = Layout::vertical([
                                Constraint::Min(3),
                                Constraint::Length(3),
                            ])
                            .areas(center_area);
                            frame.render_widget(
                                Paragraph::new(center_text).block({
                                    let mut block = Block::default()
                                        .title(ui_state.center_pane.title.as_str())
                                        .borders(Borders::ALL);
                                    if let Some(style) = center_focused_style {
                                        block = block.border_style(style);
                                    }
                                    block
                                }),
                                chat_output_area,
                            );
                            shell_state.global_supervisor_chat_input.focused =
                                shell_state.mode == UiMode::Insert;
                            Input::new(&shell_state.global_supervisor_chat_input)
                                .label("draft (Enter send)")
                                .placeholder("Type supervisor query")
                                .render_stateful(frame, chat_input_area);
                        } else {
                            frame.render_widget(
                                Paragraph::new(center_text).block({
                                    let mut block = Block::default()
                                        .title(ui_state.center_pane.title.as_str())
                                        .borders(Borders::ALL);
                                    if let Some(style) = center_focused_style {
                                        block = block.border_style(style);
                                    }
                                    block
                                }),
                                center_area,
                            );
                        }
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
                    if let Some(ticket) = shell_state
                        .ticket_picker_overlay
                        .archive_confirm_ticket
                        .as_ref()
                    {
                        render_ticket_archive_confirm_overlay(frame, main, ticket);
                    }
                    if let Some(session_id) = shell_state.archive_session_confirm_session.as_ref() {
                        render_archive_session_confirm_overlay(
                            frame,
                            main,
                            &shell_state.domain,
                            session_id,
                        );
                    }
                    if let Some(modal) = shell_state.worktree_diff_modal.as_ref() {
                        render_worktree_diff_modal(frame, main, &shell_state.domain, modal);
                    }
                    if let Some(session_id) = shell_state.review_merge_confirm_session.as_ref() {
                        render_review_merge_confirm_overlay(
                            frame,
                            main,
                            &shell_state.domain,
                            session_id,
                        );
                    }
                })?;

                if shell_state.ticket_picker_overlay.has_repository_prompt()
                    || shell_state.terminal_needs_input_is_note_insert_mode()
                    || (shell_state.mode == UiMode::Terminal && shell_state.is_terminal_view_active())
                {
                    let _ = io::stdout()
                        .execute(Show)
                        .and_then(|stdout| stdout.execute(SetCursorStyle::BlinkingBlock));
                } else {
                    let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
                }
            }

            force_draw = false;
            let poll_timeout = if shell_state.has_active_animated_indicator() {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(250)
            };
            if event::poll(poll_timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && handle_key_press(&mut shell_state, key) {
                        break;
                    }
                    force_draw = true;
                    cached_ui_state = None;
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
