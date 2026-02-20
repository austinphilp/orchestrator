struct UiShellState {
    base_status: String,
    status_warning: Option<String>,
    domain: ProjectionState,
    selected_inbox_index: Option<usize>,
    selected_inbox_item_id: Option<InboxItemId>,
    view_stack: ViewStack,
    keymap: &'static KeymapTrie,
    mode_key_buffer: Vec<KeyStroke>,
    which_key_overlay: Option<WhichKeyOverlayState>,
    mode: UiMode,
    terminal_escape_pending: bool,
    supervisor_provider: Option<Arc<dyn LlmProvider>>,
    supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
    supervisor_chat_stream: Option<ActiveSupervisorChatStream>,
    global_supervisor_chat_input: InputState,
    global_supervisor_chat_last_query: Option<String>,
    global_supervisor_chat_return_context: Option<GlobalSupervisorChatReturnContext>,
    ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
    ticket_picker_sender: Option<mpsc::Sender<TicketPickerEvent>>,
    ticket_picker_receiver: Option<mpsc::Receiver<TicketPickerEvent>>,
    ticket_picker_overlay: TicketPickerOverlayState,
    ticket_picker_priority_states: Vec<String>,
    startup_session_feed_opened: bool,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
    selected_session_index: Option<usize>,
    terminal_session_sender: Option<mpsc::Sender<TerminalSessionEvent>>,
    terminal_session_receiver: Option<mpsc::Receiver<TerminalSessionEvent>>,
    terminal_session_states: HashMap<WorkerSessionId, TerminalViewState>,
    terminal_session_streamed: HashSet<WorkerSessionId>,
    pending_needs_input_prompts: HashMap<WorkerSessionId, VecDeque<NeedsInputPromptState>>,
    needs_input_modal: Option<NeedsInputModalState>,
    terminal_compose_input: TextAreaState,
    archive_session_confirm_session: Option<WorkerSessionId>,
    archiving_session_id: Option<WorkerSessionId>,
    review_merge_confirm_session: Option<WorkerSessionId>,
    merge_queue: VecDeque<MergeQueueRequest>,
    merge_last_dispatched_at: Option<Instant>,
    merge_last_poll_at: Option<Instant>,
    merge_event_sender: Option<mpsc::Sender<MergeQueueEvent>>,
    merge_event_receiver: Option<mpsc::Receiver<MergeQueueEvent>>,
    merge_pending_sessions: HashSet<WorkerSessionId>,
    merge_finalizing_sessions: HashSet<WorkerSessionId>,
    review_sync_instructions_sent: HashSet<WorkerSessionId>,
    worktree_diff_modal: Option<WorktreeDiffModalState>,
}

impl UiShellState {
    #[cfg(test)]
    fn new(status: String, domain: ProjectionState) -> Self {
        Self::new_with_integrations(status, domain, None, None, None, None)
    }
    #[cfg(test)]
    fn new_with_supervisor(
        status: String,
        domain: ProjectionState,
        supervisor_provider: Option<Arc<dyn LlmProvider>>,
    ) -> Self {
        Self::new_with_integrations(status, domain, supervisor_provider, None, None, None)
    }

    fn new_with_integrations(
        status: String,
        domain: ProjectionState,
        supervisor_provider: Option<Arc<dyn LlmProvider>>,
        supervisor_command_dispatcher: Option<Arc<dyn SupervisorCommandDispatcher>>,
        ticket_picker_provider: Option<Arc<dyn TicketPickerProvider>>,
        worker_backend: Option<Arc<dyn WorkerBackend>>,
    ) -> Self {
        let (ticket_picker_sender, ticket_picker_receiver) = if ticket_picker_provider.is_some() {
            let (sender, receiver) = mpsc::channel(TICKET_PICKER_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let (terminal_session_sender, terminal_session_receiver) = if worker_backend.is_some() {
            let (sender, receiver) = mpsc::channel(TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let (merge_event_sender, merge_event_receiver) = if supervisor_command_dispatcher.is_some()
        {
            let (sender, receiver) = mpsc::channel(TERMINAL_STREAM_EVENT_CHANNEL_CAPACITY);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };

        Self {
            base_status: status,
            status_warning: None,
            domain,
            selected_inbox_index: None,
            selected_inbox_item_id: None,
            view_stack: ViewStack::default(),
            keymap: default_keymap_trie(),
            mode_key_buffer: Vec::new(),
            which_key_overlay: None,
            mode: UiMode::Normal,
            terminal_escape_pending: false,
            supervisor_provider,
            supervisor_command_dispatcher,
            supervisor_chat_stream: None,
            global_supervisor_chat_input: InputState::empty(),
            global_supervisor_chat_last_query: None,
            global_supervisor_chat_return_context: None,
            ticket_picker_provider,
            ticket_picker_sender,
            ticket_picker_receiver,
            ticket_picker_overlay: TicketPickerOverlayState::default(),
            ticket_picker_priority_states: ticket_picker_priority_states_from_env(),
            startup_session_feed_opened: false,
            worker_backend,
            selected_session_index: None,
            terminal_session_sender,
            terminal_session_receiver,
            terminal_session_states: HashMap::new(),
            terminal_session_streamed: HashSet::new(),
            pending_needs_input_prompts: HashMap::new(),
            needs_input_modal: None,
            terminal_compose_input: TextAreaState::empty().with_tab_config(TabConfig::Literal),
            archive_session_confirm_session: None,
            archiving_session_id: None,
            review_merge_confirm_session: None,
            merge_queue: VecDeque::new(),
            merge_last_dispatched_at: None,
            merge_last_poll_at: None,
            merge_event_sender,
            merge_event_receiver,
            merge_pending_sessions: HashSet::new(),
            merge_finalizing_sessions: HashSet::new(),
            review_sync_instructions_sent: HashSet::new(),
            worktree_diff_modal: None,
        }
    }

    fn ui_state(&self) -> UiState {
        let status = self.status_text();
        let terminal_view_state = self
            .active_terminal_session_id()
            .and_then(|session_id| self.terminal_session_states.get(session_id));
        let mut ui_state = project_ui_state(
            status.as_str(),
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
            terminal_view_state,
        );
        self.append_global_supervisor_chat_state(&mut ui_state);
        self.append_live_supervisor_chat(&mut ui_state);
        ui_state
    }

    fn status_text(&self) -> String {
        let base_status = sanitize_terminal_display_text(self.base_status.as_str());
        match self.status_warning.as_deref() {
            Some(warning) => format!(
                "{} | warning: {}",
                base_status,
                sanitize_terminal_display_text(warning)
            ),
            None => base_status,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let ui_state = self.ui_state();
        if ui_state.selected_inbox_item_id.is_none() && self.move_session_selection(delta) {
            return;
        }
        if ui_state.inbox_rows.is_empty() {
            self.selected_inbox_index = None;
            self.selected_inbox_item_id = None;
            return;
        }

        let current = ui_state.selected_inbox_index.unwrap_or(0) as isize;
        let upper_bound = ui_state.inbox_rows.len() as isize - 1;
        let next = (current + delta).clamp(0, upper_bound) as usize;
        self.set_selection(Some(next), &ui_state.inbox_rows);
    }

    fn jump_to_first_item(&mut self) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            if self.move_to_first_session() {
                return;
            }
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(0), &ui_state.inbox_rows);
    }

    fn jump_to_last_item(&mut self) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            if self.move_to_last_session() {
                return;
            }
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(ui_state.inbox_rows.len() - 1), &ui_state.inbox_rows);
    }

    fn jump_to_batch(&mut self, target: InboxBatchKind) {
        let ui_state = self.ui_state();
        if let Some(index) = ui_state
            .inbox_batch_surfaces
            .iter()
            .find(|surface| surface.kind == target)
            .and_then(UiBatchSurface::selection_index)
        {
            self.set_selection(Some(index), &ui_state.inbox_rows);
        }
    }

    fn cycle_batch(&mut self, delta: isize) {
        let ui_state = self.ui_state();
        let selectable_surfaces = ui_state
            .inbox_batch_surfaces
            .iter()
            .filter(|surface| surface.total_count > 0)
            .collect::<Vec<_>>();

        if selectable_surfaces.is_empty() {
            return;
        }

        let current_surface_idx = ui_state.selected_inbox_index.and_then(|selected_index| {
            let selected_row = ui_state.inbox_rows.get(selected_index)?;
            selectable_surfaces
                .iter()
                .position(|surface| selected_row.batch_kind == surface.kind)
        });

        let current = current_surface_idx.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(selectable_surfaces.len() as isize) as usize;

        if let Some(index) = selectable_surfaces[next].selection_index() {
            self.set_selection(Some(index), &ui_state.inbox_rows);
        }
    }

    fn open_focus_card_for_selected(&mut self) {
        let ui_state = self.ui_state();
        if let Some(inbox_item_id) = ui_state.selected_inbox_item_id {
            self.selected_inbox_item_id = Some(inbox_item_id.clone());
            self.view_stack
                .replace_center(CenterView::FocusCardView { inbox_item_id });
        }
    }

    fn open_terminal_for_selected(&mut self) {
        let ui_state = self.ui_state();
        let selected_inbox_item_id = ui_state.selected_inbox_item_id.clone();
        let has_selected_inbox_item = selected_inbox_item_id.is_some();
        let mut terminal_session_id = None;
        if let (Some(inbox_item_id), Some(session_id)) = (
            selected_inbox_item_id.as_ref(),
            ui_state.selected_session_id,
        ) {
            self.open_focus_and_push_center(
                inbox_item_id.clone(),
                CenterView::TerminalView {
                    session_id: session_id.clone(),
                },
            );
            terminal_session_id = Some(session_id.clone());
        } else if let Some(session_id) = self.selected_session_id_for_panel() {
            if let Some(inbox_item_id) = self.inbox_item_id_for_session(&session_id) {
                self.open_focus_and_push_center(
                    inbox_item_id,
                    CenterView::TerminalView {
                        session_id: session_id.clone(),
                    },
                );
                terminal_session_id = Some(session_id);
            } else {
                let _ = self.view_stack.push_center(CenterView::TerminalView {
                    session_id: session_id.clone(),
                });
                terminal_session_id = Some(session_id);
            }
        } else {
            match self.spawn_manual_terminal_session() {
                Ok(Some(session_id)) => {
                    terminal_session_id = Some(session_id.clone());
                    if let Some(inbox_item_id) = selected_inbox_item_id {
                        self.open_focus_and_push_center(
                            inbox_item_id,
                            CenterView::TerminalView { session_id },
                        );
                    } else {
                        let _ = self
                            .view_stack
                            .push_center(CenterView::TerminalView { session_id });
                    }
                }
                Ok(None) => {
                    if has_selected_inbox_item {
                        self.status_warning = Some(
                            "terminal unavailable: selected inbox item has no active session"
                                .to_owned(),
                        );
                    } else {
                        self.status_warning = Some(
                            "terminal unavailable: no open session is currently selected"
                                .to_owned(),
                        );
                    }
                }
                Err(error) => {
                    self.status_warning = Some(format!("terminal unavailable: {error}"));
                }
            }
        }

        if let Some(session_id) = terminal_session_id {
            self.ensure_terminal_stream(session_id);
        }
    }

    fn spawn_manual_terminal_session(&mut self) -> Result<Option<WorkerSessionId>, String> {
        let Some(backend) = self.worker_backend.clone() else {
            return Ok(None);
        };

        let workdir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let session_id = WorkerSessionId::new(format!(
            "manual-{nanos}",
            nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since_epoch| since_epoch.as_nanos())
                .unwrap_or(0)
        ));
        let spec = SpawnSpec {
            session_id: session_id.clone().into(),
            workdir,
            model: None,
            instruction_prelude: None,
            environment: Vec::new(),
        };

        let spawn_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("terminal spawn runtime unavailable: {error}"))?;

