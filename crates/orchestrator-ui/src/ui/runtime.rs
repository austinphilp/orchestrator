pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    supervisor_provider: Option<Arc<dyn LlmProvider>>,
    supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
    ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
    keyboard_enhancement_enabled: bool,
}

impl Ui {
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let keyboard_enhancement_enabled =
            match crossterm::terminal::supports_keyboard_enhancement() {
                Ok(true) => stdout
                    .execute(crossterm::event::PushKeyboardEnhancementFlags(
                        crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                            | crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
                    ))
                    .is_ok(),
                _ => false,
            };
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            supervisor_provider: None,
            supervisor_command_dispatcher: None,
            ticket_picker_provider: None,
            worker_backend: None,
            keyboard_enhancement_enabled,
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
            let now = Instant::now();
            let mut changed = false;
            changed |= shell_state.drain_async_events_and_report();
            changed |= shell_state.run_due_periodic_tasks_and_report(now);
            changed |= shell_state.maintain_active_terminal_view_and_report();
            changed |= shell_state.ensure_startup_session_feed_opened_and_report();
            if changed {
                shell_state.invalidate_draw_caches();
            }

            shell_state.maybe_emit_projection_perf_log(now);
            let animation_state = shell_state.animation_state(now);
            let animation_frame_interval = match animation_state {
                AnimationState::ActiveTurn => Some(Duration::from_millis(200)),
                AnimationState::ResolvedOnly => Some(Duration::from_millis(1_000)),
                AnimationState::None => None,
            };
            let animation_frame_ready = animation_frame_interval
                .map(|interval| now.duration_since(last_animation_frame) >= interval)
                .unwrap_or(false);
            let should_draw = force_draw
                || changed
                || (animation_frame_interval.is_some() && animation_frame_ready);
            let should_refresh_ui_state = changed
                || cached_ui_state.is_none()
                || matches!(animation_state, AnimationState::ResolvedOnly) && animation_frame_ready;

