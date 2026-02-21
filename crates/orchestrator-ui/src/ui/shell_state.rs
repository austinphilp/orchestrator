#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarFocus {
    Sessions,
    Inbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationState {
    None,
    ActiveTurn,
    ResolvedOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingWorkingStatePersist {
    is_working: bool,
    deadline: Instant,
}

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
    ticket_picker_create_refresh_deadline: Option<Instant>,
    ticket_picker_priority_states: Vec<String>,
    startup_session_feed_opened: bool,
    worker_backend: Option<Arc<dyn WorkerBackend>>,
    selected_session_index: Option<usize>,
    selected_session_id: Option<WorkerSessionId>,
    sidebar_focus: SidebarFocus,
    pane_focus: PaneFocus,
    terminal_session_sender: Option<mpsc::Sender<TerminalSessionEvent>>,
    terminal_session_receiver: Option<mpsc::Receiver<TerminalSessionEvent>>,
    session_info_summary_sender: Option<mpsc::Sender<SessionInfoSummaryEvent>>,
    session_info_summary_receiver: Option<mpsc::Receiver<SessionInfoSummaryEvent>>,
    terminal_session_states: HashMap<WorkerSessionId, TerminalViewState>,
    session_info_diff_cache: HashMap<WorkerSessionId, SessionInfoDiffCache>,
    session_info_summary_cache: HashMap<WorkerSessionId, SessionInfoSummaryCache>,
    session_info_summary_deadline: Option<Instant>,
    pending_session_working_state_persists: HashMap<WorkerSessionId, PendingWorkingStatePersist>,
    session_info_summary_last_refresh_at: HashMap<WorkerSessionId, Instant>,
    session_info_diff_last_refresh_at: HashMap<WorkerSessionId, Instant>,
    terminal_session_streamed: HashSet<WorkerSessionId>,
    terminal_compose_editor: EditorState,
    terminal_compose_event_handler: EditorEventHandler,
    session_panel_scroll_line: usize,
    session_panel_viewport_rows: usize,
    session_panel_rendered_line_count: usize,
    archive_session_confirm_session: Option<WorkerSessionId>,
    archiving_session_id: Option<WorkerSessionId>,
    review_merge_confirm_session: Option<WorkerSessionId>,
    merge_queue: VecDeque<MergeQueueRequest>,
    merge_last_dispatched_at: Option<Instant>,
    merge_last_poll_at: Option<Instant>,
    approval_reconcile_last_poll_at: Option<Instant>,
    merge_event_sender: Option<mpsc::Sender<MergeQueueEvent>>,
    merge_event_receiver: Option<mpsc::Receiver<MergeQueueEvent>>,
    review_reconcile_eligible_sessions: HashSet<WorkerSessionId>,
    approval_reconcile_candidate_sessions: HashSet<WorkerSessionId>,
    merge_pending_sessions: HashSet<WorkerSessionId>,
    merge_finalizing_sessions: HashSet<WorkerSessionId>,
    review_sync_instructions_sent: HashSet<WorkerSessionId>,
    session_ci_status_cache: HashMap<WorkerSessionId, SessionCiStatusCache>,
    ci_failure_signatures_notified: HashMap<WorkerSessionId, String>,
    worktree_diff_modal: Option<WorktreeDiffModalState>,
    draw_cache_epoch: u64,
    attention_projection_cache: Option<AttentionProjectionCache>,
    session_panel_rows_cache: Option<SessionPanelRowsCache>,
    event_derived_label_cache: EventDerivedLabelCache,
    full_projection_replacements: u64,
    incremental_domain_event_applies: u64,
    attention_projection_recomputes: u64,
    last_projection_perf_log_at: Option<Instant>,
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
        let (session_info_summary_sender, session_info_summary_receiver) =
            if supervisor_provider.is_some() {
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

        let mut state = Self {
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
            ticket_picker_create_refresh_deadline: None,
            ticket_picker_priority_states: ticket_picker_priority_states_from_env(),
            startup_session_feed_opened: false,
            worker_backend,
            selected_session_index: None,
            selected_session_id: None,
            sidebar_focus: SidebarFocus::Inbox,
            pane_focus: PaneFocus::Left,
            terminal_session_sender,
            terminal_session_receiver,
            session_info_summary_sender,
            session_info_summary_receiver,
            terminal_session_states: HashMap::new(),
            session_info_diff_cache: HashMap::new(),
            session_info_summary_cache: HashMap::new(),
            session_info_summary_deadline: None,
            pending_session_working_state_persists: HashMap::new(),
            session_info_summary_last_refresh_at: HashMap::new(),
            session_info_diff_last_refresh_at: HashMap::new(),
            pending_session_working_state_persists: HashMap::new(),
            terminal_session_streamed: HashSet::new(),
            terminal_compose_editor: insert_mode_editor_state(),
            terminal_compose_event_handler: EditorEventHandler::default(),
            session_panel_scroll_line: 0,
            session_panel_viewport_rows: 1,
            session_panel_rendered_line_count: 0,
            archive_session_confirm_session: None,
            archiving_session_id: None,
            review_merge_confirm_session: None,
            merge_queue: VecDeque::new(),
            merge_last_dispatched_at: None,
            merge_last_poll_at: None,
            approval_reconcile_last_poll_at: None,
            merge_event_sender,
            merge_event_receiver,
            review_reconcile_eligible_sessions: HashSet::new(),
            approval_reconcile_candidate_sessions: HashSet::new(),
            merge_pending_sessions: HashSet::new(),
            merge_finalizing_sessions: HashSet::new(),
            review_sync_instructions_sent: HashSet::new(),
            session_ci_status_cache: HashMap::new(),
            ci_failure_signatures_notified: HashMap::new(),
            worktree_diff_modal: None,
            draw_cache_epoch: 0,
            attention_projection_cache: None,
            session_panel_rows_cache: None,
            event_derived_label_cache: EventDerivedLabelCache::default(),
            full_projection_replacements: 0,
            incremental_domain_event_applies: 0,
            attention_projection_recomputes: 0,
            last_projection_perf_log_at: None,
        };
        state.rebuild_reconcile_eligibility_indexes();
        state
    }

    fn invalidate_draw_caches(&mut self) {
        self.draw_cache_epoch = self.draw_cache_epoch.wrapping_add(1);
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

    fn ui_state_for_draw(&mut self, now: Instant) -> UiState {
        let status = self.status_text();
        let attention_projection = self.attention_projection_for_draw(now).clone();
        let terminal_view_state = None;
        let mut ui_state = project_ui_state_with_attention(
            status.as_str(),
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
            terminal_view_state,
            &attention_projection,
        );
        self.append_global_supervisor_chat_state(&mut ui_state);
        self.append_live_supervisor_chat(&mut ui_state);
        ui_state
    }

    fn attention_projection_for_draw(&mut self, now: Instant) -> &UiAttentionProjection {
        let should_refresh = self
            .attention_projection_cache
            .as_ref()
            .map(|cache| {
                if cache.epoch != self.draw_cache_epoch {
                    return true;
                }
                cache.projection.has_resolved_items
                    && now.duration_since(cache.refreshed_at) >= RESOLVED_ANIMATION_REFRESH_INTERVAL
            })
            .unwrap_or(true);
        if should_refresh {
            self.attention_projection_recomputes =
                self.attention_projection_recomputes.saturating_add(1);
            self.attention_projection_cache = Some(AttentionProjectionCache {
                projection: build_ui_attention_projection(&self.domain),
                refreshed_at: now,
                epoch: self.draw_cache_epoch,
            });
        }
        &self
            .attention_projection_cache
            .as_ref()
            .expect("attention projection cache should be present")
            .projection
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
        if !self.is_left_pane_focused() {
            return;
        }
        if matches!(self.sidebar_focus, SidebarFocus::Sessions) {
            let _ = self.move_session_selection(delta);
            return;
        }
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }

        let current = ui_state.selected_inbox_index.unwrap_or(0) as isize;
        let upper_bound = ui_state.inbox_rows.len() as isize - 1;
        let next = (current + delta).clamp(0, upper_bound) as usize;
        self.set_selection(Some(next), &ui_state.inbox_rows);
    }

    fn jump_to_first_item(&mut self) {
        if !self.is_left_pane_focused() {
            return;
        }
        if matches!(self.sidebar_focus, SidebarFocus::Sessions) {
            let _ = self.move_to_first_session();
            return;
        }
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(0), &ui_state.inbox_rows);
    }

    fn jump_to_last_item(&mut self) {
        if !self.is_left_pane_focused() {
            return;
        }
        if matches!(self.sidebar_focus, SidebarFocus::Sessions) {
            let _ = self.move_to_last_session();
            return;
        }
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(ui_state.inbox_rows.len() - 1), &ui_state.inbox_rows);
    }

    fn cycle_sidebar_focus(&mut self, delta: isize) {
        if !self.is_left_pane_focused() {
            return;
        }
        if delta.rem_euclid(2) == 0 {
            return;
        }
        self.sidebar_focus = match self.sidebar_focus {
            SidebarFocus::Sessions => SidebarFocus::Inbox,
            SidebarFocus::Inbox => SidebarFocus::Sessions,
        };
    }

    fn cycle_pane_focus(&mut self) {
        self.pane_focus = match self.pane_focus {
            PaneFocus::Left => PaneFocus::Right,
            PaneFocus::Right => PaneFocus::Left,
        };
        self.enter_normal_mode();
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

    fn find_next_inbox_session_selection_index(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<(usize, Vec<UiInboxRow>)> {
        let ui_state = self.ui_state();
        let current_index = ui_state
            .inbox_rows
            .iter()
            .position(|row| row.session_id.as_ref() == Some(session_id))?;
        let next_index = ui_state
            .inbox_rows
            .iter()
            .enumerate()
            .skip(current_index + 1)
            .find_map(|(index, row)| row.session_id.as_ref().map(|_| index))?;
        Some((next_index, ui_state.inbox_rows))
    }

    fn auto_advance_inbox_selection_after_workflow_progression(
        &mut self,
        session_id: &WorkerSessionId,
    ) {
        let Some((next_index, rows)) = self.find_next_inbox_session_selection_index(session_id)
        else {
            return;
        };
        self.set_selection(Some(next_index), &rows);
    }

    fn open_terminal_for_selected(&mut self) {
        if self.open_selected_inbox_output(false, false) {
            return;
        }

        let mut terminal_session_id = None;
        if let Some(session_id) = self.selected_session_id_for_panel() {
            self.view_stack.replace_center(CenterView::TerminalView {
                session_id: session_id.clone(),
            });
            terminal_session_id = Some(session_id);
        } else {
            match self.spawn_manual_terminal_session() {
                Ok(Some(session_id)) => {
                    terminal_session_id = Some(session_id.clone());
                    self.view_stack
                        .replace_center(CenterView::TerminalView { session_id });
                }
                Ok(None) => {
                    self.status_warning = Some(
                        "terminal unavailable: no open session is currently selected".to_owned(),
                    );
                }
                Err(error) => {
                    self.status_warning = Some(format!("terminal unavailable: {error}"));
                }
            }
        }

        if let Some(session_id) = terminal_session_id {
            self.ensure_terminal_stream(session_id.clone());
            let _ = self.flush_deferred_terminal_output_for_session(&session_id);
            self.enter_terminal_mode();
            self.schedule_session_info_summary_refresh_for_active_session();
        }
    }

    fn open_session_output_for_selected_inbox(&mut self) {
        let _ = self.open_selected_inbox_output(true, true);
    }

    fn open_selected_inbox_output(
        &mut self,
        acknowledge_selection: bool,
        require_inbox_sidebar_focus: bool,
    ) -> bool {
        if require_inbox_sidebar_focus && !matches!(self.sidebar_focus, SidebarFocus::Inbox) {
            return false;
        }
        let ui_state = self.ui_state();
        let Some(selected_index) = ui_state.selected_inbox_index else {
            self.status_warning =
                Some("session output unavailable: select an inbox item first".to_owned());
            return false;
        };
        let Some(selected_row) = ui_state.inbox_rows.get(selected_index).cloned() else {
            self.status_warning =
                Some("session output unavailable: select an inbox item first".to_owned());
            return false;
        };
        let Some(session_id) = selected_row.session_id.clone() else {
            self.status_warning =
                Some("session output unavailable: selected inbox item has no active session".to_owned());
            return false;
        };

        if let Some(index) = self
            .session_ids_for_navigation()
            .iter()
            .position(|candidate| candidate == &session_id)
        {
            self.selected_session_index = Some(index);
        }
        self.selected_session_id = Some(session_id.clone());
        self.view_stack.replace_center(CenterView::TerminalView {
            session_id: session_id.clone(),
        });
        self.ensure_terminal_stream(session_id.clone());
        let _ = self.flush_deferred_terminal_output_for_session(&session_id);
        self.enter_terminal_mode();
        self.status_warning = None;

        if acknowledge_selection {
            self.acknowledge_inbox_item(selected_row.inbox_item_id, selected_row.work_item_id);
        }

        true
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
        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!("archiving {}", labels.compact_label));
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

    fn session_panel_rows(&self) -> Vec<SessionPanelRow> {
        session_panel_rows(&self.domain, &self.terminal_session_states)
    }

    fn session_panel_rows_for_draw(&mut self) -> Vec<SessionPanelRow> {
        if let Some(cache) = self.session_panel_rows_cache.as_ref() {
            if cache.epoch == self.draw_cache_epoch {
                return cache.rows.clone();
            }
        }

        self.event_derived_label_cache.refresh(&self.domain);
        let rows = session_panel_rows_with_labels(
            &self.domain,
            &self.terminal_session_states,
            &self.event_derived_label_cache.work_item_repo,
            &self.event_derived_label_cache.ticket_labels,
        );
        self.session_panel_rows_cache = Some(SessionPanelRowsCache {
            rows: rows.clone(),
            epoch: self.draw_cache_epoch,
        });
        rows
    }

    fn session_ids_for_navigation(&self) -> Vec<WorkerSessionId> {
        self.session_panel_rows()
            .into_iter()
            .map(|row| row.session_id)
            .collect()
    }

    fn selected_session_id_for_panel(&self) -> Option<WorkerSessionId> {
        let session_ids = self.session_ids_for_navigation();
        self.selected_session_id_for_panel_from_ids(session_ids.as_slice())
    }

    fn selected_session_id_for_panel_from_rows(
        &self,
        rows: &[SessionPanelRow],
    ) -> Option<WorkerSessionId> {
        let session_ids = rows
            .iter()
            .map(|row| row.session_id.clone())
            .collect::<Vec<_>>();
        self.selected_session_id_for_panel_from_ids(session_ids.as_slice())
    }

    fn selected_session_id_for_panel_from_ids(
        &self,
        session_ids: &[WorkerSessionId],
    ) -> Option<WorkerSessionId> {
        if session_ids.is_empty() {
            return None;
        }
        if let Some(selected_session_id) = self.selected_session_id.as_ref() {
            if session_ids
                .iter()
                .any(|candidate| candidate == selected_session_id)
            {
                return Some(selected_session_id.clone());
            }
        }
        let index = self
            .selected_session_index
            .unwrap_or(0)
            .min(session_ids.len() - 1);
        session_ids.get(index).cloned()
    }

    fn sync_selected_session_panel_state(&mut self) {
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            self.selected_session_index = None;
            self.selected_session_id = None;
            return;
        }

        if let Some(selected_session_id) = self.selected_session_id.as_ref() {
            if let Some(index) = session_ids
                .iter()
                .position(|candidate| candidate == selected_session_id)
            {
                self.selected_session_index = Some(index);
                return;
            }
        }

        let index = self
            .selected_session_index
            .unwrap_or(0)
            .min(session_ids.len() - 1);
        self.selected_session_index = Some(index);
        self.selected_session_id = session_ids.get(index).cloned();
    }

    fn is_sessions_sidebar_focused(&self) -> bool {
        self.is_left_pane_focused() && matches!(self.sidebar_focus, SidebarFocus::Sessions)
    }

    fn is_inbox_sidebar_focused(&self) -> bool {
        self.is_left_pane_focused() && matches!(self.sidebar_focus, SidebarFocus::Inbox)
    }

    fn is_left_pane_focused(&self) -> bool {
        matches!(self.pane_focus, PaneFocus::Left)
    }

    fn is_right_pane_focused(&self) -> bool {
        matches!(self.pane_focus, PaneFocus::Right)
    }

    fn should_show_session_info_sidebar(&self) -> bool {
        self.active_terminal_session_id().is_some()
    }

    fn sync_session_panel_viewport(
        &mut self,
        rendered_line_count: usize,
        selected_line: Option<usize>,
        viewport_rows: usize,
    ) {
        self.session_panel_rendered_line_count = rendered_line_count;
        self.session_panel_viewport_rows = viewport_rows.max(1);
        if rendered_line_count == 0 {
            self.session_panel_scroll_line = 0;
            return;
        }

        let max_scroll = rendered_line_count.saturating_sub(self.session_panel_viewport_rows);
        let mut next_scroll = self.session_panel_scroll_line.min(max_scroll);
        if let Some(selected_line) = selected_line {
            if selected_line < next_scroll {
                next_scroll = selected_line;
            } else if selected_line >= next_scroll + self.session_panel_viewport_rows {
                next_scroll = selected_line
                    .saturating_add(1)
                    .saturating_sub(self.session_panel_viewport_rows)
                    .min(max_scroll);
            }
        }
        self.session_panel_scroll_line = next_scroll;
    }

    fn session_panel_scroll_line(&self) -> usize {
        self.session_panel_scroll_line
    }

    fn session_info_is_foreground(&self) -> bool {
        self.active_terminal_session_id().is_some()
            && self.is_right_pane_focused()
            && self.mode == UiMode::Terminal
    }

    fn session_info_diff_cache_for(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<&SessionInfoDiffCache> {
        self.session_info_diff_cache.get(session_id)
    }

    fn session_info_summary_cache_for(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<&SessionInfoSummaryCache> {
        self.session_info_summary_cache.get(session_id)
    }

    fn session_ci_status_cache_for(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<&SessionCiStatusCache> {
        self.session_ci_status_cache.get(session_id)
    }

    fn move_session_selection(&mut self, delta: isize) -> bool {
        self.sync_selected_session_panel_state();
        let session_ids = self.session_ids_for_navigation();
        let len = session_ids.len();
        if len == 0 {
            self.selected_session_index = None;
            self.selected_session_id = None;
            return false;
        }

        let mut index = self.selected_session_index.unwrap_or(0);
        if index >= len {
            index = len - 1;
        }
        let next = (index as isize + delta).rem_euclid(len as isize) as usize;
        self.selected_session_index = Some(next);
        self.selected_session_id = session_ids.get(next).cloned();
        self.show_selected_session_output();
        true
    }

    fn move_to_first_session(&mut self) -> bool {
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            self.selected_session_index = None;
            self.selected_session_id = None;
            false
        } else {
            self.selected_session_index = Some(0);
            self.selected_session_id = session_ids.first().cloned();
            self.show_selected_session_output();
            true
        }
    }

    fn move_to_last_session(&mut self) -> bool {
        let session_ids = self.session_ids_for_navigation();
        let len = session_ids.len();
        if len == 0 {
            self.selected_session_index = None;
            self.selected_session_id = None;
            false
        } else {
            self.selected_session_index = Some(len - 1);
            self.selected_session_id = session_ids.last().cloned();
            self.show_selected_session_output();
            true
        }
    }

    fn show_selected_session_output(&mut self) {
        self.sync_selected_session_panel_state();
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
        self.ensure_terminal_stream(session_id.clone());
        let _ = self.flush_deferred_terminal_output_for_session(&session_id);
        self.schedule_session_info_summary_refresh_for_active_session();
    }

    fn ensure_startup_session_feed_opened_and_report(&mut self) -> bool {
        if self.startup_session_feed_opened {
            return false;
        }
        if self.active_terminal_session_id().is_some() {
            self.startup_session_feed_opened = true;
            return true;
        }
        let session_ids = self.session_ids_for_navigation();
        if session_ids.is_empty() {
            return false;
        }
        self.sync_selected_session_panel_state();
        self.show_selected_session_output();
        self.startup_session_feed_opened = true;
        true
    }

    fn focus_and_stream_session(&mut self, session_id: WorkerSessionId) {
        self.selected_session_id = Some(session_id.clone());
        let session_ids = self.session_ids_for_navigation();
        if let Some(index) = session_ids.iter().position(|candidate| candidate == &session_id) {
            self.selected_session_index = Some(index);
        }

        self.view_stack.replace_center(CenterView::TerminalView {
            session_id: session_id.clone(),
        });

        self.ensure_terminal_stream(session_id.clone());
        let _ = self.flush_deferred_terminal_output_for_session(&session_id);
        self.enter_terminal_mode();
        self.schedule_session_info_summary_refresh_for_active_session();
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

    fn work_item_id_for_session(&self, session_id: &WorkerSessionId) -> Option<WorkItemId> {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.clone())
    }

    fn session_id_for_work_item(&self, work_item_id: &WorkItemId) -> Option<WorkerSessionId> {
        self.domain
            .work_items
            .get(work_item_id)
            .and_then(|work_item| work_item.session_id.clone())
    }

    fn spawn_publish_inbox_item(&mut self, request: InboxPublishRequest) {
        if request.title.trim().is_empty() || request.coalesce_key.trim().is_empty() {
            return;
        }
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_publish_inbox_item_task(provider, request, sender).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "inbox publish unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn spawn_resolve_inbox_item(&mut self, request: InboxResolveRequest) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_resolve_inbox_item_task(provider, request, sender).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "inbox resolution unavailable: tokio runtime is not active".to_owned(),
                );
            }
        }
    }

    fn spawn_set_session_working_state(
        &mut self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) {
        let Some(provider) = self.ticket_picker_provider.clone() else {
            return;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    run_set_session_working_state_task(provider, session_id, is_working).await;
                });
            }
            Err(_) => {
                self.status_warning = Some(
                    "session working-state persistence unavailable: tokio runtime is not active"
                        .to_owned(),
                );
            }
        }
    }

    fn enqueue_or_persist_session_working_state(
        &mut self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) {
        if self.session_is_in_planning_stage(&session_id) {
            self.pending_session_working_state_persists.insert(
                session_id,
                PendingWorkingStatePersist {
                    is_working,
                    deadline: Instant::now() + PLANNING_WORKING_STATE_PERSIST_DEBOUNCE,
                },
            );
            return;
        }

        self.persist_session_working_state_immediately(session_id, is_working);
    }

    fn persist_session_working_state_immediately(
        &mut self,
        session_id: WorkerSessionId,
        is_working: bool,
    ) {
        self.pending_session_working_state_persists
            .remove(&session_id);
        self.spawn_set_session_working_state(session_id, is_working);
    }

    fn flush_due_session_working_state_persists(&mut self) -> bool {
        if self.pending_session_working_state_persists.is_empty() {
            return false;
        }

        let now = Instant::now();
        let due_persists = self
            .pending_session_working_state_persists
            .iter()
            .filter_map(|(session_id, pending)| {
                if now >= pending.deadline || !self.session_is_in_planning_stage(session_id) {
                    Some((session_id.clone(), pending.is_working))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for (session_id, is_working) in due_persists.iter() {
            self.persist_session_working_state_immediately(session_id.clone(), *is_working);
        }

        !due_persists.is_empty()
    }

    fn acknowledge_inbox_item(&mut self, inbox_item_id: InboxItemId, work_item_id: WorkItemId) {
        if let Some(item) = self.domain.inbox_items.get_mut(&inbox_item_id) {
            item.resolved = true;
        }
        self.spawn_resolve_inbox_item(InboxResolveRequest {
            inbox_item_id,
            work_item_id,
        });
    }

    fn acknowledge_needs_decision_for_work_item(&mut self, work_item_id: &WorkItemId) {
        let Some(work_item) = self.domain.work_items.get(work_item_id) else {
            return;
        };
        let to_resolve = work_item
            .inbox_items
            .iter()
            .filter_map(|inbox_item_id| {
                self.domain
                    .inbox_items
                    .get(inbox_item_id)
                    .filter(|item| !item.resolved && item.kind == InboxItemKind::NeedsDecision)
                    .map(|_| inbox_item_id.clone())
            })
            .collect::<Vec<_>>();

        for inbox_item_id in to_resolve {
            if let Some(item) = self.domain.inbox_items.get_mut(&inbox_item_id) {
                item.resolved = true;
            }
            self.spawn_resolve_inbox_item(InboxResolveRequest {
                inbox_item_id,
                work_item_id: work_item_id.clone(),
            });
        }
    }

    fn publish_inbox_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        kind: InboxItemKind,
        title: String,
        coalesce_key: &str,
    ) {
        if !is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) {
            return;
        }

        let Some(work_item_id) = self.work_item_id_for_session(session_id) else {
            return;
        };
        self.spawn_publish_inbox_item(InboxPublishRequest {
            work_item_id,
            session_id: Some(session_id.clone()),
            kind,
            title,
            coalesce_key: coalesce_key.to_owned(),
        });
    }

    fn progression_approval_coalesce_key() -> &'static str {
        "workflow-awaiting-progression"
    }

    fn progression_approval_title_prefix() -> &'static str {
        "Approval needed to progress this ticket"
    }

    fn expected_progression_approval_inbox_id(
        session_id: &WorkerSessionId,
    ) -> InboxItemId {
        InboxItemId::new(format!(
            "inbox-{}-{}",
            session_id.as_str(),
            Self::progression_approval_coalesce_key()
        ))
    }

    fn session_is_actively_working(&self, session_id: &WorkerSessionId) -> bool {
        if self
            .terminal_session_states
            .get(session_id)
            .map(|state| {
                state.active_needs_input.is_some() || !state.pending_needs_input_prompts.is_empty()
            })
            .unwrap_or(false)
        {
            return false;
        }

        if self
            .terminal_session_states
            .get(session_id)
            .map(|state| state.turn_active)
            .unwrap_or(false)
        {
            return true;
        }

        self.domain
            .session_runtime
            .get(session_id)
            .map(|runtime| runtime.is_working)
            .unwrap_or(true)
    }

    fn session_waiting_for_plan_input(&self, session_id: &WorkerSessionId) -> bool {
        if !matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning)
        ) {
            return false;
        }

        if matches!(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
            Some(WorkerSessionStatus::WaitingForUser)
        ) {
            return true;
        }

        self.terminal_session_states
            .get(session_id)
            .map(|state| {
                state.active_needs_input.is_some() || !state.pending_needs_input_prompts.is_empty()
            })
            .unwrap_or(false)
    }

    fn session_requires_progression_approval(&self, session_id: &WorkerSessionId) -> bool {
        if !matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning | WorkflowState::Implementing)
        ) {
            return false;
        }

        if self.session_is_actively_working(session_id) {
            return false;
        }

        !self.session_waiting_for_plan_input(session_id)
    }

    fn session_is_in_planning_stage(&self, session_id: &WorkerSessionId) -> bool {
        matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning)
        )
    }

    fn find_progression_approval_inbox_for_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<(InboxItemId, WorkItemId)> {
        let expected_id = Self::expected_progression_approval_inbox_id(session_id);
        let work_item_id = self.work_item_id_for_session(session_id)?;
        self.domain
            .inbox_items
            .get(&expected_id)
            .filter(|item| item.kind == InboxItemKind::NeedsApproval && !item.resolved)
            .map(|_| (expected_id, work_item_id))
    }

    fn reconcile_progression_approval_inbox_for_session(
        &mut self,
        session_id: &WorkerSessionId,
    ) -> bool {
        if !is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) {
            return false;
        }

        if self.session_requires_progression_approval(session_id) {
            self.publish_inbox_for_session(
                session_id,
                InboxItemKind::NeedsApproval,
                format!(
                    "{}: {}",
                    Self::progression_approval_title_prefix(),
                    session_display_labels(&self.domain, session_id).compact_label
                ),
                Self::progression_approval_coalesce_key(),
            );
            return true;
        }

        if let Some((inbox_item_id, work_item_id)) =
            self.find_progression_approval_inbox_for_session(session_id)
        {
            self.spawn_resolve_inbox_item(InboxResolveRequest {
                inbox_item_id,
                work_item_id,
            });
            return true;
        }

        false
    }

    fn workflow_state_for_session(&self, session_id: &WorkerSessionId) -> Option<WorkflowState> {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.clone())
    }

    fn route_needs_input_inbox_for_session(
        &self,
        session_id: &WorkerSessionId,
    ) -> Option<(InboxItemKind, &'static str, &'static str)> {
        match self.workflow_state_for_session(session_id) {
            Some(WorkflowState::New | WorkflowState::Planning) => Some((
                InboxItemKind::NeedsDecision,
                "plan-input-request",
                "Plan input request",
            )),
            Some(
                WorkflowState::AwaitingYourReview
                | WorkflowState::ReadyForReview
                | WorkflowState::InReview
                | WorkflowState::PendingMerge,
            ) => Some((
                InboxItemKind::ReadyForReview,
                "review-input-request",
                "Review input request",
            )),
            Some(WorkflowState::Done | WorkflowState::Abandoned) => None,
            Some(WorkflowState::Implementing | WorkflowState::PRDrafted) | None => Some((
                InboxItemKind::NeedsApproval,
                Self::progression_approval_coalesce_key(),
                "Worker waiting for progression",
            )),
        }
    }

    fn workflow_transition_inbox_for_state(
        workflow_state: &WorkflowState,
    ) -> Option<(InboxItemKind, &'static str, &'static str)> {
        match workflow_state {
            WorkflowState::PRDrafted => Some((
                InboxItemKind::NeedsApproval,
                Self::progression_approval_coalesce_key(),
                "Approval needed to progress this ticket",
            )),
            WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview
            | WorkflowState::PendingMerge => Some((
                InboxItemKind::ReadyForReview,
                "review-idle",
                "Ticket is idle in review stage",
            )),
            _ => None,
        }
    }

    fn publish_review_idle_inbox_for_session(&mut self, session_id: &WorkerSessionId) {
        if let Some((kind, coalesce_key, title_prefix)) = self
            .workflow_state_for_session(session_id)
            .as_ref()
            .and_then(Self::workflow_transition_inbox_for_state)
            .filter(|(kind, _, _)| *kind == InboxItemKind::ReadyForReview)
        {
            self.publish_inbox_for_session(
                session_id,
                kind,
                format!(
                    "{title_prefix}: {}",
                    session_display_labels(&self.domain, session_id).compact_label
                ),
                coalesce_key,
            );
        }
    }

    fn publish_error_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        source_key: &str,
        message: &str,
    ) {
        self.publish_inbox_for_session(
            session_id,
            InboxItemKind::Blocked,
            format!("Error: {}", compact_focus_card_text(message)),
            format!("error-{source_key}").as_str(),
        );
    }

    fn publish_error_for_work_item(
        &mut self,
        work_item_id: &WorkItemId,
        session_id: Option<WorkerSessionId>,
        source_key: &str,
        message: &str,
    ) {
        self.spawn_publish_inbox_item(InboxPublishRequest {
            work_item_id: work_item_id.clone(),
            session_id,
            kind: InboxItemKind::Blocked,
            title: format!("Error: {}", compact_focus_card_text(message)),
            coalesce_key: format!("error-{source_key}"),
        });
    }

    fn needs_input_summary(event: &BackendNeedsInputEvent) -> String {
        let summary = event
            .questions
            .first()
            .map(|question| question.question.as_str())
            .unwrap_or_else(|| event.question.as_str());
        compact_focus_card_text(summary)
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

    fn terminal_total_rendered_rows_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        width: u16,
        indicator: TerminalActivityIndicator,
    ) -> usize {
        self.terminal_session_states
            .get_mut(session_id)
            .map(|view| terminal_total_rendered_rows(view, width, indicator))
            .unwrap_or(0)
    }

    fn render_terminal_output_viewport_for_session(
        &mut self,
        session_id: &WorkerSessionId,
        request: TerminalViewportRequest,
    ) -> Option<TerminalViewportRender> {
        let view = self.terminal_session_states.get_mut(session_id)?;
        Some(render_terminal_output_viewport(view, request))
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
        if let Some(work_item_id) = ui_state.selected_work_item_id {
            if matches!(self.view_stack.active_center(), Some(CenterView::InspectorView { .. })) {
                let _ = self.view_stack.pop_center();
            }
            let _ = self.view_stack.push_center(CenterView::InspectorView {
                work_item_id,
                inspector,
            });
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

        if !self.view_stack.pop_center() {
            self.view_stack.clear_center();
        }
        self.enter_normal_mode();

        if let Some(context) = self.global_supervisor_chat_return_context.take() {
            self.selected_inbox_index = context.selected_inbox_index;
            self.selected_inbox_item_id = context.selected_inbox_item_id;
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
        self.ticket_picker_create_refresh_deadline = None;
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
        if self.ticket_picker_overlay.new_ticket_mode {
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

    fn submit_created_ticket_from_picker(&mut self, submit_mode: TicketCreateSubmitMode) {
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
            .new_ticket_brief_editor
            .lines
            .to_string()
            .trim()
            .to_owned();
        let selected_project = self.ticket_picker_overlay.selected_project_name();
        self.ticket_picker_overlay.error = None;
        self.ticket_picker_overlay.new_ticket_mode = false;
        clear_editor_state(&mut self.ticket_picker_overlay.new_ticket_brief_editor);
        self.ticket_picker_overlay.creating = true;
        if !self.ticket_picker_overlay.loading {
            self.spawn_ticket_picker_load();
        }
        self.ticket_picker_create_refresh_deadline =
            Some(Instant::now() + TICKET_PICKER_CREATE_REFRESH_INTERVAL);
        self.spawn_ticket_picker_create(CreateTicketFromPickerRequest {
            brief,
            selected_project,
            submit_mode,
        });
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

    fn spawn_ticket_picker_create(&mut self, request: CreateTicketFromPickerRequest) {
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
                    run_ticket_picker_create_task(provider, request, sender).await;
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

    fn tick_ticket_picker_and_report(&mut self) -> bool {
        let mut changed = self.poll_ticket_picker_events();
        changed |= self.tick_ticket_picker_create_refresh();
        changed
    }

    fn tick_ticket_picker_create_refresh(&mut self) -> bool {
        if !self.ticket_picker_overlay.visible || !self.ticket_picker_overlay.creating {
            self.ticket_picker_create_refresh_deadline = None;
            return false;
        }
        if self.ticket_picker_overlay.loading {
            return false;
        }
        let now = Instant::now();
        let deadline = self.ticket_picker_create_refresh_deadline.unwrap_or(now);
        if now < deadline {
            return false;
        }
        self.spawn_ticket_picker_load();
        self.ticket_picker_create_refresh_deadline =
            Some(now + TICKET_PICKER_CREATE_REFRESH_INTERVAL);
        true
    }

    fn filtered_ticket_picker_tickets(&self, tickets: Vec<TicketSummary>) -> Vec<TicketSummary> {
        let active_ticket_ids = self.active_ticket_ids_for_picker();
        tickets
            .into_iter()
            .filter(|ticket| !active_ticket_ids.iter().any(|id| id == &ticket.ticket_id))
            .collect()
    }

    fn active_ticket_ids_for_picker(&self) -> Vec<TicketId> {
        self.domain
            .work_items
            .values()
            .filter_map(|work_item| {
                let ticket_id = work_item.ticket_id.clone()?;
                let session_id = work_item.session_id.as_ref()?;
                let session = self.domain.sessions.get(session_id)?;
                match session.status {
                    Some(
                        WorkerSessionStatus::Running
                        | WorkerSessionStatus::WaitingForUser
                        | WorkerSessionStatus::Blocked,
                    ) => Some(ticket_id),
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
    }

    fn tick_terminal_view_and_report(&mut self) -> bool {
        let mut changed = false;
        changed |= self.poll_terminal_session_events();
        changed |= self.flush_due_session_working_state_persists();
        changed |= self.flush_background_terminal_output_and_report();
        changed |= self.enqueue_progression_approval_reconcile_polls();
        changed |= self.poll_merge_queue_events();
        changed |= self.poll_session_info_summary_events();
        changed |= self.tick_session_info_summary_refresh();
        changed |= self.enqueue_merge_reconcile_polls();
        changed |= self.dispatch_merge_queue_requests();
        changed |= self.ensure_session_info_diff_loaded_for_active_session();
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            changed |= self.flush_deferred_terminal_output_for_session(&session_id);
            changed |= self.ensure_terminal_stream_and_report(session_id);
        }
        changed
    }

    fn flush_deferred_terminal_output_for_session(&mut self, session_id: &WorkerSessionId) -> bool {
        let Some(view) = self.terminal_session_states.get_mut(session_id) else {
            return false;
        };
        if view.deferred_output.is_empty() {
            return false;
        }
        let bytes = std::mem::take(&mut view.deferred_output);
        append_terminal_assistant_output(view, bytes);
        view.last_background_flush_at = Some(Instant::now());
        true
    }

    fn flush_background_terminal_output_and_report(&mut self) -> bool {
        let active_session_id = self.active_terminal_session_id().cloned();
        let now = Instant::now();
        let interval = background_session_refresh_interval_config_value();
        let mut flushed_any = false;

        let session_ids = self
            .terminal_session_states
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for session_id in session_ids {
            if active_session_id.as_ref() == Some(&session_id) {
                continue;
            }
            let should_flush = self
                .terminal_session_states
                .get(&session_id)
                .map(|view| {
                    !view.deferred_output.is_empty()
                        && view
                            .last_background_flush_at
                            .map(|previous| now.duration_since(previous) >= interval)
                            .unwrap_or(true)
                })
                .unwrap_or(false);
            if !should_flush {
                continue;
            }

            let Some(view) = self.terminal_session_states.get_mut(&session_id) else {
                continue;
            };
            let bytes = std::mem::take(&mut view.deferred_output);
            append_terminal_assistant_output(view, bytes);
            view.last_background_flush_at = Some(now);
            flushed_any = true;
        }

        flushed_any
    }

    fn poll_terminal_session_events(&mut self) -> bool {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.terminal_session_receiver.as_mut() else {
                return false;
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

        let had_events = !events.is_empty();
        for event in events {
            match event {
                TerminalSessionEvent::Output { session_id, output } => {
                    let is_active_session = self.active_terminal_session_id() == Some(&session_id);
                    let view = self.terminal_session_states.entry(session_id).or_default();
                    if is_active_session {
                        if !view.deferred_output.is_empty() {
                            let deferred = std::mem::take(&mut view.deferred_output);
                            append_terminal_assistant_output(view, deferred);
                            view.last_background_flush_at = Some(Instant::now());
                        }
                        append_terminal_assistant_output(view, output.bytes);
                    } else {
                        view.deferred_output.extend_from_slice(output.bytes.as_slice());
                        if view.deferred_output.len() >= BACKGROUND_SESSION_DEFERRED_OUTPUT_MAX_BYTES
                        {
                            let deferred = std::mem::take(&mut view.deferred_output);
                            append_terminal_assistant_output(view, deferred);
                            view.last_background_flush_at = Some(Instant::now());
                        } else if view.last_background_flush_at.is_none() {
                            view.last_background_flush_at = Some(Instant::now());
                        }
                    }
                    view.error = None;
                }
                TerminalSessionEvent::TurnState {
                    session_id,
                    turn_state,
                } => {
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    let previous_turn_active = view.turn_active;
                    view.turn_active = turn_state.active;
                    if previous_turn_active != turn_state.active {
                        self.enqueue_or_persist_session_working_state(
                            session_id.clone(),
                            turn_state.active,
                        );
                        if self.active_terminal_session_id() == Some(&session_id) {
                            self.schedule_session_info_summary_refresh_for_active_session();
                        }
                    }
                    self.reconcile_progression_approval_inbox_for_session(&session_id);
                }
                TerminalSessionEvent::NeedsInput {
                    session_id,
                    needs_input,
                } => {
                    let needs_input_summary = Self::needs_input_summary(&needs_input);
                    if let Some((kind, coalesce_key, title_prefix)) =
                        self.route_needs_input_inbox_for_session(&session_id)
                    {
                        self.publish_inbox_for_session(
                            &session_id,
                            kind,
                            format!("{title_prefix}: {needs_input_summary}"),
                            coalesce_key,
                        );
                    }
                    let prompt = self.needs_input_prompt_from_event(&session_id, needs_input);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    view.enqueue_needs_input_prompt(prompt);
                    self.schedule_session_info_summary_refresh_for_active_session();
                    self.reconcile_progression_approval_inbox_for_session(&session_id);
                }
                TerminalSessionEvent::StreamFailed { session_id, error } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    view.error = Some(error.to_string());
                    view.deferred_output.clear();
                    view.last_background_flush_at = None;
                    view.entries.clear();
                    view.output_fragment.clear();
                    view.render_cache.invalidate_all();
                    view.output_rendered_line_count = 0;
                    view.output_scroll_line = 0;
                    view.output_follow_tail = true;
                    view.turn_active = false;
                    self.persist_session_working_state_immediately(session_id.clone(), false);
                    self.publish_error_for_session(
                        &session_id,
                        "terminal-stream",
                        error.to_string().as_str(),
                    );
                    self.reconcile_progression_approval_inbox_for_session(&session_id);
                    if let RuntimeError::SessionNotFound(_) = error {
                        self.recover_terminal_session_on_not_found(&session_id);
                    }
                    if self.active_terminal_session_id() == Some(&session_id) {
                        self.schedule_session_info_summary_refresh_for_active_session();
                    }
                }
                TerminalSessionEvent::StreamEnded { session_id } => {
                    self.terminal_session_streamed.remove(&session_id);
                    let view = self
                        .terminal_session_states
                        .entry(session_id.clone())
                        .or_default();
                    if !view.deferred_output.is_empty() {
                        let deferred = std::mem::take(&mut view.deferred_output);
                        append_terminal_assistant_output(view, deferred);
                    }
                    view.last_background_flush_at = None;
                    flush_terminal_output_fragment(view);
                    view.turn_active = false;
                    self.persist_session_working_state_immediately(session_id.clone(), false);
                    if self.active_terminal_session_id() == Some(&session_id) {
                        self.schedule_session_info_summary_refresh_for_active_session();
                    }
                    self.reconcile_progression_approval_inbox_for_session(&session_id);
                }
            }
        }
        had_events
    }

    fn poll_merge_queue_events(&mut self) -> bool {
        let mut events = Vec::new();
        {
            let Some(receiver) = self.merge_event_receiver.as_mut() else {
                return false;
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

        let had_events = !events.is_empty();
        for event in events {
            match event {
                MergeQueueEvent::Completed {
                    session_id,
                    kind,
                    completed,
                    merge_conflict,
                    base_branch,
                    head_branch,
                    ci_checks,
                    ci_failures,
                    ci_has_failures,
                    ci_status_error,
                    error,
                } => {
                    self.session_ci_status_cache.insert(
                        session_id.clone(),
                        SessionCiStatusCache {
                            checks: ci_checks.clone(),
                            error: ci_status_error.clone(),
                        },
                    );

                    let mut failure_labels = if ci_failures.is_empty() {
                        ci_checks
                            .iter()
                            .filter(|check| check.bucket.eq_ignore_ascii_case("fail"))
                            .map(|check| {
                                let workflow_prefix = check
                                    .workflow
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|workflow| !workflow.is_empty())
                                    .map(|workflow| format!("{workflow} / "))
                                    .unwrap_or_default();
                                format!("{workflow_prefix}{}", check.name)
                            })
                            .collect::<Vec<_>>()
                    } else {
                        ci_failures
                            .iter()
                            .map(|entry| entry.trim())
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    };
                    failure_labels.sort();
                    failure_labels.dedup();

                    if ci_has_failures && !failure_labels.is_empty() {
                        let signature = failure_labels.join("|");
                        let should_notify = self
                            .ci_failure_signatures_notified
                            .get(&session_id)
                            .map(|existing| existing != &signature)
                            .unwrap_or(true);
                        if should_notify && self.session_is_in_review_stage(&session_id) {
                            let failed = failure_labels.join(", ");
                            let instruction = format!(
                                "CI pipeline failure detected while this ticket is in review ({failed}). Investigate failing GitHub Actions checks, implement a fix, push updates, and report what changed. Use CI as the source of truth instead of running the full local build/test suite."
                            );
                            self.send_terminal_instruction_to_session(
                                &session_id,
                                instruction.as_str(),
                            );
                            let labels = session_display_labels(&self.domain, &session_id);
                            self.publish_inbox_for_session(
                                &session_id,
                                InboxItemKind::FYI,
                                format!(
                                    "CI checks failed for {}; harness is fixing: {}",
                                    labels.compact_label, failed
                                ),
                                "ci-pipeline-failure",
                            );
                            self.status_warning = Some(format!(
                                "ci checks failing for {}; harness is fixing",
                                labels.compact_label
                            ));
                        }
                        self.ci_failure_signatures_notified
                            .insert(session_id.clone(), signature);
                    } else {
                        self.ci_failure_signatures_notified.remove(&session_id);
                    }

                    if let Some(error) = error {
                        self.publish_error_for_session(&session_id, "merge-queue", error.as_str());
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
                        let labels = session_display_labels(&self.domain, &session_id);
                        self.status_warning = Some(format!(
                            "merge conflict for review {}: resolve conflicts and push updates",
                            labels.compact_label
                        ));
                    } else if let Some(view) = self.terminal_session_states.get_mut(&session_id) {
                        view.last_merge_conflict_signature = None;
                    }

                    if completed {
                        self.publish_inbox_for_session(
                            &session_id,
                            InboxItemKind::FYI,
                            format!(
                                "Ticket merge completed for {}",
                                session_display_labels(&self.domain, &session_id).compact_label
                            ),
                            "merge-completed",
                        );
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
                        self.refresh_reconcile_eligibility_for_session(&session_id);
                        let labels = session_display_labels(&self.domain, &session_id);
                        self.status_warning = Some(format!(
                            "merge completed for review {}",
                            labels.compact_label
                        ));
                        self.spawn_session_merge_finalize(session_id.clone());
                    } else if kind == MergeQueueCommandKind::Merge {
                        let labels = session_display_labels(&self.domain, &session_id);
                        self.status_warning = Some(format!(
                            "merge pending for review {} (waiting for checks or merge queue)",
                            labels.compact_label
                        ));
                    }
                }
                MergeQueueEvent::SessionFinalized {
                    session_id,
                    event,
                } => {
                    self.apply_domain_event(event);
                    self.merge_finalizing_sessions.remove(&session_id);
                }
                MergeQueueEvent::SessionFinalizeFailed {
                    session_id,
                    message,
                } => {
                    self.merge_finalizing_sessions.remove(&session_id);
                    self.publish_error_for_session(&session_id, "merge-finalize", message.as_str());
                    let labels = session_display_labels(&self.domain, &session_id);
                    self.status_warning = Some(format!(
                        "merged {} finalized with warnings: {}",
                        labels.compact_label,
                        compact_focus_card_text(message.as_str())
                    ));
                }
            }
        }
        had_events
    }

    fn enqueue_merge_reconcile_polls(&mut self) -> bool {
        if self.supervisor_command_dispatcher.is_none() {
            return false;
        }
        let now = Instant::now();
        if self
            .merge_last_poll_at
            .map(|previous| now.duration_since(previous) < MERGE_POLL_INTERVAL)
            .unwrap_or(false)
        {
            return false;
        }
        let mut changed = false;
        self.merge_last_poll_at = Some(now);

        let session_ids = self
            .review_reconcile_eligible_sessions
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let active_review_sessions = session_ids.iter().cloned().collect::<HashSet<_>>();
        let review_len_before = self.review_sync_instructions_sent.len();
        self.review_sync_instructions_sent
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.review_sync_instructions_sent.len() != review_len_before;
        let pending_len_before = self.merge_pending_sessions.len();
        self.merge_pending_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.merge_pending_sessions.len() != pending_len_before;
        let finalizing_len_before = self.merge_finalizing_sessions.len();
        self.merge_finalizing_sessions
            .retain(|session_id| active_review_sessions.contains(session_id));
        changed |= self.merge_finalizing_sessions.len() != finalizing_len_before;
        let ci_cache_len_before = self.session_ci_status_cache.len();
        self.session_ci_status_cache
            .retain(|session_id, _| active_review_sessions.contains(session_id));
        changed |= self.session_ci_status_cache.len() != ci_cache_len_before;
        let ci_notified_len_before = self.ci_failure_signatures_notified.len();
        self.ci_failure_signatures_notified
            .retain(|session_id, _| active_review_sessions.contains(session_id));
        changed |= self.ci_failure_signatures_notified.len() != ci_notified_len_before;

        for session_id in session_ids {
            self.publish_review_idle_inbox_for_session(&session_id);
            changed |= self.ensure_review_sync_instruction(&session_id);
            changed |= self.enqueue_merge_queue_request(session_id, MergeQueueCommandKind::Reconcile);
        }
        changed
    }

    fn enqueue_progression_approval_reconcile_polls(&mut self) -> bool {
        let now = Instant::now();
        if self
            .approval_reconcile_last_poll_at
            .map(|previous| now.duration_since(previous) < APPROVAL_RECONCILE_POLL_INTERVAL)
            .unwrap_or(false)
        {
            return false;
        }
        self.approval_reconcile_last_poll_at = Some(now);

        let session_ids = self
            .approval_reconcile_candidate_sessions
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let mut changed = false;
        for session_id in session_ids {
            changed |= self.reconcile_progression_approval_inbox_for_session(&session_id);
        }
        changed
    }

    fn ensure_review_sync_instruction(&mut self, session_id: &WorkerSessionId) -> bool {
        if self.review_sync_instructions_sent.contains(session_id) {
            return false;
        }
        self.send_terminal_instruction_to_session(
            session_id,
            "Review-stage sync directive: keep this worktree synced with remote and the PR base branch. Fetch regularly, rebase/merge as needed, and resolve conflicts promptly while merge checks run automatically.",
        );
        self.review_sync_instructions_sent
            .insert(session_id.clone());
        true
    }

    fn dispatch_merge_queue_requests(&mut self) -> bool {
        if self.supervisor_command_dispatcher.is_none() {
            return false;
        }
        if self.merge_queue.is_empty() {
            return false;
        }
        let now = Instant::now();
        if self
            .merge_last_dispatched_at
            .map(|previous| now.duration_since(previous) < MERGE_REQUEST_RATE_LIMIT)
            .unwrap_or(false)
        {
            return false;
        }
        let Some(dispatcher) = self.supervisor_command_dispatcher.clone() else {
            return false;
        };
        let Some(sender) = self.merge_event_sender.clone() else {
            return false;
        };
        let Some(request) = self.merge_queue.pop_front() else {
            return false;
        };

        match TokioHandle::try_current() {
            Ok(runtime) => {
                self.merge_last_dispatched_at = Some(now);
                runtime.spawn(async move {
                    run_merge_queue_command_task(dispatcher, request, sender).await;
                });
                true
            }
            Err(_) => {
                self.status_warning =
                    Some("workflow merge queue unavailable: tokio runtime unavailable".to_owned());
                true
            }
        }
    }

    fn session_is_in_review_stage(&self, session_id: &WorkerSessionId) -> bool {
        self.session_is_merge_reconcile_eligible(session_id)
    }

    fn session_is_merge_reconcile_eligible(&self, session_id: &WorkerSessionId) -> bool {
        if !is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) {
            return false;
        }
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
                        | WorkflowState::PendingMerge
                )
            })
            .unwrap_or(false)
    }

    fn session_is_approval_reconcile_candidate(
        &self,
        session_id: &WorkerSessionId,
    ) -> bool {
        is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) && matches!(
            self.workflow_state_for_session(session_id),
            Some(WorkflowState::Planning | WorkflowState::Implementing)
        )
    }

    fn refresh_reconcile_eligibility_for_session(&mut self, session_id: &WorkerSessionId) {
        let review_eligible = is_open_session_status(
            self.domain
                .sessions
                .get(session_id)
                .and_then(|session| session.status.as_ref()),
        ) && self.session_is_in_review_stage(session_id);

        if review_eligible {
            self.review_reconcile_eligible_sessions
                .insert(session_id.clone());
        } else {
            self.review_reconcile_eligible_sessions.remove(session_id);
        }

        if self.session_is_approval_reconcile_candidate(session_id) {
            self.approval_reconcile_candidate_sessions
                .insert(session_id.clone());
        } else {
            self.approval_reconcile_candidate_sessions.remove(session_id);
        }
    }

    fn rebuild_reconcile_eligibility_indexes(&mut self) {
        self.review_reconcile_eligible_sessions.clear();
        self.approval_reconcile_candidate_sessions.clear();

        let session_ids = self.domain.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            self.refresh_reconcile_eligibility_for_session(&session_id);
        }
    }

    fn apply_domain_event(&mut self, event: StoredEventEnvelope) {
        self.incremental_domain_event_applies =
            self.incremental_domain_event_applies.saturating_add(1);
        let event_session_id = event.session_id.clone();
        let event_work_item_id = event.work_item_id.clone();
        apply_event(&mut self.domain, event);
        if let Some(session_id) = event_session_id {
            self.refresh_reconcile_eligibility_for_session(&session_id);
        } else if let Some(work_item_id) = event_work_item_id {
            if let Some(session_id) = self
                .domain
                .work_items
                .get(&work_item_id)
                .and_then(|work_item| work_item.session_id.clone())
            {
                self.refresh_reconcile_eligibility_for_session(&session_id);
            }
        }
    }

    fn replace_domain_projection(&mut self, projection: ProjectionState, reason: &'static str) {
        self.full_projection_replacements = self.full_projection_replacements.saturating_add(1);
        self.domain = projection;
        self.rebuild_reconcile_eligibility_indexes();
        tracing::debug!(
            reason,
            full_projection_replacements = self.full_projection_replacements,
            incremental_domain_event_applies = self.incremental_domain_event_applies,
            attention_projection_recomputes = self.attention_projection_recomputes,
            "ui full projection replacement applied"
        );
    }

    fn maybe_emit_projection_perf_log(&mut self, now: Instant) {
        if self
            .last_projection_perf_log_at
            .map(|previous| now.duration_since(previous) < PROJECTION_PERF_LOG_INTERVAL)
            .unwrap_or(false)
        {
            return;
        }
        self.last_projection_perf_log_at = Some(now);
        tracing::debug!(
            full_projection_replacements = self.full_projection_replacements,
            incremental_domain_event_applies = self.incremental_domain_event_applies,
            attention_projection_recomputes = self.attention_projection_recomputes,
            domain_event_count = self.domain.events.len(),
            "ui projection perf counters"
        );
    }

    fn session_requires_manual_needs_input_activation(&self, session_id: &WorkerSessionId) -> bool {
        self.domain
            .sessions
            .get(session_id)
            .and_then(|session| session.work_item_id.as_ref())
            .and_then(|work_item_id| self.domain.work_items.get(work_item_id))
            .and_then(|work_item| work_item.workflow_state.as_ref())
            .map(|state| matches!(state, WorkflowState::New | WorkflowState::Planning))
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
        let labels = session_display_labels(&self.domain, session_id);
        let fallback = format!("session {}", session_id.as_str());
        if !warning.contains(labels.compact_label.as_str()) && !warning.contains(fallback.as_str()) {
            return;
        }
        let is_merge_status = warning.starts_with("merge queued for review")
            || warning.starts_with("merge pending for review")
            || warning.starts_with("merge completed for review")
            || warning.starts_with("merge conflict for review")
            || warning.starts_with("ci checks failing for");
        if is_merge_status {
            self.status_warning = None;
        }
    }

    fn enqueue_merge_queue_request(
        &mut self,
        session_id: WorkerSessionId,
        kind: MergeQueueCommandKind,
    ) -> bool {
        let Some(context) = self.supervisor_context_for_session(&session_id) else {
            return false;
        };
        if self
            .merge_queue
            .iter()
            .any(|queued| queued.session_id == session_id && queued.kind == kind)
        {
            return false;
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
        true
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
        let _ = self.ensure_terminal_stream_and_report(session_id);
    }

    fn ensure_terminal_stream_and_report(&mut self, session_id: WorkerSessionId) -> bool {
        if self.terminal_session_streamed.contains(&session_id) {
            return false;
        }
        let Some(backend) = self.worker_backend.clone() else {
            self.terminal_session_streamed.remove(&session_id);
            return false;
        };
        let Some(sender) = self.terminal_session_sender.clone() else {
            self.terminal_session_streamed.remove(&session_id);
            self.status_warning = Some("terminal stream channel unavailable".to_owned());
            return true;
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            self.terminal_session_streamed.remove(&session_id);
            self.status_warning =
                Some("terminal stream unavailable: cannot build session handle".to_owned());
            return true;
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
                true
            }
            Err(_) => {
                self.terminal_session_streamed.remove(&session_id);
                self.status_warning =
                    Some("terminal stream unavailable: tokio runtime unavailable".to_owned());
                true
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
                    self.selected_inbox_item_id = Some(inbox_item_id);
                }
                self.view_stack.replace_center(CenterView::TerminalView {
                    session_id: new_session_id,
                });
                self.ensure_terminal_stream(started_session);
                self.enter_terminal_mode();
                let labels = session_display_labels(&self.domain, session_id);
                self.status_warning = Some(format!(
                    "terminal {} was not found; opened a fresh terminal",
                    labels.compact_label
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

    fn poll_ticket_picker_events(&mut self) -> bool {
        let mut events = Vec::new();

        {
            let Some(receiver) = self.ticket_picker_receiver.as_mut() else {
                return false;
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

        let had_events = !events.is_empty();
        for event in events {
            self.apply_ticket_picker_event(event);
        }
        had_events
    }

    fn apply_ticket_picker_event(&mut self, event: TicketPickerEvent) {
        match event {
            TicketPickerEvent::TicketsLoaded { tickets, projects } => {
                self.ticket_picker_overlay.loading = false;
                self.ticket_picker_overlay.error = None;
                let tickets = self.filtered_ticket_picker_tickets(tickets);
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
            } => {
                self.apply_domain_event(outcome.event.clone());

                if let Some(instruction) = outcome.instruction.as_deref() {
                    self.send_terminal_instruction_to_session(&outcome.session_id, instruction);
                }
                if let Some((kind, coalesce_key, title_prefix)) =
                    Self::workflow_transition_inbox_for_state(&outcome.to)
                {
                    self.publish_inbox_for_session(
                        &outcome.session_id,
                        kind,
                        format!(
                            "{title_prefix}: {}",
                            session_display_labels(&self.domain, &outcome.session_id)
                                .compact_label
                        ),
                        coalesce_key,
                    );
                }
                self.reconcile_progression_approval_inbox_for_session(&outcome.session_id);
                self.auto_advance_inbox_selection_after_workflow_progression(&outcome.session_id);

                let labels = session_display_labels(&self.domain, &outcome.session_id);
                self.status_warning = Some(format!(
                    "workflow advanced for {}: {:?} -> {:?}",
                    labels.compact_label,
                    outcome.from,
                    outcome.to
                ));
                self.session_info_diff_cache.remove(&outcome.session_id);
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::SessionWorkflowAdvanceFailed {
                session_id,
                message,
            } => {
                self.publish_error_for_session(&session_id, "workflow-advance", message.as_str());
                let labels = session_display_labels(&self.domain, &session_id);
                self.status_warning = Some(format!(
                    "workflow advance warning for {}: {}",
                    labels.compact_label,
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
                    // Full replacement remains only for ticket-start authoritative reloads.
                    self.replace_domain_projection(projection, "ticket-start");
                }
                if let Some(tickets) = tickets {
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
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
                self.schedule_session_info_summary_refresh_for_active_session();
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
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
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
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
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
            TicketPickerEvent::TicketCreated {
                created_ticket,
                submit_mode,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_create_refresh_deadline = None;
                self.ticket_picker_overlay.error = None;
                let mut display_tickets = tickets
                    .unwrap_or_else(|| self.ticket_picker_overlay.tickets_snapshot());
                display_tickets = self.filtered_ticket_picker_tickets(display_tickets);
                if !display_tickets
                    .iter()
                    .any(|ticket| ticket.ticket_id == created_ticket.ticket_id)
                {
                    display_tickets.push(created_ticket.clone());
                }
                let projects = self.ticket_picker_overlay.project_names();
                self.ticket_picker_overlay.apply_tickets_preferring(
                    display_tickets,
                    projects,
                    &self.ticket_picker_priority_states,
                    Some(&created_ticket.ticket_id),
                );

                let mut status = format!("created {}", created_ticket.identifier);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
                if submit_mode == TicketCreateSubmitMode::CreateAndStart {
                    self.start_selected_ticket_from_picker_with_override(created_ticket, None);
                }
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::TicketCreateFailed {
                message,
                tickets,
                warning,
            } => {
                self.ticket_picker_overlay.creating = false;
                self.ticket_picker_create_refresh_deadline = None;
                self.ticket_picker_overlay.error = Some(message.clone());
                if let Some(tickets) = tickets {
                    let tickets = self.filtered_ticket_picker_tickets(tickets);
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
                let cache = self
                    .session_info_diff_cache
                    .entry(diff.session_id.clone())
                    .or_default();
                cache.loading = false;
                cache.error = None;
                cache.content = diff.diff.clone();
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
                if self.active_terminal_session_id() == Some(&diff.session_id) {
                    self.schedule_session_info_summary_refresh_for_active_session();
                }
            }
            TicketPickerEvent::SessionDiffFailed {
                session_id,
                message,
            } => {
                let cache = self
                    .session_info_diff_cache
                    .entry(session_id.clone())
                    .or_default();
                cache.loading = false;
                cache.error = Some(message.clone());
                if let Some(modal) = self.worktree_diff_modal.as_mut() {
                    if modal.session_id == session_id {
                        modal.loading = false;
                        modal.error = Some(message.clone());
                    }
                }
                self.publish_error_for_session(&session_id, "diff-load", message.as_str());
                self.status_warning = Some(format!(
                    "diff load warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
                if self.active_terminal_session_id() == Some(&session_id) {
                    self.schedule_session_info_summary_refresh_for_active_session();
                }
            }
            TicketPickerEvent::SessionArchived {
                session_id,
                warning,
                event,
            } => {
                self.archiving_session_id = None;
                self.archive_session_confirm_session = None;
                self.apply_domain_event(event);
                self.terminal_session_states.remove(&session_id);
                self.session_info_diff_cache.remove(&session_id);
                self.session_info_summary_cache.remove(&session_id);
                self.session_info_diff_last_refresh_at.remove(&session_id);
                self.session_info_summary_last_refresh_at.remove(&session_id);
                self.terminal_session_streamed.remove(&session_id);
                self.merge_pending_sessions.remove(&session_id);
                self.merge_finalizing_sessions.remove(&session_id);
                self.review_sync_instructions_sent.remove(&session_id);
                if self.active_terminal_session_id() == Some(&session_id) {
                    let _ = self.view_stack.pop_center();
                }
                let labels = session_display_labels(&self.domain, &session_id);
                let mut status = format!("archived {}", labels.compact_label);
                if let Some(message) = warning {
                    status.push_str(": ");
                    status.push_str(compact_focus_card_text(message.as_str()).as_str());
                }
                self.status_warning = Some(status);
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::SessionArchiveFailed {
                session_id,
                message,
            } => {
                self.archiving_session_id = None;
                self.archive_session_confirm_session = None;
                self.publish_error_for_session(&session_id, "session-archive", message.as_str());
                let labels = session_display_labels(&self.domain, &session_id);
                self.status_warning = Some(format!(
                    "session archive warning ({}): {}",
                    labels.compact_label,
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::InboxItemPublished { event } => {
                self.apply_domain_event(event);
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::InboxItemPublishFailed { message } => {
                self.status_warning = Some(format!(
                    "inbox publish warning: {}",
                    compact_focus_card_text(message.as_str())
                ));
            }
            TicketPickerEvent::InboxItemResolved { event } => {
                if let Some(event) = event {
                    self.apply_domain_event(event);
                }
                self.schedule_session_info_summary_refresh_for_active_session();
            }
            TicketPickerEvent::InboxItemResolveFailed { message } => {
                self.status_warning = Some(format!(
                    "inbox resolve warning: {}",
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
        if self.terminal_session_has_any_needs_input() {
            return false;
        }
        if is_ctrl_char(key, '\\') || is_ctrl_char(key, 'n') {
            return false;
        }

        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                if self.terminal_compose_editor.mode == EditorMode::Normal {
                    return false;
                }
                if let Some(key_input) = map_edtui_key_input(key) {
                    self.terminal_compose_event_handler
                        .on_key_event(key_input, &mut self.terminal_compose_editor);
                }
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::CONTROL => {
                self.submit_terminal_compose_input();
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if self.terminal_compose_editor.mode == EditorMode::Normal {
                    self.submit_terminal_compose_input();
                } else {
                    let enter = edtui_key_input(KeyCode::Enter, KeyModifiers::NONE)
                        .expect("enter key conversion");
                    self.terminal_compose_event_handler
                        .on_key_event(enter, &mut self.terminal_compose_editor);
                }
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                let enter = edtui_key_input(KeyCode::Enter, KeyModifiers::NONE)
                    .expect("enter key conversion");
                self.terminal_compose_event_handler
                    .on_key_event(enter, &mut self.terminal_compose_editor);
                true
            }
            _ => {
                if let Some(key_input) = map_edtui_key_input(key) {
                    self.terminal_compose_event_handler
                        .on_key_event(key_input, &mut self.terminal_compose_editor);
                    true
                } else {
                    false
                }
            }
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

    #[cfg(test)]
    fn tick_supervisor_stream(&mut self) {
        let _ = self.tick_supervisor_stream_and_report();
    }

    fn tick_supervisor_stream_and_report(&mut self) -> bool {
        let mut changed = self.poll_supervisor_stream_events();
        if let Some(stream) = self.supervisor_chat_stream.as_mut() {
            changed |= stream.flush_pending_delta();
        }
        changed
    }

    fn poll_supervisor_stream_events(&mut self) -> bool {
        let mut cancel_stream_id: Option<String> = None;
        let mut warning_message: Option<String> = None;
        let mut changed = false;

        {
            let Some(stream) = self.supervisor_chat_stream.as_mut() else {
                return false;
            };

            loop {
                match stream.receiver.try_recv() {
                    Ok(SupervisorStreamEvent::Started { stream_id }) => {
                        changed = true;
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
                            changed = true;
                            stream.pending_delta.push_str(text.as_str());
                            stream.pending_chunk_count += 1;
                        }
                    }
                    Ok(SupervisorStreamEvent::RateLimit { state }) => {
                        changed = true;
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
                        changed = true;
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
                        changed = true;
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
                        changed = true;
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
                            changed = true;
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
            let inspector_context = self.supervisor_chat_stream.as_ref().and_then(|stream| {
                if let SupervisorStreamTarget::Inspector { work_item_id } = &stream.target {
                    Some((work_item_id.clone(), self.session_id_for_work_item(work_item_id)))
                } else {
                    None
                }
            });
            if let Some((work_item_id, session_id)) = inspector_context {
                self.publish_error_for_work_item(
                    &work_item_id,
                    session_id,
                    "supervisor-stream",
                    message.as_str(),
                );
            }
            let state = classify_supervisor_stream_error(message.as_str());
            self.status_warning = Some(format!(
                "supervisor {} warning: {}",
                response_state_warning_label(state),
                compact_focus_card_text(message.as_str())
            ));
            changed = true;
        }
        changed
    }

    #[cfg(test)]
    fn has_active_animated_indicator(&mut self, now: Instant) -> bool {
        !matches!(self.animation_state(now), AnimationState::None)
    }

    fn animation_state(&mut self, now: Instant) -> AnimationState {
        let has_active_terminal_turn = self
            .terminal_session_states
            .iter()
            .any(|(session_id, state)| {
                state.turn_active
                    && matches!(
                        self.domain
                            .sessions
                            .get(session_id)
                            .and_then(|session| session.status.as_ref()),
                        Some(WorkerSessionStatus::Running)
                    )
            });
        if has_active_terminal_turn {
            return AnimationState::ActiveTurn;
        }

        if self.attention_projection_for_draw(now).has_resolved_items {
            AnimationState::ResolvedOnly
        } else {
            AnimationState::None
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
        let previous = self.selected_inbox_item_id.clone();
        let valid_selected_index = selected_index.filter(|index| *index < rows.len());
        self.selected_inbox_index = valid_selected_index;
        self.selected_inbox_item_id =
            valid_selected_index.map(|index| rows[index].inbox_item_id.clone());
        if self.selected_inbox_item_id.is_some() && self.selected_inbox_item_id != previous {
            let _ = self.open_selected_inbox_output(false, false);
        }
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

    fn enter_insert_mode_for_current_focus(&mut self) {
        if self.is_right_pane_focused() && self.is_terminal_view_active() {
            if self.terminal_session_has_any_needs_input() && !self.terminal_session_has_active_needs_input()
            {
                let _ = self.activate_terminal_needs_input(true);
            } else {
                self.enter_terminal_mode();
            }
            return;
        }
        self.enter_insert_mode();
    }

    fn enter_terminal_mode(&mut self) {
        if self.is_terminal_view_active() {
            self.pane_focus = PaneFocus::Right;
            self.snap_active_terminal_output_to_bottom();
            self.terminal_compose_editor.mode = EditorMode::Insert;
            self.mode = UiMode::Terminal;
            self.mode_key_buffer.clear();
            self.which_key_overlay = None;
            self.terminal_escape_pending = false;
            self.schedule_session_info_summary_refresh_for_active_session();
        }
    }

    fn open_terminal_and_enter_mode(&mut self) {
        self.open_terminal_for_selected();
        self.enter_terminal_mode();
    }

    fn needs_input_prompt_from_event(
        &self,
        session_id: &WorkerSessionId,
        event: BackendNeedsInputEvent,
    ) -> NeedsInputPromptState {
        let is_structured_plan_request = !event.questions.is_empty();
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
            requires_manual_activation: self
                .session_requires_manual_needs_input_activation(session_id),
            is_structured_plan_request,
        }
    }

    fn active_terminal_needs_input(&self) -> Option<&NeedsInputComposerState> {
        let session_id = self.active_terminal_session_id()?;
        self.terminal_session_states
            .get(session_id)
            .and_then(|view| view.active_needs_input.as_ref())
    }

    fn active_terminal_needs_input_mut(&mut self) -> Option<&mut NeedsInputComposerState> {
        let session_id = self.active_terminal_session_id()?.clone();
        self.terminal_session_states
            .get_mut(&session_id)
            .and_then(|view| view.active_needs_input.as_mut())
    }

    fn terminal_session_has_active_needs_input(&self) -> bool {
        self.active_terminal_needs_input()
            .map(|prompt| prompt.interaction_active)
            .unwrap_or(false)
    }

    fn terminal_session_has_any_needs_input(&self) -> bool {
        self.active_terminal_needs_input().is_some()
    }

    fn active_terminal_session_requires_manual_needs_input_activation(&self) -> bool {
        self.active_terminal_session_id()
            .map(|session_id| self.session_requires_manual_needs_input_activation(session_id))
            .unwrap_or(false)
    }

    fn deactivate_terminal_needs_input_interaction(&mut self) -> bool {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return false;
        };
        if !prompt.interaction_active {
            return false;
        }
        prompt.persist_current_draft();
        prompt.set_interaction_active(false);
        true
    }

    fn activate_terminal_needs_input(&mut self, enable_note_insert_mode: bool) -> bool {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return false;
        };
        if prompt.interaction_active {
            return false;
        }
        prompt.set_interaction_active(true);
        if enable_note_insert_mode {
            prompt.note_insert_mode = true;
            prompt.note_editor_state.mode = EditorMode::Insert;
            prompt.select_state.focused = false;
        } else {
            prompt.note_insert_mode = false;
            prompt.note_editor_state.mode = EditorMode::Normal;
            prompt.select_state.focused = prompt.current_question_requires_option_selection();
        }
        true
    }

    fn terminal_needs_input_is_note_insert_mode(&self) -> bool {
        self.active_terminal_needs_input()
            .map(|prompt| prompt.note_insert_mode)
            .unwrap_or(false)
    }

    fn move_terminal_needs_input_question(&mut self, delta: isize) {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return;
        };
        let current = prompt.current_question_index as isize;
        let upper = prompt.questions.len().saturating_sub(1) as isize;
        let next = (current + delta).clamp(0, upper) as usize;
        prompt.move_to_question(next);
    }

    fn toggle_terminal_needs_input_note_insert_mode(&mut self, enabled: bool) {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return;
        };
        if !prompt.interaction_active {
            return;
        }
        prompt.note_insert_mode = enabled;
        prompt.note_editor_state.mode = if enabled {
            EditorMode::Insert
        } else {
            EditorMode::Normal
        };
        prompt.select_state.focused = !enabled && prompt.current_question_requires_option_selection();
    }

    fn apply_terminal_needs_input_note_key(&mut self, key: KeyEvent) -> bool {
        let Some(prompt) = self.active_terminal_needs_input_mut() else {
            return false;
        };
        if !prompt.interaction_active || !prompt.note_insert_mode {
            return false;
        }

        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                prompt.note_insert_mode = false;
                prompt.note_editor_state.mode = EditorMode::Normal;
                prompt.select_state.focused = prompt.current_question_requires_option_selection();
                true
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                let enter = edtui_key_input(KeyCode::Enter, KeyModifiers::NONE)
                    .expect("enter key conversion");
                EditorEventHandler::default().on_key_event(enter, &mut prompt.note_editor_state);
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL => {
                false
            }
            _ => {
                if let Some(key_input) = map_edtui_key_input(key) {
                    EditorEventHandler::default()
                        .on_key_event(key_input, &mut prompt.note_editor_state);
                }
                true
            }
        }
    }

    fn submit_terminal_needs_input_response(&mut self) {
        let Some(backend) = self.worker_backend.clone() else {
            if let Some(prompt) = self.active_terminal_needs_input_mut() {
                prompt.error = Some("input response unavailable: no worker backend configured".to_owned());
            }
            return;
        };
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return;
        };
        let Some(active_prompt) = self
            .terminal_session_states
            .get_mut(&session_id)
            .and_then(|view| view.active_needs_input.as_mut())
        else {
            return;
        };
        let prompt_id = active_prompt.prompt_id.clone();
        let answers = match active_prompt.build_runtime_answers() {
            Ok(answers) => answers,
            Err(error) => {
                active_prompt.error = Some(sanitize_terminal_display_text(error.to_string().as_str()));
                return;
            }
        };
        let Some(handle) = self.terminal_session_handle(&session_id) else {
            if let Some(prompt) = self.active_terminal_needs_input_mut() {
                prompt.error = Some(
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
                if let Some(view) = self.terminal_session_states.get_mut(&session_id) {
                    view.complete_active_needs_input_prompt();
                }
                if let Some(work_item_id) = self.work_item_id_for_session(&session_id) {
                    self.acknowledge_needs_decision_for_work_item(&work_item_id);
                }
                self.reconcile_progression_approval_inbox_for_session(&session_id);
            }
            Err(_) => {
                if let Some(prompt) = self.active_terminal_needs_input_mut() {
                    prompt.error =
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
        let mut current = editor_state_text(&self.terminal_compose_editor);
        if !current.is_empty() && !current.ends_with(char::is_whitespace) {
            current.push(' ');
        }
        current.push_str(insertion.as_str());
        if !current.ends_with(char::is_whitespace) {
            current.push(' ');
        }
        set_editor_state_text(&mut self.terminal_compose_editor, current.as_str());
        self.status_warning = Some(format!(
            "added {} diff reference{} to compose input",
            refs.len(),
            if refs.len() == 1 { "" } else { "s" }
        ));
    }

    fn schedule_session_info_summary_refresh_for_active_session(&mut self) {
        if !self.should_show_session_info_sidebar() {
            self.session_info_summary_deadline = None;
            return;
        }
        if let Some(session_id) = self.active_terminal_session_id().cloned() {
            let now = Instant::now();
            let deadline = if self.session_info_is_foreground() {
                now + Duration::from_millis(500)
            } else {
                let interval = session_info_background_refresh_interval_config_value();
                self.session_info_summary_last_refresh_at
                    .get(&session_id)
                    .map(|previous| (*previous + interval).max(now))
                    .unwrap_or(now + interval)
            };
            self.session_info_summary_deadline = Some(
                self.session_info_summary_deadline
                    .map(|existing| existing.min(deadline))
                    .unwrap_or(deadline),
            );
            let cache = self.session_info_summary_cache.entry(session_id).or_default();
            cache.loading = true;
            cache.error = None;
        }
    }

    fn poll_session_info_summary_events(&mut self) -> bool {
        let mut events = Vec::new();
        {
            let Some(receiver) = self.session_info_summary_receiver.as_mut() else {
                return false;
            };
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }

        let changed = !events.is_empty();
        for event in events {
            match event {
                SessionInfoSummaryEvent::Completed {
                    session_id,
                    summary,
                    context_fingerprint,
                } => {
                    let cache = self.session_info_summary_cache.entry(session_id).or_default();
                    if cache.context_fingerprint.as_deref() != Some(context_fingerprint.as_str()) {
                        continue;
                    }
                    cache.loading = false;
                    cache.error = None;
                    cache.text = Some(summary);
                }
                SessionInfoSummaryEvent::Failed {
                    session_id,
                    message,
                    context_fingerprint,
                } => {
                    let cache = self.session_info_summary_cache.entry(session_id).or_default();
                    if cache.context_fingerprint.as_deref() != Some(context_fingerprint.as_str()) {
                        continue;
                    }
                    cache.loading = false;
                    cache.error = Some(message);
                    if cache.text.is_none() {
                        cache.text = None;
                    }
                }
            }
        }
        changed
    }

    fn tick_session_info_summary_refresh(&mut self) -> bool {
        let Some(deadline) = self.session_info_summary_deadline else {
            return false;
        };
        let now = Instant::now();
        if now < deadline {
            return false;
        }
        if !self.session_info_is_foreground() {
            let Some(session_id) = self.active_terminal_session_id().cloned() else {
                self.session_info_summary_deadline = None;
                return false;
            };
            let interval = session_info_background_refresh_interval_config_value();
            if let Some(previous) = self.session_info_summary_last_refresh_at.get(&session_id) {
                if now.duration_since(*previous) < interval {
                    self.session_info_summary_deadline = Some(*previous + interval);
                    return false;
                }
            }
        }
        self.session_info_summary_deadline = None;
        self.spawn_active_session_summary_refresh()
    }

    fn spawn_active_session_summary_refresh(&mut self) -> bool {
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return false;
        };
        let Some(provider) = self.supervisor_provider.clone() else {
            let cache = self.session_info_summary_cache.entry(session_id).or_default();
            cache.loading = false;
            cache.error = Some("no LLM provider configured".to_owned());
            return true;
        };
        let diff_cache = self.session_info_diff_cache.get(&session_id);
        let Some(context) = session_info_summary_prompt(&self.domain, &session_id, diff_cache) else {
            return false;
        };

        let cache = self
            .session_info_summary_cache
            .entry(context.session_id.clone())
            .or_default();
        if cache.context_fingerprint.as_deref() == Some(context.context_fingerprint.as_str())
            && cache.text.is_some()
            && cache.error.is_none()
        {
            cache.loading = false;
            return false;
        }
        cache.context_fingerprint = Some(context.context_fingerprint.clone());
        cache.loading = true;
        cache.error = None;

        let Some(sender) = self.session_info_summary_sender.clone() else {
            cache.loading = false;
            cache.error = Some("summary channel unavailable".to_owned());
            return true;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                self.session_info_summary_last_refresh_at
                    .insert(context.session_id.clone(), Instant::now());
                handle.spawn(async move {
                    run_session_info_summary_task(provider, context, sender).await;
                });
            }
            Err(_) => {
                cache.loading = false;
                cache.error = Some("tokio runtime unavailable".to_owned());
            }
        }
        true
    }

    fn ensure_session_info_diff_loaded_for_active_session(&mut self) -> bool {
        let Some(session_id) = self.active_terminal_session_id().cloned() else {
            return false;
        };
        let needs_load = self
            .session_info_diff_cache
            .get(&session_id)
            .map(|entry| entry.content.is_empty() && !entry.loading)
            .unwrap_or(true);
        if !needs_load {
            return false;
        }
        if !self.session_info_is_foreground() {
            let now = Instant::now();
            let interval = session_info_background_refresh_interval_config_value();
            match self.session_info_diff_last_refresh_at.get(&session_id).copied() {
                Some(previous) => {
                    if now.duration_since(previous) < interval {
                        return false;
                    }
                }
                None => {
                    self.session_info_diff_last_refresh_at
                        .insert(session_id.clone(), now);
                    return false;
                }
            }
        }
        self.spawn_session_info_diff_load(session_id)
    }

    fn spawn_session_info_diff_load(&mut self, session_id: WorkerSessionId) -> bool {
        let cache = self
            .session_info_diff_cache
            .entry(session_id.clone())
            .or_default();
        cache.loading = true;
        cache.error = None;
        if cache.content.is_empty() {
            cache.content = String::new();
        }
        let Some(provider) = self.ticket_picker_provider.clone() else {
            cache.loading = false;
            cache.error = Some("ticket provider unavailable".to_owned());
            return true;
        };
        let Some(sender) = self.ticket_picker_sender.clone() else {
            cache.loading = false;
            cache.error = Some("ticket event channel unavailable".to_owned());
            return true;
        };

        match TokioHandle::try_current() {
            Ok(handle) => {
                self.session_info_diff_last_refresh_at
                    .insert(session_id.clone(), Instant::now());
                handle.spawn(async move {
                    run_session_diff_load_task(provider, session_id, sender).await;
                });
            }
            Err(_) => {
                cache.loading = false;
                cache.error = Some("tokio runtime unavailable".to_owned());
            }
        }
        true
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

        if editor_state_text(&self.terminal_compose_editor).trim().is_empty() {
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

        let user_message = editor_state_text(&self.terminal_compose_editor);
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
                clear_editor_state(&mut self.terminal_compose_editor);
                self.enter_normal_mode();
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
            let labels = session_display_labels(&self.domain, &session_id);
            self.status_warning = Some(format!(
                "workflow advance unavailable: {} has no canonical workflow state",
                labels.compact_label
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

        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!(
            "advancing workflow for {} from {:?}",
            labels.compact_label,
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
        let _ = self.enqueue_merge_queue_request(session_id.clone(), MergeQueueCommandKind::Merge);
        let labels = session_display_labels(&self.domain, &session_id);
        self.status_warning = Some(format!(
            "merge queued for review {}",
            labels.compact_label
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

fn editor_state_text(state: &EditorState) -> String {
    state.lines.to_string()
}

fn set_editor_state_text(state: &mut EditorState, text: &str) {
    *state = insert_mode_editor_state_with_text(text);
}

fn clear_editor_state(state: &mut EditorState) {
    *state = insert_mode_editor_state();
}

fn insert_mode_editor_state() -> EditorState {
    insert_mode_editor_state_with_text("")
}

fn insert_mode_editor_state_with_text(text: &str) -> EditorState {
    let mut state = EditorState::new(Lines::from(text));
    state.mode = EditorMode::Insert;
    state
}

fn map_edtui_key_input(key: KeyEvent) -> Option<edtui::events::KeyInput> {
    edtui_key_input(key.code, key.modifiers)
}

fn edtui_key_input(code: KeyCode, modifiers: KeyModifiers) -> Option<edtui::events::KeyInput> {
    let code = match code {
        KeyCode::Char(ch) => ratatui::crossterm::event::KeyCode::Char(ch),
        KeyCode::Enter => ratatui::crossterm::event::KeyCode::Enter,
        KeyCode::Esc => ratatui::crossterm::event::KeyCode::Esc,
        KeyCode::Backspace => ratatui::crossterm::event::KeyCode::Backspace,
        KeyCode::Delete => ratatui::crossterm::event::KeyCode::Delete,
        KeyCode::Tab => ratatui::crossterm::event::KeyCode::Tab,
        KeyCode::Left => ratatui::crossterm::event::KeyCode::Left,
        KeyCode::Right => ratatui::crossterm::event::KeyCode::Right,
        KeyCode::Up => ratatui::crossterm::event::KeyCode::Up,
        KeyCode::Down => ratatui::crossterm::event::KeyCode::Down,
        KeyCode::Home => ratatui::crossterm::event::KeyCode::Home,
        KeyCode::End => ratatui::crossterm::event::KeyCode::End,
        _ => return None,
    };
    let mut mapped_modifiers = ratatui::crossterm::event::KeyModifiers::empty();
    if modifiers.contains(KeyModifiers::SHIFT) {
        mapped_modifiers |= ratatui::crossterm::event::KeyModifiers::SHIFT;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        mapped_modifiers |= ratatui::crossterm::event::KeyModifiers::CONTROL;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        mapped_modifiers |= ratatui::crossterm::event::KeyModifiers::ALT;
    }
    Some(edtui::events::KeyInput::with_modifiers(
        code,
        mapped_modifiers,
    ))
}