            runtime
                .block_on(async move { backend.spawn(spec).await })
                .map_err(|error| error.to_string())
        });
        let handle = spawn_thread
            .join()
            .map_err(|_| "terminal spawn worker thread panicked".to_owned())?
            .map_err(|error| format!("terminal spawn failed: {error}"))?;

        if handle.session_id.as_str() != session_id.as_str() {
            return Err(format!(
                "worker backend returned unexpected session id: expected '{expected}', got '{actual}'",
                expected = session_id.as_str(),
                actual = handle.session_id.as_str()
            ));
        }

        Ok(Some(session_id))
    }

    fn selected_session_id_for_terminal_action(&self) -> Option<WorkerSessionId> {
        self.active_terminal_session_id()
            .cloned()
            .or_else(|| self.selected_session_id_for_panel())
    }

    fn begin_archive_selected_session_confirmation(&mut self) {
        if self.archive_session_confirm_session.is_some() || self.archiving_session_id.is_some() {
            return;
        }
        let Some(session_id) = self.selected_session_id_for_terminal_action() else {
            self.status_warning = Some("session archive unavailable: no session selected".to_owned());
            return;
        };
        self.archive_session_confirm_session = Some(session_id);
        self.status_warning = None;
    }

    fn cancel_archive_selected_session_confirmation(&mut self) {
        self.archive_session_confirm_session = None;
    }

    fn confirm_archive_selected_session(&mut self) {
        let Some(session_id) = self.archive_session_confirm_session.take() else {
            return;
        };
        if self.archiving_session_id.is_some() {
            return;
        }
        self.archiving_session_id = Some(session_id.clone());
        self.status_warning = Some(format!("archiving session {}", session_id.as_str()));
        self.spawn_session_archive(session_id);
    }

    fn spawn_session_archive(&mut self, session_id: WorkerSessionId) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.archiving_session_id = None;
            self.status_warning = Some("session archive unavailable: ticket provider not configured".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.archiving_session_id = None;
            self.status_warning =
                Some("session archive unavailable: ticket event channel unavailable".to_owned());
            return;
        };
        match TokioHandle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    run_session_archive_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                self.archiving_session_id = None;
                self.status_warning = Some(
                    "session archive unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn session_panel_rows(&self) -> Vec<(WorkerSessionId, String, String)> {
        session_panel_rows(&self.domain, &self.terminal_session_states)
    }

    fn session_ids_for_navigation(&self) -> Vec<WorkerSessionId> {
        self.session_panel_rows()
            .into_iter()
            .map(|(session_id, _repo, _line)| session_id)
            .collect()
    }

    fn selected_session_id_for_panel(&self) -> Option<WorkerSessionId> {
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            return None;
        }
        let index = self
            .selected_session_index
            .unwrap_or(0)
            .min(session_ids.len() - 1);
        session_ids.get(index).cloned()
    }

    fn move_session_selection(&mut self, delta: isize) -> bool {
        let session_ids = self.session_ids_for_navigation();
        let len = session_ids.len();
        if len == 0 {
            self.selected_session_index = None;
            return false;
        }

        let mut index = self.selected_session_index.unwrap_or(0);
        if index >= len {
            index = len - 1;
        }
        let next = (index as isize + delta).rem_euclid(len as isize) as usize;
        self.selected_session_index = Some(next);
        self.show_selected_session_output();
        true
    }

    fn move_to_first_session(&mut self) -> bool {
        if self.session_ids_for_navigation().is_empty() {
            self.selected_session_index = None;
            false
        } else {
            self.selected_session_index = Some(0);
            self.show_selected_session_output();
            true
        }
    }

    fn move_to_last_session(&mut self) -> bool {
        let len = self.session_ids_for_navigation().len();
        if len == 0 {
            self.selected_session_index = None;
            false
        } else {
            self.selected_session_index = Some(len - 1);
            self.show_selected_session_output();
            true
        }
    }

    fn show_selected_session_output(&mut self) {
        let Some(session_id) = self.selected_session_id_for_panel() else {
            return;
        };
        let should_switch_view = !matches!(
            self.view_stack.active_center(),
            Some(CenterView::TerminalView {
                session_id: active_session_id
            }) if *active_session_id == session_id
        );
        if should_switch_view {
            self.view_stack.replace_center(CenterView::TerminalView {
                session_id: session_id.clone(),
            });
        }
        self.ensure_terminal_stream(session_id);
    }

    fn ensure_startup_session_feed_opened(&mut self) {
        if self.startup_session_feed_opened {
            return;
        }
        if self.active_terminal_session_id().is_some() {
            self.startup_session_feed_opened = true;
            return;
        }
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            return;
        }
        if self.selected_session_index.is_none() {
            self.selected_session_index = Some(0);
        }
        self.show_selected_session_output();
        self.startup_session_feed_opened = true;
    }

    fn focus_and_stream_session(&mut self, session_id: WorkerSessionId) {
        let session_ids = self.session_ids_for_navigation();
        if let Some(index) = session_ids.iter().position(|candidate| candidate == &session_id) {
            self.selected_session_index = Some(index);
        }

        if let Some(inbox_item_id) = self.inbox_item_id_for_session(&session_id) {
            self.open_focus_and_push_center(
                inbox_item_id,
                CenterView::TerminalView {
                    session_id: session_id.clone(),
                },
            );
        } else {
            self.view_stack.replace_center(CenterView::TerminalView {
                session_id: session_id.clone(),
            });
        }

        self.ensure_terminal_stream(session_id);
    }

    fn inbox_item_id_for_session(&self, session_id: &WorkerSessionId) -> Option<InboxItemId> {
        self.domain
            .work_items
            .values()
            .find_map(|work_item| {
                if work_item.session_id.as_ref() == Some(session_id) {
                    work_item.inbox_items.first().cloned()
                } else {
                    None
                }
            })
            .and_then(|inbox_item_id| {
                self.domain
                    .inbox_items
                    .get(&inbox_item_id)
                    .cloned()
                    .map(|_| inbox_item_id)
            })
    }

    fn active_terminal_session_id(&self) -> Option<&WorkerSessionId> {
        match self.view_stack.active_center() {
            Some(CenterView::TerminalView { session_id }) => Some(session_id),
            _ => None,
        }
    }

    fn active_terminal_view_state_mut(&mut self) -> Option<&mut TerminalViewState> {
        let session_id = self.active_terminal_session_id()?.clone();
        self.terminal_session_states.get_mut(&session_id)
    }

    fn snap_active_terminal_output_to_bottom(&mut self) {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return;
        };

        let rendered_line_count = terminal_output_line_count_for_scroll(view);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return;
        }

        view.output_follow_tail = true;
        view.output_scroll_line = rendered_line_count.saturating_sub(view.output_viewport_rows.max(1));
    }

    fn sync_terminal_output_viewport(&mut self, rendered_line_count: usize, viewport_rows: usize) {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return;
        };

        view.output_rendered_line_count = rendered_line_count;
        view.output_viewport_rows = viewport_rows.max(1);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return;
        }

        let max_scroll = rendered_line_count.saturating_sub(view.output_viewport_rows);
        if view.output_follow_tail {
            view.output_scroll_line = max_scroll;
            return;
        }

        view.output_scroll_line = view.output_scroll_line.min(max_scroll);
    }

    fn scroll_terminal_output_view(&mut self, delta: isize) -> bool {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return false;
        };
        let rendered_line_count = terminal_output_line_count_for_scroll(view);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return false;
        }

        let max_scroll = rendered_line_count.saturating_sub(view.output_viewport_rows.max(1));
        let current = view.output_scroll_line.min(max_scroll);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(max_scroll)
        };
        view.output_scroll_line = next;
        view.output_follow_tail = next == max_scroll;
        next != current
    }

    fn scroll_terminal_output_to_bottom(&mut self) -> bool {
        let Some(view) = self.active_terminal_view_state_mut() else {
            return false;
        };
        let rendered_line_count = terminal_output_line_count_for_scroll(view);
        if rendered_line_count == 0 {
            view.output_scroll_line = 0;
            view.output_follow_tail = true;
            return false;
        }
        let max_scroll = rendered_line_count.saturating_sub(view.output_viewport_rows.max(1));
        let previous = view.output_scroll_line.min(max_scroll);
        view.output_scroll_line = max_scroll;
        view.output_follow_tail = true;
        view.output_scroll_line != previous
    }

    fn terminal_session_handle(&self, session_id: &WorkerSessionId) -> Option<SessionHandle> {
        Some(SessionHandle {
            session_id: RuntimeSessionId::from(session_id.clone()),
            backend: self.worker_backend.as_ref()?.kind(),
        })
    }

    fn open_inspector_for_selected(&mut self, inspector: ArtifactInspectorKind) {
        let ui_state = self.ui_state();
        if let (Some(inbox_item_id), Some(work_item_id)) = (
            ui_state.selected_inbox_item_id,
            ui_state.selected_work_item_id,
        ) {
            self.open_focus_and_push_center(
                inbox_item_id,
                CenterView::InspectorView {
                    work_item_id,
                    inspector,
                },
            );
        }
    }

    fn open_chat_inspector_for_selected(&mut self) {
        self.open_inspector_for_selected(ArtifactInspectorKind::Chat);
        self.start_supervisor_stream_for_selected();
    }

    fn toggle_global_supervisor_chat(&mut self) {
        if self.is_global_supervisor_chat_active() {
            self.close_global_supervisor_chat();
        } else {
            self.open_global_supervisor_chat();
        }
    }

    fn open_global_supervisor_chat(&mut self) {
        if self.is_global_supervisor_chat_active() {
            self.enter_insert_mode();
            return;
        }

        self.global_supervisor_chat_return_context = Some(GlobalSupervisorChatReturnContext {
            selected_inbox_index: self.selected_inbox_index,
            selected_inbox_item_id: self.selected_inbox_item_id.clone(),
        });
        let _ = self.view_stack.push_center(CenterView::SupervisorChatView);
        self.enter_insert_mode();
    }

    fn close_global_supervisor_chat(&mut self) {
        if !self.is_global_supervisor_chat_active() {
            return;
        }

        if self.is_active_supervisor_stream_visible() {
            self.cancel_supervisor_stream();
        }

        let _ = self.view_stack.pop_center();
        self.enter_normal_mode();

        if let Some(context) = self.global_supervisor_chat_return_context.take() {
            self.selected_inbox_index = context.selected_inbox_index;
            self.selected_inbox_item_id = context.selected_inbox_item_id;
        }
    }

    fn open_focus_and_push_center(&mut self, inbox_item_id: InboxItemId, top_view: CenterView) {
        self.selected_inbox_item_id = Some(inbox_item_id.clone());
        let focus_view = CenterView::FocusCardView { inbox_item_id };
        self.view_stack.replace_center(focus_view);
        self.view_stack.push_center(top_view);
    }

    fn minimize_center_view(&mut self) {
        let _ = self.view_stack.pop_center();
        if !self.is_terminal_view_active() {
            self.enter_normal_mode();
        }
    }

    fn open_ticket_picker(&mut self) {
        if self.ticket_picker_provider.is_none() {
            self.status_warning =
                Some("ticket picker unavailable: no ticket provider configured".to_owned());
            return;
        }

        self.enter_normal_mode();
        self.ticket_picker_overlay.open();
        self.spawn_ticket_picker_load();
    }

    fn close_ticket_picker(&mut self) {
        self.ticket_picker_overlay.close();
    }

    fn begin_create_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.begin_new_ticket_mode();
    }

    fn cancel_create_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.cancel_new_ticket_mode();
    }

    fn append_create_ticket_brief_char(&mut self, ch: char) {
        if !self.ticket_picker_overlay.visible || !self.ticket_picker_overlay.new_ticket_mode {
            return;
        }
        self.ticket_picker_overlay.append_new_ticket_brief_char(ch);
    }

    fn pop_create_ticket_brief_char(&mut self) {
        if !self.ticket_picker_overlay.visible || !self.ticket_picker_overlay.new_ticket_mode {
            return;
        }
        self.ticket_picker_overlay.pop_new_ticket_brief_char();
    }

    fn move_ticket_picker_selection(&mut self, delta: isize) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.move_selection(delta);
    }

    fn fold_ticket_picker_selected_project(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.fold_selected_project();
    }

    fn unfold_ticket_picker_selected_project(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        self.ticket_picker_overlay.unfold_selected_project();
    }

    fn start_selected_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        if self.ticket_picker_overlay.creating || self.ticket_picker_overlay.new_ticket_mode {
            return;
        }
        let Some(ticket) = self.ticket_picker_overlay.selected_ticket().cloned() else {
            return;
        };
        if self.ticket_picker_overlay.starting_ticket_id.is_some() {
            return;
        }
        self.spawn_ticket_picker_start_with_override(ticket, None);
    }

    fn start_selected_ticket_from_picker_with_override(
        &mut self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) {
        if self.ticket_picker_overlay.starting_ticket_id.is_some() {
            return;
        }
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.starting_ticket_id = Some(ticket.ticket_id.clone());
        self.spawn_ticket_picker_start_with_override(ticket, repository_override);
    }

    fn begin_archive_selected_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        if self.ticket_picker_overlay.loading
            || self.ticket_picker_overlay.creating
            || self.ticket_picker_overlay.new_ticket_mode
            || self.ticket_picker_overlay.starting_ticket_id.is_some()
            || self.ticket_picker_overlay.archiving_ticket_id.is_some()
        {
            return;
        }
        let Some(ticket) = self.ticket_picker_overlay.selected_ticket().cloned() else {
            return;
        };
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.archive_confirm_ticket = Some(ticket);
    }

    fn cancel_ticket_picker_archive_confirmation(&mut self) {
        self.ticket_picker_overlay.archive_confirm_ticket = None;
    }

    fn submit_ticket_picker_archive_confirmation(&mut self) {
        let Some(ticket) = self.ticket_picker_overlay.archive_confirm_ticket.clone() else {
            return;
        };
        if self.ticket_picker_overlay.archiving_ticket_id.is_some() {
            return;
        }
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.archiving_ticket_id = Some(ticket.ticket_id.clone());
        self.spawn_ticket_picker_archive(ticket);
    }

    fn submit_ticket_picker_repository_prompt(&mut self) {
        if !self.ticket_picker_overlay.visible {
            return;
        }
        let Some(ticket) = self
            .ticket_picker_overlay
            .repository_prompt_ticket
            .as_ref()
            .cloned()
        else {
            return;
        };
        let repository_path = self.ticket_picker_overlay.repository_prompt_input.text().trim();
        if repository_path.is_empty() {
            self.ticket_picker_overlay.error = Some("repository path cannot be empty".to_owned());
            return;
        }
        let Some(repository_path) = expand_tilde_path(repository_path) else {
            self.ticket_picker_overlay.error =
                Some("could not expand repository path: HOME is not set".to_owned());
            return;
        };
        self.start_selected_ticket_from_picker_with_override(ticket, Some(repository_path));
    }

    fn cancel_ticket_picker_repository_prompt(&mut self) {
        self.ticket_picker_overlay.cancel_repository_prompt();
    }

    fn append_repository_prompt_char(&mut self, ch: char) {
        self.ticket_picker_overlay.repository_prompt_input.insert_char(ch);
    }

    fn pop_repository_prompt_char(&mut self) {
        self.ticket_picker_overlay
            .repository_prompt_input
            .delete_char_backward();
    }

    fn submit_created_ticket_from_picker(&mut self) {
        if !self.ticket_picker_overlay.visible || !self.ticket_picker_overlay.new_ticket_mode {
            return;
        }
        if !self.ticket_picker_overlay.can_submit_new_ticket() {
            self.ticket_picker_overlay.error =
                Some("enter a brief description before creating a ticket".to_owned());
            return;
        }

        let brief = self
            .ticket_picker_overlay
            .new_ticket_brief_input
            .text()
            .trim()
            .to_owned();
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.creating = true;
        self.spawn_ticket_picker_create(brief);
    }

    fn spawn_ticket_picker_load(&mut self) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.ticket_picker_overlay.loading = false;
            self.ticket_picker_overlay.error =
                Some("ticket provider unavailable while loading tickets".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.ticket_picker_overlay.loading = false;
            self.ticket_picker_overlay.error =
                Some("ticket picker event channel unavailable while loading tickets".to_owned());
            return;
        };

        self.ticket_picker_overlay.loading = true;

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_ticket_picker_load_task(provider, sender).await;
                });
            }
            Err(_) => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error =
                    Some("tokio runtime unavailable; cannot load tickets".to_owned());
            }
        }
    }

    fn spawn_ticket_picker_start_with_override(
        &mut self,
        ticket: TicketSummary,
        repository_override: Option<PathBuf>,
    ) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.ticket_picker_overlay.starting_ticket_id = None;
            self.ticket_picker_overlay.error =
                Some("ticket provider unavailable while starting ticket".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.ticket_picker_overlay.starting_ticket_id = None;
            self.ticket_picker_overlay.error =
                Some("ticket picker event channel unavailable while starting ticket".to_owned());
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_ticket_picker_start_task(provider, ticket, repository_override, sender)
                        .await;
                });
            }
            Err(_) => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.error =
                    Some("tokio runtime unavailable; cannot start ticket".to_owned());
            }
        }
    }

    fn spawn_ticket_picker_create(&mut self, brief: String) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.ticket_picker_overlay.creating = false;
            self.ticket_picker_overlay.error =
                Some("ticket provider unavailable while creating ticket".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.ticket_picker_overlay.creating = false;
            self.ticket_picker_overlay.error =
                Some("ticket picker event channel unavailable while creating ticket".to_owned());
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_ticket_picker_create_task(provider, brief, sender).await;
                });
            }
            Err(_) => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_overlay.error =
                    Some("tokio runtime unavailable; cannot create ticket".to_owned());
            }
        }
    }

    fn spawn_ticket_picker_archive(&mut self, ticket: TicketSummary) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.ticket_picker_overlay.archiving_ticket_id = None;
            self.ticket_picker_overlay.error =
                Some("ticket provider unavailable while archiving ticket".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.ticket_picker_overlay.archiving_ticket_id = None;
            self.ticket_picker_overlay.error =
                Some("ticket picker event channel unavailable while archiving ticket".to_owned());
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_ticket_picker_archive_task(provider, ticket, sender).await;
                });
            }
            Err(_) => {
                self.ticket_picker_overlay.archiving_ticket_id = None;
                self.ticket_picker_overlay.error =
                    Some("tokio runtime unavailable; cannot archive ticket".to_owned());
            }
        }
    }

    fn tick_ticket_picker(&mut self) {
        self.poll_ticket_picker_events();
    }

    fn tick_terminal_view(&mut self) {
        self.poll_terminal_session_events();
        self.poll_merge_queue_events();
        self.enqueue_merge_reconcile_polls();
        self.dispatch_merge_queue_requests();
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            self.ensure_terminal_stream(session_id);
        }
    }

    fn poll_terminal_session_events(&mut self) {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.terminal_session_receiver.as_mut() else {
                return;
            };

            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status_warning =
                            Some("terminal stream channel closed unexpectedly".to_owned());
                        break;
                    }
                }
            }
        }

        for event in events {
            match event {
                TerminalSessionEvent::Output { session_id, output } => {
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    append_terminal_assistant_output(view, output.bytes);
                    view.error = None;
                }
                TerminalSessionEvent::TurnState {
                    session_id,
                    turn_state,
                } => {
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    view.turn_active = turn_state.active;
                }
                TerminalSessionEvent::NeedsInput {
                    session_id,
                    needs_input,
                } => {
                    let should_open_for_active_terminal = self
                        .active_terminal_session_id()
                        .map(|active_session_id| active_session_id == &session_id)
                        .unwrap_or(false)
                        && self.is_terminal_view_active();
                    let prompt = self.needs_input_prompt_from_event(needs_input);
                    if self
                        .needs_input_modal
                        .as_ref()
                        .map(|modal| {
                            modal.session_id == session_id && modal.prompt_id == prompt.prompt_id
                        })
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let queue = self
                        .pending_needs_input_prompts
                        .entry(session_id)
                        .or_default();
                    if !queue.iter().any(|entry| entry.prompt_id == prompt.prompt_id) {
                        queue.push_back(prompt);
                    }
                    if should_open_for_active_terminal {
                        self.open_pending_needs_input_modal_for_active_session();
                    }
                }
                TerminalSessionEvent::StreamFailed { session_id, error } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    view.error = Some(error.to_string());
                    view.entries.clear();
                    view.output_fragment.clear();
                    view.output_scroll_line = 0;
                    view.output_follow_tail = true;
                    view.turn_active = false;
                    if let RuntimeError::SessionNotFound(_) = error {
                        self.recover_terminal_session_on_not_found(&session_id);
                    }
                }
                TerminalSessionEvent::StreamEnded { session_id } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    flush_terminal_output_fragment(view);
                    view.turn_active = false;
                }
            }
        }
    }

    fn poll_merge_queue_events(&mut self) {
        let mut events = Vec::new();
        {
            let Some(receiver) = self.merge_event_receiver.as_mut() else {
                return;
            };
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status_warning =
                            Some("merge queue channel closed unexpectedly".to_owned());
                        break;
                    }
                }
            }
        }

        for event in events {
            match event {
                MergeQueueEvent::Completed {
                    session_id,
                    kind,
                    completed,
                    merge_conflict,
                    base_branch,
                    head_branch,
                    error,
                } => {
                    if let Some(error) = error {
                        if kind == MergeQueueCommandKind::Merge {
                            self.merge_pending_sessions.remove(&session_id);
                        }
                        self.status_warning = Some(format!(
                            "workflow merge check failed: {}",
                            sanitize_terminal_display_text(error.as_str())
                        ));
                        continue;
                    }

                    self.clear_merge_status_warning_for_session(&session_id);
                    if kind == MergeQueueCommandKind::Merge || !completed {
                        self.merge_pending_sessions.insert(session_id.clone());
                    }

                    if merge_conflict {
                        let base = base_branch.unwrap_or_else(|| "main".to_owned());
                        let head =
                            head_branch.unwrap_or_else(|| "current feature branch".to_owned());
                        let signature = format!("{head}->{base}");
                        let should_notify = {
                            let view = self
                                .terminal_session_states
                                .entry(session_id.clone())
                                .or_default();
                            if view.last_merge_conflict_signature.as_deref()
                                == Some(signature.as_str())
                            {
                                false
                            } else {
                                view.last_merge_conflict_signature = Some(signature.clone());
                                true
                            }
                        };

                        if should_notify {
                            let instruction = format!(
                                "Merge conflict detected on PR branch '{head}' into '{base}'. Resolve the conflict now: update your branch against '{base}', fix conflicts, push the branch, and report what changed."
                            );
                            self.send_terminal_instruction_to_session(
                                &session_id,
                                instruction.as_str(),
                            );
                        }
                        self.status_warning = Some(format!(
                            "merge conflict for review session {}: resolve conflicts and push updates",
                            session_id.as_str()
                        ));
                    } else if let Some(view) = self.terminal_session_states.get_mut(&session_id) {
                        view.last_merge_conflict_signature = None;
                    }

                    if completed {
                        self.merge_pending_sessions.remove(&session_id);
                        if let Some(session) = self.domain.sessions.get_mut(&session_id) {
                            session.status = Some(WorkerSessionStatus::Done);
                        }
                        if let Some(work_item_id) = self
                            .domain
                            .sessions
                            .get(&session_id)
                            .and_then(|session| session.work_item_id.clone())
                        {
                            if let Some(work_item) = self.domain.work_items.get_mut(&work_item_id) {
                                work_item.workflow_state = Some(WorkflowState::Done);
                            }
                        }
                        self.status_warning = Some(format!(
                            "merge completed for review session {}",
                            session_id.as_str()
                        ));
                        self.spawn_session_merge_finalize(session_id.clone());
                    } else if kind == MergeQueueCommandKind::Merge {
                        self.status_warning = Some(format!(
                            "merge pending for review session {} (waiting for checks or merge queue)",
                            session_id.as_str()
                        ));
                    }
                }
                MergeQueueEvent::SessionFinalized { session_id } => {
                    self.merge_finalizing_sessions.remove(&session_id);
                }
                MergeQueueEvent::SessionFinalizeFailed {
                    session_id,
                    message,
                } => {
                    self.merge_finalizing_sessions.remove(&session_id);
                    self.status_warning = Some(format!(
                        "merged session {} finalized with warnings: {}",
                        session_id.as_str(),
                        compact_focus_card_text(message.as_str())
                    ));
                }
            }
        }
    }

    fn enqueue_merge_reconcile_polls(&mut self) {
        if self.supervisor_command_dispatcher.is_none() {
            return;
        }
        let now = Instant::now();
        if self
            .merge_last_poll_at
            .map(|previous| now.duration_since(previous) < MERGE_POLL_INTERVAL)
            .unwrap_or(false)
        {
            return;
        }
        self.merge_last_poll_at = Some(now);

        let session_ids = self
            .domain
            .sessions
            .iter()
            .filter_map(|(session_id, session)| {
                if !is_open_session_status(session.status.as_ref()) {
                    return None;
                }
                if self.session_is_in_review_stage(session_id) {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let active_review_sessions = session_ids.iter().cloned().collect::<HashSet<_>>();
        self.review_sync_instructions_sent
            .retain(|session_id| active_review_sessions.contains(session_id));
        self.merge_pending_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        self.merge_finalizing_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));

        for session_id in session_ids {
            self.ensure_review_sync_instruction(&session_id);
            self.enqueue_merge_queue_request(session_id, MergeQueueCommandKind::Reconcile);
        }
    }

    fn ensure_review_sync_instruction(&mut self, session_id: &WorkerSessionId) {
        if self.review_sync_instructions_sent.contains(session_id) {
            return;
        }
        self.send_terminal_instruction_to_session(
            session_id,
            "Review-stage sync directive: keep this worktree synced with remote and the PR base branch. Fetch regularly, rebase/merge as needed, and resolve conflicts promptly while merge checks run automatically.",
        );
        self.review_sync_instructions_sent
            .insert(session_id.clone());
    }

    fn dispatch_merge_queue_requests(&mut self) {
        if self.supervisor_command_dispatcher.is_none() {
            return;
        }
        if self.merge_queue.is_empty() {
            return;
        }
        let now = Instant::now();
        if self
            .merge_last_dispatched_at
            .map(|previous| now.duration_since(previous) < MERGE_REQUEST_RATE_LIMIT)
            .unwrap_or(false)
        {
            return;
        }
        let Some(dispatcher) = self.supervisor_command_dispatcher.clone() else {
            return;
        };
        let Some(sender) = self.merge_event_sender.clone() else {
            return;
        };
        let Some(request) = self.merge_queue.pop_front() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(runtime) => {
                self.merge_last_dispatched_at = Some(now);
                runtime.spawn(async move {
                    run_merge_queue_command_task(dispatcher, request, sender).await;
                });
            }
            Err(_) => {
                self.status_warning =
                    Some("workflow merge queue unavailable: tokio runtime unavailable".to_owned());
            }
        }
    }

    fn session_is_in_review_stage(&self, session_id: &WorkerSessionId) -> bool {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.as_ref())
            .map(|state| {
                matches!(
                    state,
                    WorkflowState::AwaitingYourReview
                        | WorkflowState::ReadyForReview
                        | WorkflowState::InReview
                        | WorkflowState::Merging
                )
            })
            .unwrap_or(false)
    }

    fn send_terminal_instruction_to_session(
        &mut self,
        session_id: &WorkerSessionId,
        instruction: &str,
    ) {
        if instruction.trim().is_empty() {
            return;
        }
        if let Some(view) = self.terminal_session_states.get_mut(session_id) {
            append_terminal_system_message(view, instruction);
            view.error = None;
        }

        let Some(backend) = self.worker_backend.clone() else {
            return;
        };
        let Some(handle) = self.terminal_session_handle(session_id) else {
            return;
        };
        let mut payload = instruction.to_owned();
        if !payload.ends_with('\n') {
            payload.push('\n');
        }
        let bytes = payload.into_bytes();

        if let Ok(runtime) = TokioHandle::try_current() {
            runtime.spawn(async move {
                let _ = backend.send_input(&handle, bytes.as_slice()).await;
            });
        }
    }

    fn clear_merge_status_warning_for_session(&mut self, session_id: &WorkerSessionId) {
        let Some(warning) = self.status_warning.as_deref() else {
            return;
        };
        if !warning.contains(session_id.as_str()) {
            return;
        }
        let is_merge_status = warning.starts_with("merge queued for review session")
            || warning.starts_with("merge pending for review session")
            || warning.starts_with("merge completed for review session")
            || warning.starts_with("merge conflict for review session");
        if is_merge_status {
            self.status_warning = None;
        }
    }

    fn enqueue_merge_queue_request(
        &mut self,
        session_id: WorkerSessionId,
        kind: MergeQueueCommandKind,
    ) {
        let Some(context) = self.supervisor_context_for_session(&session_id) else {
            return;
        };
        if self
            .merge_queue
            .iter()
            .any(|queued| queued.session_id == session_id && queued.kind == kind)
        {
            return;
        }
        let request = MergeQueueRequest {
            session_id,
            context,
            kind,
        };
        if kind == MergeQueueCommandKind::Merge {
            self.merge_queue.push_front(request);
        } else {
            self.merge_queue.push_back(request);
        }
    }

    fn supervisor_context_for_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<SupervisorCommandContext> {
        let session = self.domain.sessions.get(session_id)?;
        let work_item_id = session
            .work_item_id
            .as_ref()
            .map(|id| id.as_str().to_owned());
        Some(SupervisorCommandContext {
            selected_work_item_id: work_item_id,
            selected_session_id: Some(session_id.as_str().to_owned()),
            scope: Some(format!("session:{}", session_id.as_str())),
        })
    }

    fn ensure_terminal_stream(&mut self, session_id: WorkerSessionId) {
        if self.terminal_session_streamed.contains(&session_id) {
            return;
        }
        let Some(backend) = self.worker_backend.clone() else {
            self.terminal_session_streamed.remove(&session_id);
            return;
        };
        let Some(sender) = self.terminal_session_sender.clone() else {
            self.terminal_session_streamed.remove(&session_id);
            self.status_warning = Some("terminal stream channel unavailable".to_owned());
            return;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.terminal_session_streamed.remove(&session_id);
            self.status_warning =
                Some("terminal stream unavailable: cannot build session handle".to_owned());
            return;
        };
        self.terminal_session_streamed.insert(session_id.clone());

        match TokioHandle::try_current() {
            Ok(runtime) => {
                let stream_session_id = session_id;
                runtime.spawn(async move {
                    let mut stream = match backend.subscribe(&handle).await {
                        Ok(stream) => stream,
                        Err(error) => {
                            let _ = sender
                                .send(TerminalSessionEvent::StreamFailed {
                                    session_id: stream_session_id,
                                    error,
                                })
                                .await;
                            return;
                        }
                    };

                    loop {
                        match stream.next_event().await {
                            Ok(Some(BackendEvent::Output(output))) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::Output {
                                        session_id: stream_session_id.clone(),
                                        output,
                                    })
                                    .await;
                            }
                            Ok(Some(BackendEvent::TurnState(turn_state))) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::TurnState {
                                        session_id: stream_session_id.clone(),
                                        turn_state,
                                    })
                                    .await;
                            }
                            Ok(Some(BackendEvent::NeedsInput(needs_input))) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::NeedsInput {
                                        session_id: stream_session_id.clone(),
                                        needs_input,
                                    })
                                    .await;
                            }
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::StreamEnded {
                                        session_id: stream_session_id,
                                    })
                                    .await;
                                return;
                            }
                            Err(error) => {
                                let _ = sender
                                    .send(TerminalSessionEvent::StreamFailed {
                                        session_id: stream_session_id,
                                        error,
                                    })
                                    .await;
                                return;
                            }
                        }
                    }
                });
            }
            Err(_) => {
                self.terminal_session_streamed.remove(&session_id);
                self.status_warning =
                    Some("terminal stream unavailable: tokio runtime unavailable".to_owned());
            }
        }
    }

    fn recover_terminal_session_on_not_found(&mut self, session_id: &WorkerSessionId) {
        if self.terminal_session_event_is_stale(session_id) {
            return;
        }

        match self.spawn_manual_terminal_session() {
            Ok(Some(new_session_id)) => {
                let started_session = new_session_id.clone();
                let focus_session = self
                    .inbox_item_id_for_session(session_id)
                    .or_else(|| self.ui_state().selected_inbox_item_id.clone());

                if let Some(inbox_item_id) = focus_session {
                    self.open_focus_and_push_center(
                        inbox_item_id,
                        CenterView::TerminalView {
                            session_id: new_session_id,
                        },
                    );
                } else {
                    let _ = self.view_stack.pop_center();
                    let _ = self.view_stack.push_center(CenterView::TerminalView {
                        session_id: new_session_id,
                    });
                }
                self.ensure_terminal_stream(started_session);
                self.status_warning = Some(format!(
                    "terminal session {} was not found; opened a fresh terminal",
                    session_id.as_str()
                ));
            }
            Ok(None) => {
                self.status_warning = Some(
                    "terminal session unavailable: cannot spawn replacement terminal".to_owned(),
                );
            }
            Err(error) => {
                self.status_warning = Some(format!("terminal replacement failed: {error}"));
            }
        }
    }

    fn terminal_session_event_is_stale(&self, session_id: &WorkerSessionId) -> bool {
        if !self.is_terminal_view_active() {
            return true;
        }

        match self.active_terminal_session_id() {
            Some(active_session_id) => active_session_id != session_id,
            None => true,
        }
    }

    fn poll_ticket_picker_events(&mut self) {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.ticket_picker_receiver.as_mut() else {
                return;
            };

            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status_warning =
                            Some("ticket picker event channel closed unexpectedly".to_owned());
                        break;
                    }
                }
            }
        }

        for event in events {
            self.apply_ticket_picker_event(event);
        }
    }