            if should_draw {
                if animation_frame_ready {
                    last_animation_frame = now;
                }
                let ui_state = if should_refresh_ui_state {
                    let state = shell_state.ui_state_for_draw(now);
                    cached_ui_state = Some(state.clone());
                    state
                } else {
                    cached_ui_state
                        .clone()
                        .expect("cached UI state should exist for redraw")
                };
                self.terminal.draw(|frame| {
                    let area = frame.area();
                    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(4)]);
                    let [main, footer] = layout.areas(area);
                    let main_layout = Layout::horizontal([
                        Constraint::Percentage(35),
                        Constraint::Percentage(65),
                    ]);
                    let [left_area, center_area] = main_layout.areas(main);
                    let left_layout =
                        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]);
                    let [sessions_area, inbox_area] = left_layout.areas(left_area);

                    let sessions_viewport_rows =
                        usize::from(sessions_area.height.saturating_sub(2).max(1));
                    let selected_session_index = shell_state.selected_session_index;
                    let selected_session_id_hint = shell_state.selected_session_id.clone();
                    let session_panel_scroll_line = shell_state.session_panel_scroll_line();
                    let (session_metrics, sessions_text) = {
                        let session_rows = shell_state.session_panel_rows_for_draw();
                        let selected_session_id = if session_rows.is_empty() {
                            None
                        } else if let Some(selected_session_id) = selected_session_id_hint.as_ref() {
                            if session_rows
                                .iter()
                                .any(|row| &row.session_id == selected_session_id)
                            {
                                Some(selected_session_id.clone())
                            } else {
                                let index = selected_session_index.unwrap_or(0).min(session_rows.len() - 1);
                                session_rows.get(index).map(|row| row.session_id.clone())
                            }
                        } else {
                            let index = selected_session_index.unwrap_or(0).min(session_rows.len() - 1);
                            session_rows.get(index).map(|row| row.session_id.clone())
                        };
                        let metrics =
                            session_panel_line_metrics_from_rows(session_rows, selected_session_id.as_ref());
                        let text = render_sessions_panel_text_virtualized_from_rows(
                            session_rows,
                            selected_session_id.as_ref(),
                            session_panel_scroll_line,
                            sessions_viewport_rows,
                        );
                        (metrics, text)
                    };
                    shell_state.sync_session_panel_viewport(
                        session_metrics.total_lines,
                        session_metrics.selected_line,
                        sessions_viewport_rows,
                    );
                    let sessions_title = if shell_state.is_sessions_sidebar_focused() {
                        "sessions *"
                    } else {
                        "sessions"
                    };
                    let mut sessions_block =
                        Block::default().title(sessions_title).borders(Borders::ALL);
                    if shell_state.is_sessions_sidebar_focused() {
                        sessions_block =
                            sessions_block.border_style(Style::default().fg(Color::LightBlue));
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
                        inbox_block =
                            inbox_block.border_style(Style::default().fg(Color::LightBlue));
                    }
                    frame.render_widget(Paragraph::new(inbox_text).block(inbox_block), inbox_area);

                    let center_focused_style = shell_state
                        .is_right_pane_focused()
                        .then_some(Style::default().fg(Color::LightBlue));
                    if let Some(session_id) = shell_state.active_terminal_session_id().cloned() {
                        let (terminal_area, session_info_area) = if shell_state
                            .should_show_session_info_sidebar()
                            && center_area.width >= 80
                        {
                            let [terminal_area, sidebar_area] = Layout::horizontal([
                                Constraint::Percentage(68),
                                Constraint::Percentage(32),
                            ])
                            .areas(center_area);
                            (terminal_area, Some(sidebar_area))
                        } else {
                            (center_area, None)
                        };
                        let active_needs_input = shell_state.active_terminal_needs_input().cloned();
                        let terminal_input_height = terminal_input_pane_height(
                            terminal_area.height,
                            terminal_area.width,
                            active_needs_input.as_ref(),
                        );
                        let center_layout = Layout::vertical([
                            Constraint::Length(3),
                            Constraint::Min(1),
                            Constraint::Length(terminal_input_height),
                        ]);
                        let [terminal_meta_area, terminal_output_area, terminal_input_area] =
                            center_layout.areas(terminal_area);
                        let terminal_view_state =
                            shell_state.terminal_session_states.get(&session_id);
                        let meta_text = render_terminal_top_bar(
                            &shell_state.domain,
                            &session_id,
                            terminal_view_state,
                        );
                        let mut terminal_meta_block =
                            Block::default().title("terminal").borders(Borders::ALL);
                        if let Some(style) = center_focused_style {
                            terminal_meta_block = terminal_meta_block.border_style(style);
                        }
                        frame.render_widget(
                            Paragraph::new(meta_text).block(terminal_meta_block),
                            terminal_meta_area,
                        );

                        const TERMINAL_VIEWPORT_OVERSCAN_ROWS: usize = 8;
                        let output_width = terminal_output_area.width.saturating_sub(2).max(1);
                        let viewport_height = terminal_output_area.height.saturating_sub(2).max(1);
                        let indicator = terminal_activity_indicator(
                            &shell_state.domain,
                            &shell_state.terminal_session_states,
                            &session_id,
                        );
                        let total_rows = shell_state
                            .terminal_total_rendered_rows_for_session(
                                &session_id,
                                output_width,
                                indicator,
                            )
                            .max(1);
                        shell_state.sync_terminal_output_viewport(
                            total_rows,
                            usize::from(viewport_height),
                        );
                        let output_render = shell_state
                            .render_terminal_output_viewport_for_session(
                                &session_id,
                                TerminalViewportRequest {
                                    width: output_width,
                                    scroll_top: shell_state
                                        .terminal_session_states
                                        .get(&session_id)
                                        .map(|view| view.output_scroll_line)
                                        .unwrap_or(0),
                                    viewport_rows: usize::from(viewport_height),
                                    overscan_rows: TERMINAL_VIEWPORT_OVERSCAN_ROWS,
                                    indicator,
                                },
                            )
                            .unwrap_or_else(|| TerminalViewportRender {
                                text: Text::raw("No terminal output available yet."),
                                local_scroll_top: 0,
                            });
                        let mut terminal_output_block =
                            Block::default().title("output").borders(Borders::ALL);
                        if let Some(style) = center_focused_style {
                            terminal_output_block = terminal_output_block.border_style(style);
                        }
                        frame.render_widget(
                            Paragraph::new(output_render.text)
                                .wrap(Wrap { trim: false })
                                .scroll((output_render.local_scroll_top, 0))
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
                            frame.render_widget(
                                EditorView::new(&mut shell_state.terminal_compose_editor)
                                    .theme(nord_editor_theme(
                                        Block::default()
                                            .title("input (Esc+Enter send | Ctrl+Enter send)")
                                            .borders(Borders::ALL),
                                    ))
                                    .wrap(true),
                                terminal_input_area,
                            );
                        }

                        if let Some(sidebar_area) = session_info_area {
                            let sidebar_text = render_session_info_panel(
                                &shell_state.domain,
                                &session_id,
                                shell_state.session_info_diff_cache_for(&session_id),
                                shell_state.session_info_summary_cache_for(&session_id),
                                shell_state.session_ci_status_cache_for(&session_id),
                            );
                            frame.render_widget(
                                Paragraph::new(sidebar_text).block(
                                    Block::default().title("session info").borders(Borders::ALL),
                                ),
                                sidebar_area,
                            );
                        }
                    } else {
                        let center_text = render_center_panel(&ui_state);
                        if shell_state.is_global_supervisor_chat_active() {
                            let [chat_output_area, chat_input_area] =
                                Layout::vertical([Constraint::Min(3), Constraint::Length(3)])
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

                    let footer_style = bottom_bar_style(shell_state.mode);
                    let footer_text = Text::from(vec![
                        Line::from(format!(
                            "status: {} | mode: {} | app: {}",
                            ui_state.status,
                            shell_state.mode.label(),
                            shell_state.application_mode_label()
                        )),
                        Line::from(mode_help(shell_state.mode)),
                    ]);
                    frame.render_widget(
                        Paragraph::new(footer_text).style(footer_style).block(
                            Block::default()
                                .title("shell")
                                .borders(Borders::ALL)
                                .border_style(footer_style)
                                .style(footer_style),
                        ),
                        footer,
                    );

                    if let Some(which_key) = shell_state.which_key_overlay.as_ref() {
                        render_which_key_overlay(frame, center_area, which_key);
                    }
                    if shell_state.ticket_picker_overlay.visible {
                        render_ticket_picker_overlay(
                            frame,
                            main,
                            &mut shell_state.ticket_picker_overlay,
                        );
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
                    || (shell_state.mode == UiMode::Terminal
                        && shell_state.is_terminal_view_active())
                {
                    let _ = io::stdout()
                        .execute(Show)
                        .and_then(|stdout| stdout.execute(SetCursorStyle::BlinkingBlock));
                } else {
                    let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
                }
            }

            force_draw = false;
            let event_scan_interval = if matches!(animation_state, AnimationState::ActiveTurn)
                || shell_state.has_pending_async_activity()
            {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(1)
            };
            let wake_deadline =
                shell_state.next_wake_deadline(now, animation_state, last_animation_frame);
            let poll_timeout = match wake_deadline {
                Some(deadline) if deadline <= now => Duration::from_millis(0),
                Some(deadline) => deadline.duration_since(now).min(event_scan_interval),
                None => event_scan_interval,
            };
            if event::poll(poll_timeout)? {
                if let Event::Key(key) = event::read()? {
                    shell_state.invalidate_draw_caches();
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
        if self.keyboard_enhancement_enabled {
            let _ = io::stdout().execute(crossterm::event::PopKeyboardEnhancementFlags);
        }
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(SetCursorStyle::DefaultUserShape);
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn bottom_bar_style(mode: UiMode) -> Style {
    match mode {
        UiMode::Normal => Style::default()
            .fg(Color::Rgb(236, 239, 244))
            .bg(Color::Rgb(59, 66, 82)),
        UiMode::Insert => Style::default()
            .fg(Color::Rgb(236, 239, 244))
            .bg(Color::Rgb(76, 86, 106)),
        UiMode::Terminal => Style::default()
            .fg(Color::Rgb(236, 239, 244))
            .bg(Color::Rgb(94, 129, 172)),
    }
}