fn apply_ticket_picker_event(&mut self, event: TicketPickerEvent) {
        match event {
            TicketPickerEvent::TicketsLoaded { tickets, projects } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = None;
                self.ticket_picker_overlay
                    .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
            }
            TicketPickerEvent::TicketsLoadFailed { message } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = Some(message.clone());
                self.status_warning = Some(format!(
                    "ticket picker load warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::SessionWorkflowAdvanced {
                outcome,
                projection,
            } => {
                if let Some(projection) = projection {
                    self.domain = projection;
                } else if let Some(work_item) = self.domain.work_items.get_mut(&outcome.work_item_id)
                {
                    work_item.workflow_state = Some(outcome.to.clone());
                }

                if let Some(instruction) = outcome.instruction.as_deref() {
                    self.send_terminal_instruction_to_session(&outcome.session_id, instruction);
                }

                self.status_warning = Some(format!(
                    "workflow advanced for session {}: {:?} -> {:?}",
                    outcome.session_id.as_str(),
                    outcome.from,
                    outcome.to
                ));
            }
            TicketPickerEvent::SessionWorkflowAdvanceFailed {
                session_id,
                message,
            } => {
                self.status_warning = Some(format!(
                    "workflow advance warning for {}: {}",
                    session_id.as_str(),
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::TicketStarted {
                started_session_id,
                projection,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.cancel_repository_prompt();
                self.ticket_picker_overlay.error = None;
                if let Some(projection) = projection {
                    self.domain = projection;
                }
                if let Some(tickets) = tickets {
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                self.focus_and_stream_session(started_session_id);
                if let Some(message) = warning {
                    self.status_warning = Some(format!(
                        "ticket picker start warning: {}",
                        compact_focus_card_text(message.as_str())
                    ));
                }
            }
            TicketPickerEvent::TicketStartRequiresRepository {
                ticket,
                project_id,
                repository_path_hint,
                message,
            } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.start_repository_prompt(
                    ticket,
                    project_id,
                    repository_path_hint,
                );
                self.ticket_picker_overlay.error = Some(message);
            }
            TicketPickerEvent::TicketStartFailed { message } => {
                self.ticket_picker_overlay.starting_ticket_id = None;
                self.ticket_picker_overlay.cancel_repository_prompt();
                self.ticket_picker_overlay.error = Some(message.clone());
                self.status_warning = Some(format!(
                    "ticket picker start warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::TicketArchived {
                archived_ticket,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.archiving_ticket_id = None;
                self.ticket_picker_overlay.archive_confirm_ticket = None;
                self.ticket_picker_overlay.error = None;
                if let Some(tickets) = tickets {
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                let mut status = format!("archived {}", archived_ticket.identifier);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
            }
            TicketPickerEvent::TicketArchiveFailed {
                ticket,
                message,
                tickets,
            } => {
                self.ticket_picker_overlay.archiving_ticket_id = None;
                self.ticket_picker_overlay.archive_confirm_ticket = None;
                self.ticket_picker_overlay.error = Some(message.clone());
                if let Some(tickets) = tickets {
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                self.status_warning = Some(format!(
                    "ticket picker archive warning ({}): {}",
                    ticket.identifier,
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::TicketCreatedAndStarted {
                created_ticket,
                projection,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_overlay.new_ticket_mode = false;
                self.ticket_picker_overlay.new_ticket_brief_input.clear();
                self.ticket_picker_overlay.error = None;
                if let Some(projection) = projection {
                    self.domain = projection;
                }
                if let Some(tickets) = tickets {
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }

                let mut status = format!("created and started {}", created_ticket.identifier);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
            }
            TicketPickerEvent::TicketCreateFailed {
                message,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_overlay.error = Some(message.clone());
                if let Some(tickets) = tickets {
                    let projects = self.ticket_picker_overlay.project_names();
                    self.ticket_picker_overlay
                        .apply_tickets(tickets, projects, &self.ticket_picker_priority_states);
                }
                let mut warning_parts = vec![compact_focus_card_text(message.as_str())];
                if let Some(extra) = warning {
                    warning_parts.push(compact_focus_card_text(extra.as_str()));
                }
                self.status_warning = Some(format!(
                    "ticket picker create warning: {}",
                    warning_parts.join("; ")
                ));
            }
            TicketPickerEvent::SessionDiffLoaded { diff } => {
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    if modal.session_id == diff.session_id {
                        modal.loading = false;
                        modal.error = None;
                        modal.base_branch = diff.base_branch;
                        modal.content = diff.diff;
                        modal.selected_file_index = 0;
                        modal.selected_hunk_index = 0;
                        let files = parse_diff_file_summaries(modal.content.as_str());
                        modal.cursor_line =
                            files.first().map(|file| file.start_index).unwrap_or(0);
                        modal.scroll =
                            modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
                    }
                }
            }
            TicketPickerEvent::SessionDiffFailed {
                session_id,
                message,
            } => {
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    if modal.session_id == session_id {
                        modal.loading = false;
                        modal.error = Some(message.clone());
                    }
                }
                self.status_warning = Some(format!(
                    "diff load warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::SessionArchived {
                session_id,
                warning,
            } => {
                self.archiving_session_id = None;
                self.archive_session_confirm_session = None;
                if let Some(session) = self.domain.sessions.get_mut(&session_id) {
                    session.status = Some(WorkerSessionStatus::Done);
                }
                self.terminal_session_states.remove(&session_id);
                self.terminal_session_streamed.remove(&session_id);
                self.merge_pending_sessions.remove(&session_id);
                self.merge_finalizing_sessions.remove(&session_id);
                self.review_sync_instructions_sent.remove(&session_id);
                if self.active_terminal_session_id() == Some(&session_id) {
                    let _ = self.view_stack.pop_center();
                }
                let mut status = format!("archived session {}", session_id.as_str());
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
            }
            TicketPickerEvent::SessionArchiveFailed {
                session_id,
                message,
            } => {
                self.archiving_session_id = None;
                self.archive_session_confirm_session = None;
                self.status_warning = Some(format!(
                    "session archive warning ({}): {}",
                    session_id.as_str(),
                    compact_focus_card_text(message.as_str())
                ));
            }
        }
    }

    fn is_ticket_picker_visible(&self) -> bool {
        self.ticket_picker_overlay.visible
    }

    fn submit_global_supervisor_chat_query(&mut self) {
        let query = self.global_supervisor_chat_input.text().trim().to_owned();
        if query.is_empty() {
            let message =
                "supervisor query unavailable: enter a non-empty question before submitting";
            self.status_warning = Some(message.to_owned());
            self.set_supervisor_terminal_state(
                SupervisorStreamTarget::GlobalChatPanel,
                SupervisorResponseState::NoContext,
                message,
            );
            return;
        }

        let started = if let Some(dispatcher) = self.supervisor_command_dispatcher.clone() {
            self.start_supervisor_stream_with_dispatcher_for_global_query(dispatcher, query.clone())
        } else if let Some(provider) = self.supervisor_provider.clone() {
            let request = build_global_supervisor_chat_request(query.as_str());
            self.start_supervisor_stream_with_provider(
                SupervisorStreamTarget::GlobalChatPanel,
                provider,
                request,
            )
        } else {
            let message =
                "supervisor stream unavailable: no LLM provider or command dispatcher configured";
            self.status_warning = Some(message.to_owned());
            self.set_supervisor_terminal_state(
                SupervisorStreamTarget::GlobalChatPanel,
                SupervisorResponseState::BackendUnavailable,
                message,
            );
            false
        };

        if started {
            self.global_supervisor_chat_input.clear();
            self.global_supervisor_chat_last_query = Some(query);
        }
    }

    fn apply_global_chat_insert_key(&mut self, key: KeyEvent) -> bool {
        if !self.is_global_supervisor_chat_active() || self.mode != UiMode::Insert {
            return false;
        }

        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.submit_global_supervisor_chat_query();
                true
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.global_supervisor_chat_input.delete_char_backward();
                true
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.global_supervisor_chat_input.insert_char(ch);
                true
            }
            _ => false,
        }
    }

    fn apply_terminal_compose_key(&mut self, key: KeyEvent) -> bool {
        if self.mode != UiMode::Terminal || !self.is_terminal_view_active() {
            return false;
        }

        match key.code {
            KeyCode::Enter
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
            {
                self.submit_terminal_compose_input();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                self.terminal_compose_input.insert_newline();
                true
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.terminal_compose_input.delete_char_backward();
                true
            }
            KeyCode::Tab if key.modifiers.is_empty() => {
                self.terminal_compose_input.insert_tab();
                true
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.terminal_compose_input.insert_char(ch);
                true
            }
            _ => false,
        }
    }

    fn start_supervisor_stream_for_selected(&mut self) {
        let ui_state = project_ui_state(
            self.base_status.as_str(),
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
            None,
        );
        let Some(selected_row) = ui_state
            .selected_inbox_index
            .and_then(|index| ui_state.inbox_rows.get(index))
            .cloned()
        else {
            self.status_warning = Some(
                "supervisor query unavailable: select an inbox item before opening chat".to_owned(),
            );
            return;
        };

        if let Some(dispatcher) = self.supervisor_command_dispatcher.clone() {
            let _ = self.start_supervisor_stream_with_dispatcher(dispatcher, selected_row);
            return;
        }

        let target = SupervisorStreamTarget::Inspector {
            work_item_id: selected_row.work_item_id.clone(),
        };
        let Some(provider) = self.supervisor_provider.clone() else {
            let message =
                "supervisor stream unavailable: no LLM provider or command dispatcher configured";
            self.status_warning = Some(message.to_owned());
            self.set_supervisor_terminal_state(
                target,
                SupervisorResponseState::BackendUnavailable,
                message,
            );
            return;
        };

        let request = build_supervisor_chat_request(&selected_row, &self.domain);
        let _ = self.start_supervisor_stream_with_provider(target, provider, request);
    }

    fn start_supervisor_stream_with_dispatcher_for_global_query(
        &mut self,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
        query: String,
    ) -> bool {
        let invocation = match CommandRegistry::default().to_untyped_invocation(
            &Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query,
                context: Some(SupervisorQueryContextArgs {
                    selected_work_item_id: None,
                    selected_session_id: None,
                    scope: Some("global".to_owned()),
                }),
            }),
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                let message = format!(
                    "supervisor query unavailable: failed to build command invocation ({error})"
                );
                let state = classify_supervisor_stream_error(message.as_str());
                self.status_warning = Some(message.clone());
                self.set_supervisor_terminal_state(
                    SupervisorStreamTarget::GlobalChatPanel,
                    state,
                    message,
                );
                return false;
            }
        };

        let context = SupervisorCommandContext {
            selected_work_item_id: None,
            selected_session_id: None,
            scope: Some("global".to_owned()),
        };

        self.start_supervisor_stream_with_dispatcher_invocation(
            SupervisorStreamTarget::GlobalChatPanel,
            dispatcher,
            invocation,
            context,
        )
    }

    fn start_supervisor_stream_with_dispatcher(
        &mut self,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
        selected_row: UiInboxRow,
    ) -> bool {
        let invocation = match CommandRegistry::default().to_untyped_invocation(
            &Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query: "What is the current status of this ticket?".to_owned(),
                context: None,
            }),
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                let message = format!(
                    "supervisor query unavailable: failed to build command invocation ({error})"
                );
                let state = classify_supervisor_stream_error(message.as_str());
                self.status_warning = Some(message.clone());
                self.set_supervisor_terminal_state(
                    SupervisorStreamTarget::Inspector {
                        work_item_id: selected_row.work_item_id.clone(),
                    },
                    state,
                    message,
                );
                return false;
            }
        };

        let context = SupervisorCommandContext {
            selected_work_item_id: Some(selected_row.work_item_id.as_str().to_owned()),
            selected_session_id: selected_row
                .session_id
                .as_ref()
                .map(|session_id| session_id.as_str().to_owned()),
            scope: selected_row
                .session_id
                .as_ref()
                .map(|session_id| format!("session:{}", session_id.as_str()))
                .or_else(|| Some(format!("work_item:{}", selected_row.work_item_id.as_str()))),
        };

        self.start_supervisor_stream_with_dispatcher_invocation(
            SupervisorStreamTarget::Inspector {
                work_item_id: selected_row.work_item_id.clone(),
            },
            dispatcher,
            invocation,
            context,
        )
    }

    fn start_supervisor_stream_with_provider(
        &mut self,
        target: SupervisorStreamTarget,
        provider: Arc<dyn LlmProvider>,
        request: LlmChatRequest,
    ) -> bool {
        let Some(handle) = self.supervisor_runtime_handle() else {
            self.set_supervisor_terminal_state(
                target,
                SupervisorResponseState::BackendUnavailable,
                "supervisor stream unavailable: tokio runtime is not active",
            );
            return false;
        };

        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        self.replace_supervisor_stream(target, receiver);
        self.status_warning = None;
        handle.spawn(async move {
            run_supervisor_stream_task(provider, request, sender).await;
        });
        true
    }

    fn start_supervisor_stream_with_dispatcher_invocation(
        &mut self,
        target: SupervisorStreamTarget,
        dispatcher: Arc<dyn SupervisorCommandDispatcher>,
        invocation: UntypedCommandInvocation,
        context: SupervisorCommandContext,
    ) -> bool {
        let Some(handle) = self.supervisor_runtime_handle() else {
            self.set_supervisor_terminal_state(
                target,
                SupervisorResponseState::BackendUnavailable,
                "supervisor stream unavailable: tokio runtime is not active",
            );
            return false;
        };

        let (sender, receiver) = mpsc::channel(SUPERVISOR_STREAM_CHANNEL_CAPACITY);
        self.replace_supervisor_stream(target, receiver);
        self.status_warning = None;
        handle.spawn(async move {
            run_supervisor_command_task(dispatcher, invocation, context, sender).await;
        });
        true
    }

    fn supervisor_runtime_handle(&mut self) -> Option<TokioHandle> {
        match TokioHandle::try_current() {
            Ok(handle) => Some(handle),
            Err(_) => {
                self.status_warning =
                    Some("supervisor stream unavailable: tokio runtime is not active".to_owned());
                None
            }
        }
    }

    fn set_supervisor_terminal_state(
        &mut self,
        target: SupervisorStreamTarget,
        response_state: SupervisorResponseState,
        message: impl Into<String>,
    ) {
        if let Some(previous_stream) = self.supervisor_chat_stream.take() {
            if previous_stream.lifecycle.is_active() {
                if let Some(stream_id) = previous_stream.stream_id {
                    self.spawn_supervisor_cancel(stream_id);
                }
            }
        }
        self.supervisor_chat_stream = Some(ActiveSupervisorChatStream::terminal_state(
            target,
            response_state,
            message,
        ));
    }

    fn replace_supervisor_stream(
        &mut self,
        target: SupervisorStreamTarget,
        receiver: mpsc::Receiver<SupervisorStreamEvent>,
    ) {
        if let Some(previous_stream) = self.supervisor_chat_stream.take() {
            if previous_stream.lifecycle.is_active() {
                if let Some(stream_id) = previous_stream.stream_id {
                    self.spawn_supervisor_cancel(stream_id);
                }
            }
        }
        self.supervisor_chat_stream = Some(ActiveSupervisorChatStream::new(target, receiver));
    }

    fn cancel_supervisor_stream(&mut self) {
        let Some(stream) = self.supervisor_chat_stream.as_mut() else {
            return;
        };
        if !stream.lifecycle.is_active() {
            return;
        }

        stream.lifecycle = SupervisorStreamLifecycle::Cancelling;
        stream.pending_cancel = true;
        let stream_id = stream.stream_id.clone();

        if let Some(stream_id) = stream_id {
            stream.pending_cancel = false;
            self.spawn_supervisor_cancel(stream_id);
        }
    }

    fn spawn_supervisor_cancel(&mut self, stream_id: String) {
        if let Some(dispatcher) = self.supervisor_command_dispatcher.clone() {
            match TokioHandle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = dispatcher
                            .cancel_supervisor_command(stream_id.as_str())
                            .await;
                    });
                }
                Err(_) => {
                    self.status_warning = Some(
                        "supervisor cancel unavailable: tokio runtime is not active".to_owned(),
                    );
                }
            }
            return;
        }

        let Some(provider) = self.supervisor_provider.clone() else {
            self.status_warning =
                Some("supervisor cancel unavailable: no LLM provider configured".to_owned());
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = provider.cancel_stream(stream_id.as_str()).await;
                });
            }
            Err(_) => {
                self.status_warning =
                    Some("supervisor cancel unavailable: tokio runtime is not active".to_owned());
            }
        }
    }

    fn is_active_supervisor_stream_visible(&self) -> bool {
        let Some(stream) = self.supervisor_chat_stream.as_ref() else {
            return false;
        };
        if !stream.lifecycle.is_active() {
            return false;
        }

        match (&stream.target, self.view_stack.active_center()) {
            (
                SupervisorStreamTarget::Inspector {
                    work_item_id: stream_work_item_id,
                },
                Some(CenterView::InspectorView {
                    work_item_id,
                    inspector: ArtifactInspectorKind::Chat,
                }),
            ) => work_item_id == stream_work_item_id,
            (SupervisorStreamTarget::GlobalChatPanel, Some(CenterView::SupervisorChatView)) => true,
            _ => false,
        }
    }

    fn tick_supervisor_stream(&mut self) {
        self.poll_supervisor_stream_events();
        if let Some(stream) = self.supervisor_chat_stream.as_mut() {
            stream.flush_pending_delta();
        }
    }

    fn poll_supervisor_stream_events(&mut self) {
        let mut cancel_stream_id: Option<String> = None;
        let mut warning_message: Option<String> = None;

        {
            let Some(stream) = self.supervisor_chat_stream.as_mut() else {
                return;
            };

            loop {
                match stream.receiver.try_recv() {
                    Ok(SupervisorStreamEvent::Started { stream_id }) => {
                        stream.stream_id = Some(stream_id.clone());
                        if stream.lifecycle != SupervisorStreamLifecycle::Cancelling {
                            stream.lifecycle = SupervisorStreamLifecycle::Streaming;
                        }
                        stream.response_state = SupervisorResponseState::Nominal;
                        stream.state_message = None;
                        stream.cooldown_hint = None;
                        if stream.pending_cancel {
                            stream.pending_cancel = false;
                            cancel_stream_id = Some(stream_id);
                        }
                    }
                    Ok(SupervisorStreamEvent::Delta { text }) => {
                        if !text.is_empty() {
                            stream.pending_delta.push_str(text.as_str());
                            stream.pending_chunk_count += 1;
                        }
                    }
                    Ok(SupervisorStreamEvent::RateLimit { state }) => {
                        let exhausted = state.requests_remaining.is_some_and(|value| value == 0)
                            || state.tokens_remaining.is_some_and(|value| value == 0);
                        let low_headroom = state
                            .tokens_remaining
                            .is_some_and(|value| value <= SUPERVISOR_STREAM_LOW_TOKEN_HEADROOM);
                        stream.last_rate_limit = Some(state.clone());
                        if exhausted {
                            stream.set_response_state(
                                SupervisorResponseState::RateLimited,
                                Some(
                                    "Provider quota is exhausted for the current window."
                                        .to_owned(),
                                ),
                                state
                                    .reset_at
                                    .as_deref()
                                    .map(|reset_at| format!("rate limit reset at {reset_at}")),
                            );
                            warning_message = Some(
                                "supervisor rate limit reached; wait for cooldown before retry"
                                    .to_owned(),
                            );
                        } else if low_headroom
                            && stream.response_state == SupervisorResponseState::Nominal
                        {
                            stream.set_response_state(
                                SupervisorResponseState::HighCost,
                                Some(
                                    "Remaining token headroom is low; tighten follow-up scope."
                                        .to_owned(),
                                ),
                                state
                                    .reset_at
                                    .as_deref()
                                    .map(|reset_at| format!("rate limit reset at {reset_at}")),
                            );
                        }
                    }
                    Ok(SupervisorStreamEvent::Usage { usage }) => {
                        if usage_trips_high_cost_state(&usage)
                            && stream.response_state != SupervisorResponseState::RateLimited
                        {
                            stream.set_response_state(
                                SupervisorResponseState::HighCost,
                                Some(format!(
                                    "Response consumed {} tokens; prefer tighter prompts.",
                                    usage.total_tokens
                                )),
                                None,
                            );
                        }
                        stream.usage = Some(usage);
                    }
                    Ok(SupervisorStreamEvent::Finished { reason, usage }) => {
                        if let Some(usage) = usage {
                            if usage_trips_high_cost_state(&usage)
                                && stream.response_state != SupervisorResponseState::RateLimited
                            {
                                stream.set_response_state(
                                    SupervisorResponseState::HighCost,
                                    Some(format!(
                                        "Response consumed {} tokens; prefer tighter prompts.",
                                        usage.total_tokens
                                    )),
                                    None,
                                );
                            }
                            stream.usage = Some(usage);
                        }
                        stream.lifecycle = match reason {
                            LlmFinishReason::Cancelled => SupervisorStreamLifecycle::Cancelled,
                            LlmFinishReason::Error => SupervisorStreamLifecycle::Error,
                            _ => SupervisorStreamLifecycle::Completed,
                        };
                        if reason == LlmFinishReason::Error {
                            stream.set_response_state(
                                SupervisorResponseState::BackendUnavailable,
                                Some(
                                    "The supervisor stream ended unexpectedly; safe to retry."
                                        .to_owned(),
                                ),
                                None,
                            );
                            warning_message =
                                Some("supervisor stream ended with error finish reason".to_owned());
                        }
                    }
                    Ok(SupervisorStreamEvent::Failed { message }) => {
                        let response_state = classify_supervisor_stream_error(&message);
                        let cooldown_hint =
                            if response_state == SupervisorResponseState::RateLimited {
                                parse_rate_limit_cooldown_hint(message.as_str())
                            } else {
                                None
                            };
                        stream.error_message = Some(message.clone());
                        stream.lifecycle = SupervisorStreamLifecycle::Error;
                        stream.set_response_state(
                            response_state,
                            Some(supervisor_state_message(response_state).to_owned()),
                            cooldown_hint,
                        );
                        warning_message = Some(message);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if stream.lifecycle == SupervisorStreamLifecycle::Connecting
                            || stream.lifecycle == SupervisorStreamLifecycle::Streaming
                            || stream.lifecycle == SupervisorStreamLifecycle::Cancelling
                        {
                            stream.lifecycle = SupervisorStreamLifecycle::Error;
                            stream.set_response_state(
                                SupervisorResponseState::BackendUnavailable,
                                Some(
                                    "Supervisor transport closed unexpectedly; retry is safe."
                                        .to_owned(),
                                ),
                                None,
                            );
                            warning_message =
                                Some("supervisor stream channel closed unexpectedly".to_owned());
                        }
                        break;
                    }
                }
            }
        }

        if let Some(stream_id) = cancel_stream_id {
            self.spawn_supervisor_cancel(stream_id);
        }
        if let Some(message) = warning_message {
            let state = classify_supervisor_stream_error(message.as_str());
            self.status_warning = Some(format!(
                "supervisor {} warning: {}",
                response_state_warning_label(state),
                compact_focus_card_text(message.as_str())
            ));
        }
    }

    fn append_live_supervisor_chat(&self, ui_state: &mut UiState) {
        let Some(stream) = self.supervisor_chat_stream.as_ref() else {
            return;
        };
        match (self.view_stack.active_center(), &stream.target) {
            (
                Some(CenterView::InspectorView {
                    work_item_id,
                    inspector: ArtifactInspectorKind::Chat,
                }),
                SupervisorStreamTarget::Inspector {
                    work_item_id: stream_work_item_id,
                },
            ) if work_item_id == stream_work_item_id => {
                ui_state.center_pane.lines.extend(stream.render_lines());
            }
            (Some(CenterView::SupervisorChatView), SupervisorStreamTarget::GlobalChatPanel) => {
                ui_state.center_pane.lines.extend(stream.render_lines());
            }
            _ => {}
        }
    }

    fn append_global_supervisor_chat_state(&self, ui_state: &mut UiState) {
        if !self.is_global_supervisor_chat_active() {
            return;
        }

        ui_state.center_pane.lines.push(String::new());
        if let Some(query) = self.global_supervisor_chat_last_query.as_deref() {
            ui_state
                .center_pane
                .lines
                .push(format!("Last query: {}", compact_focus_card_text(query)));
        }
        ui_state.center_pane.lines.push(format!(
            "Draft: {}",
            if self.global_supervisor_chat_input.is_empty() {
                "<type in Insert mode>".to_owned()
            } else {
                self.global_supervisor_chat_input.text().to_owned()
            }
        ));
    }

    fn is_global_supervisor_chat_active(&self) -> bool {
        matches!(
            self.view_stack.active_center(),
            Some(CenterView::SupervisorChatView)
        )
    }

    fn set_selection(&mut self, selected_index: Option<usize>, rows: &[UiInboxRow]) {
        let valid_selected_index = selected_index.filter(|index| *index < rows.len());
        self.selected_inbox_index = valid_selected_index;
        self.selected_inbox_item_id =
            valid_selected_index.map(|index| rows[index].inbox_item_id.clone());
    }

    fn enter_normal_mode(&mut self) {
        self.mode = UiMode::Normal;
        self.mode_key_buffer.clear();
        self.which_key_overlay = None;
        self.terminal_escape_pending = false;
    }

    fn enter_insert_mode(&mut self) {
        if !self.is_terminal_view_active() {
            self.mode = UiMode::Insert;
            self.mode_key_buffer.clear();
            self.which_key_overlay = None;
            self.terminal_escape_pending = false;
        }
    }

    fn enter_terminal_mode(&mut self) {
        if self.is_terminal_view_active() {
            self.snap_active_terminal_output_to_bottom();
            self.mode = UiMode::Terminal;
            self.mode_key_buffer.clear();
            self.which_key_overlay = None;
            self.terminal_escape_pending = false;
            self.open_pending_needs_input_modal_for_active_session();
        }
    }

    fn open_terminal_and_enter_mode(&mut self) {
        self.open_terminal_for_selected();
        self.enter_terminal_mode();
    }

    fn needs_input_prompt_from_event(
        &self,
        event: BackendNeedsInputEvent,
    ) -> NeedsInputPromptState {
        let questions = if event.questions.is_empty() {
            vec![BackendNeedsInputQuestion {
                id: event.prompt_id.clone(),
                header: "Input".to_owned(),
                question: event.question,
                is_other: false,
                is_secret: false,
                options: (!event.options.is_empty()).then(|| {
                    event
                        .options
                        .into_iter()
                        .map(|label| orchestrator_runtime::BackendNeedsInputOption {
                            label,
                            description: String::new(),
                        })
                        .collect::<Vec<_>>()
                }),
            }]
        } else {
            event.questions
        };
        NeedsInputPromptState {
            prompt_id: event.prompt_id,
            questions,
        }
    }

    fn open_pending_needs_input_modal_for_active_session(&mut self) {
        if self.needs_input_modal.is_some() {
            return;
        }
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return;
        };
        let Some(queue) = self.pending_needs_input_prompts.get_mut(&session_id) else {
            return;
        };
        let Some(prompt) = queue.pop_front() else {
            return;
        };
        if queue.is_empty() {
            self.pending_needs_input_prompts.remove(&session_id);
        }
        if prompt.questions.is_empty() {
            self.status_warning = Some("input request ignored: prompt had no questions".to_owned());
            return;
        }
        self.needs_input_modal = Some(NeedsInputModalState::new(
            session_id,
            prompt.prompt_id,
            prompt.questions,
        ));
        self.mode = UiMode::Terminal;
        self.terminal_escape_pending = false;
    }

    fn close_needs_input_modal(&mut self) {
        let Some(modal) = self.needs_input_modal.take() else {
            return;
        };
        let queue = self
            .pending_needs_input_prompts
            .entry(modal.session_id)
            .or_default();
        queue.push_front(NeedsInputPromptState {
            prompt_id: modal.prompt_id,
            questions: modal.questions,
        });
    }

    fn complete_needs_input_modal(&mut self) {
        self.needs_input_modal = None;
    }

    fn needs_input_modal_is_note_insert_mode(&self) -> bool {
        self.needs_input_modal
            .as_ref()
            .map(|modal| modal.note_insert_mode)
            .unwrap_or(false)
    }

    fn move_needs_input_question(&mut self, delta: isize) {
        let Some(modal) = self.needs_input_modal.as_mut() else {
            return;
        };
        let current = modal.current_question_index as isize;
        let upper = modal.questions.len().saturating_sub(1) as isize;
        let next = (current + delta).clamp(0, upper) as usize;
        modal.move_to_question(next);
    }

    fn toggle_needs_input_note_insert_mode(&mut self, enabled: bool) {
        let Some(modal) = self.needs_input_modal.as_mut() else {
            return;
        };
        modal.note_insert_mode = enabled;
        modal.note_input_state.focused = enabled;
        modal.select_state.focused = !enabled && modal.current_question_requires_option_selection();
    }

    fn apply_needs_input_note_key(&mut self, key: KeyEvent) -> bool {
        let Some(modal) = self.needs_input_modal.as_mut() else {
            return false;
        };
        if !modal.note_insert_mode {
            return false;
        }

        match key.code {
            KeyCode::Esc => {
                modal.note_insert_mode = false;
                modal.note_input_state.focused = false;
                modal.select_state.focused = modal.current_question_requires_option_selection();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                modal.note_input_state.insert_char('\n');
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL => {
                false
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                modal.note_input_state.delete_char_backward();
                true
            }
            KeyCode::Delete if key.modifiers.is_empty() => {
                modal.note_input_state.delete_char_forward();
                true
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                modal.note_input_state.move_left();
                true
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                modal.note_input_state.move_right();
                true
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                modal.note_input_state.move_home();
                true
            }
            KeyCode::End if key.modifiers.is_empty() => {
                modal.note_input_state.move_end();
                true
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                modal.note_input_state.insert_char(ch);
                true
            }
            _ => true,
        }
    }

    fn submit_needs_input_modal(&mut self) {
        let Some(backend) = self.worker_backend.clone() else {
            if let Some(modal) = self.needs_input_modal.as_mut() {
                modal.error = Some("input response unavailable: no worker backend configured".to_owned());
            }
            return;
        };
        let Some(active_modal) = self.needs_input_modal.as_mut() else {
            return;
        };
        let session_id = active_modal.session_id.clone();
        let prompt_id = active_modal.prompt_id.clone();
        let answers = match active_modal.build_runtime_answers() {
            Ok(answers) => answers,
            Err(error) => {
                active_modal.error = Some(sanitize_terminal_display_text(error.to_string().as_str()));
                return;
            }
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            if let Some(modal) = self.needs_input_modal.as_mut() {
                modal.error = Some(
                    "input response unavailable: cannot resolve backend session handle".to_owned(),
                );
            }
            return;
        };

        match TokioHandle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let _ = backend
                        .respond_to_needs_input(&handle, prompt_id.as_str(), answers.as_slice())
                        .await;
                });
                self.complete_needs_input_modal();
                self.open_pending_needs_input_modal_for_active_session();
            }
            Err(_) => {
                if let Some(modal) = self.needs_input_modal.as_mut() {
                    modal.error =
                        Some("input response unavailable: tokio runtime unavailable".to_owned());
                }
            }
        }
    }

    fn toggle_worktree_diff_modal(&mut self) {
        if self.worktree_diff_modal.is_some() {
            self.close_worktree_diff_modal();
            return;
        }
        self.open_worktree_diff_modal();
    }

    fn close_worktree_diff_modal(&mut self) {
        self.worktree_diff_modal = None;
    }

    fn open_worktree_diff_modal(&mut self) {
        let Some(session_id) = self.selected_session_id_for_terminal_action() else {
            self.status_warning =
                Some("diff unavailable: no active or selected terminal session".to_owned());
            return;
        };
        self.worktree_diff_modal = Some(WorktreeDiffModalState {
            session_id: session_id.clone(),
            base_branch: "main".to_owned(),
            content: String::new(),
            loading: true,
            error: None,
            scroll: 0,
            cursor_line: 0,
            selected_file_index: 0,
            selected_hunk_index: 0,
            focus: DiffPaneFocus::Files,
        });
        self.spawn_session_diff_load(session_id);
    }

    fn scroll_worktree_diff_modal(&mut self, delta: isize) {
        let Some(modal) = self.worktree_diff_modal.as_mut() else {
            return;
        };
        match modal.focus {
            DiffPaneFocus::Files => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                if files.is_empty() {
                    return;
                }
                let max_index = files.len().saturating_sub(1);
                let current = modal.selected_file_index.min(max_index);
                let next = if delta < 0 {
                    current.saturating_sub(delta.unsigned_abs())
                } else {
                    current.saturating_add(delta as usize).min(max_index)
                };
                modal.selected_file_index = next;
                modal.selected_hunk_index = 0;
                modal.cursor_line = files[next].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
            DiffPaneFocus::Diff => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                let Some(file) = files.get(modal.selected_file_index.min(files.len().saturating_sub(1)))
                else {
                    return;
                };
                if file.addition_blocks.is_empty() {
                    return;
                }
                let max_index = file.addition_blocks.len().saturating_sub(1);
                let current = modal.selected_hunk_index.min(max_index);
                let next = if delta < 0 {
                    current.saturating_sub(delta.unsigned_abs())
                } else {
                    current.saturating_add(delta as usize).min(max_index)
                };
                modal.selected_hunk_index = next;
                modal.cursor_line = file.addition_blocks[next].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
        }
    }

    fn jump_worktree_diff_addition_block(&mut self, to_last: bool) {
        let Some(modal) = self.worktree_diff_modal.as_mut() else {
            return;
        };
        match modal.focus {
            DiffPaneFocus::Files => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                if files.is_empty() {
                    modal.cursor_line = if to_last {
                        worktree_diff_modal_line_count(modal).saturating_sub(1)
                    } else {
                        0
                    };
                    modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
                    return;
                }
                modal.selected_file_index = if to_last {
                    files.len().saturating_sub(1)
                } else {
                    0
                };
                modal.selected_hunk_index = 0;
                modal.cursor_line = files[modal.selected_file_index].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
            DiffPaneFocus::Diff => {
                let files = parse_diff_file_summaries(modal.content.as_str());
                let Some(file) = files.get(modal.selected_file_index.min(files.len().saturating_sub(1)))
                else {
                    return;
                };
                if file.addition_blocks.is_empty() {
                    return;
                }
                modal.selected_hunk_index = if to_last {
                    file.addition_blocks.len().saturating_sub(1)
                } else {
                    0
                };
                modal.cursor_line = file.addition_blocks[modal.selected_hunk_index].start_index;
                modal.scroll = modal.cursor_line.saturating_sub(3).min(u16::MAX as usize) as u16;
            }
        }
    }

    fn focus_worktree_diff_files_pane(&mut self) {
        if let Some(modal) = self.worktree_diff_modal.as_mut() {
            modal.focus = DiffPaneFocus::Files;
        }
    }

    fn focus_worktree_diff_detail_pane(&mut self) {
        if let Some(modal) = self.worktree_diff_modal.as_mut() {
            modal.focus = DiffPaneFocus::Diff;
        }
    }

    fn insert_selected_worktree_diff_refs_into_compose(&mut self) {
        let refs = {
            let Some(modal) = self.worktree_diff_modal.as_ref() else {
                return;
            };
            match collect_selected_worktree_diff_refs(modal) {
                Ok(entries) => entries,
                Err(message) => {
                    self.status_warning = Some(message);
                    return;
                }
            }
        };

        if refs.is_empty() {
            self.status_warning = Some("diff selection has no target-side lines".to_owned());
            return;
        }

        let insertion = refs.join(" ");
        let mut current = self.terminal_compose_input.text();
        if !current.is_empty() && !current.ends_with(char::is_whitespace) {
            current.push(' ');
        }
        current.push_str(insertion.as_str());
        if !current.ends_with(char::is_whitespace) {
            current.push(' ');
        }
        self.terminal_compose_input.set_text(current);
        self.status_warning = Some(format!(
            "added {} diff reference{} to compose input",
            refs.len(),
            if refs.len() == 1 { "" } else { "s" }
        ));
    }

    fn spawn_session_diff_load(&mut self, session_id: WorkerSessionId) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            if let Some(modal) = self.worktree_diff_modal.as_mut() {
                modal.loading = false;
                modal.error =
                    Some("diff unavailable: ticket provider is not configured".to_owned());
            }
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            if let Some(modal) = self.worktree_diff_modal.as_mut() {
                modal.loading = false;
                modal.error =
                    Some("diff unavailable: ticket event channel is not available".to_owned());
            }
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_session_diff_load_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    modal.loading = false;
                    modal.error = Some("diff unavailable: tokio runtime is not active".to_owned());
                }
            }
        }
    }

    fn spawn_session_workflow_advance(&mut self, session_id: WorkerSessionId) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.status_warning =
                Some("workflow advance unavailable: ticket provider is not configured".to_owned());
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            self.status_warning = Some(
                "workflow advance unavailable: ticket picker event channel is not available"
                    .to_owned(),
            );
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_session_workflow_advance_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                self.status_warning =
                    Some("workflow advance unavailable: tokio runtime is not active".to_owned());
            }
        }
    }

    fn spawn_session_merge_finalize(&mut self, session_id: WorkerSessionId) {
        if !self.merge_finalizing_sessions.insert(session_id.clone()) {
            return;
        }

        let Some(provider) = self.ticket_picker_provider.clone() else {
            self.merge_finalizing_sessions.remove(&session_id);
            return;
        };
        let Some(sender) = self.merge_event_sender.clone() else {
            self.merge_finalizing_sessions.remove(&session_id);
            self.status_warning = Some(
                "merge finalization unavailable: merge event channel is not available".to_owned(),
            );
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_session_merge_finalize_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                self.merge_finalizing_sessions.remove(&session_id);
                self.status_warning = Some(
                    "merge finalization unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn begin_terminal_escape_chord(&mut self) {
        if self.mode == UiMode::Terminal {
            self.terminal_escape_pending = true;
        }
    }

    fn submit_terminal_compose_input(&mut self) {
        if !self.is_terminal_view_active() {
            return;
        }

        if self.terminal_compose_input.text().trim().is_empty() {
            self.status_warning = Some(
                "terminal input unavailable: compose a non-empty message before sending".to_owned(),
            );
            return;
        }

        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            self.status_warning =
                Some("terminal input unavailable: no active terminal session selected".to_owned());
            return;
        };
        let Some(backend) = self.worker_backend.clone() else {
            self.status_warning =
                Some("terminal input unavailable: no worker backend configured".to_owned());
            return;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.status_warning = Some(
                "terminal input unavailable: cannot resolve backend session handle".to_owned(),
            );
            return;
        };

        let user_message = self.terminal_compose_input.text();
        let mut payload = user_message.clone();
        if !payload.ends_with('\n') {
            payload.push('\n');
        }
        let bytes = payload.into_bytes();

        if backend.kind() != BackendKind::Codex {
            let view = self
                .terminal_session_states
                .entry(session_id.clone())
                .or_default();
            append_terminal_user_message(view, user_message.as_str());
            view.error = None;
        }

        match TokioHandle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let _ = backend.send_input(&handle, bytes.as_slice()).await;
                });
                self.terminal_compose_input.clear();
            }
            Err(_) => {
                self.status_warning =
                    Some("terminal input unavailable: tokio runtime unavailable".to_owned());
            }
        }
    }

    fn advance_terminal_workflow_stage(&mut self) {
        if !self.is_terminal_view_active() {
            self.status_warning =
                Some("workflow advance unavailable: open a terminal session first".to_owned());
            return;
        }

        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            self.status_warning = Some(
                "workflow advance unavailable: no active terminal session selected".to_owned(),
            );
            return;
        };

        let current_state = self
            .domain
            .sessions
            .get(&session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.clone());

        let Some(current_state) = current_state else {
            self.status_warning = Some(format!(
                "workflow advance unavailable: session {} has no canonical workflow state",
                session_id.as_str()
            ));
            return;
        };

        if matches!(
            current_state,
            WorkflowState::AwaitingYourReview
                | WorkflowState::ReadyForReview
                | WorkflowState::InReview
        ) {
            self.review_merge_confirm_session = Some(session_id);
            self.status_warning = None;
            return;
        }

        if matches!(current_state, WorkflowState::Done | WorkflowState::Abandoned) {
            self.status_warning = Some("workflow advance ignored: session is already complete".to_owned());
            return;
        }

        self.status_warning = Some(format!(
            "advancing workflow for session {} from {:?}",
            session_id.as_str(),
            current_state
        ));
        self.spawn_session_workflow_advance(session_id);
    }

    fn is_terminal_view_active(&self) -> bool {
        matches!(
            self.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        )
    }

    fn cancel_review_merge_confirmation(&mut self) {
        self.review_merge_confirm_session = None;
    }

    fn confirm_review_merge(&mut self) {
        let Some(session_id) = self.review_merge_confirm_session.take() else {
            return;
        };
        self.merge_pending_sessions.insert(session_id.clone());
        self.enqueue_merge_queue_request(session_id.clone(), MergeQueueCommandKind::Merge);
        self.status_warning = Some(format!(
            "merge queued for review session {}",
            session_id.as_str()
        ));
    }

    fn refresh_which_key_overlay(&mut self) {
        if self.mode_key_buffer.is_empty() {
            self.which_key_overlay = None;
            return;
        }

        let Some(prefix_hint_view) = self
            .keymap
            .prefix_hints(self.mode, self.mode_key_buffer.as_slice())
        else {
            self.which_key_overlay = None;
            return;
        };

        let hints = prefix_hint_view
            .hints
            .into_iter()
            .map(|hint| WhichKeyHint {
                key: hint.key,
                description: describe_next_key_binding(&hint),
            })
            .collect();

        self.which_key_overlay = Some(WhichKeyOverlayState {
            prefix: self.mode_key_buffer.clone(),
            group_label: prefix_hint_view.label,
            hints,
        });
    }
}

fn terminal_output_line_count_for_scroll(view: &TerminalViewState) -> usize {
    if view.output_rendered_line_count > 0 {
        return view.output_rendered_line_count;
    }
    render_terminal_transcript_entries(view).len()
}
