#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiCommand {
    EnterNormalMode,
    EnterInsertMode,
    ToggleGlobalSupervisorChat,
    OpenTerminalForSelected,
    OpenDiffInspectorForSelected,
    OpenTestInspectorForSelected,
    OpenPrInspectorForSelected,
    OpenChatInspectorForSelected,
    StartTerminalEscapeChord,
    QuitShell,
    FocusNextInbox,
    FocusPreviousInbox,
    CycleSidebarFocusNext,
    CycleSidebarFocusPrevious,
    CycleBatchNext,
    CycleBatchPrevious,
    JumpFirstInbox,
    JumpLastInbox,
    JumpBatchDecideOrUnblock,
    JumpBatchApprovals,
    JumpBatchReviewReady,
    JumpBatchFyiDigest,
    OpenTicketPicker,
    CloseTicketPicker,
    TicketPickerMoveNext,
    TicketPickerMovePrevious,
    TicketPickerFoldProject,
    TicketPickerUnfoldProject,
    TicketPickerStartSelected,
    SetApplicationModeAutopilot,
    SetApplicationModeManual,
    ToggleWorkflowProfilesModal,
    ToggleWorktreeDiffModal,
    AdvanceTerminalWorkflowStage,
    ArchiveSelectedSession,
    OpenSessionOutputForSelectedInbox,
}

impl UiCommand {
    const ALL: [Self; 36] = [
        Self::EnterNormalMode,
        Self::EnterInsertMode,
        Self::ToggleGlobalSupervisorChat,
        Self::OpenTerminalForSelected,
        Self::OpenDiffInspectorForSelected,
        Self::OpenTestInspectorForSelected,
        Self::OpenPrInspectorForSelected,
        Self::OpenChatInspectorForSelected,
        Self::StartTerminalEscapeChord,
        Self::QuitShell,
        Self::FocusNextInbox,
        Self::FocusPreviousInbox,
        Self::CycleSidebarFocusNext,
        Self::CycleSidebarFocusPrevious,
        Self::CycleBatchNext,
        Self::CycleBatchPrevious,
        Self::JumpFirstInbox,
        Self::JumpLastInbox,
        Self::JumpBatchDecideOrUnblock,
        Self::JumpBatchApprovals,
        Self::JumpBatchReviewReady,
        Self::JumpBatchFyiDigest,
        Self::OpenTicketPicker,
        Self::CloseTicketPicker,
        Self::TicketPickerMoveNext,
        Self::TicketPickerMovePrevious,
        Self::TicketPickerFoldProject,
        Self::TicketPickerUnfoldProject,
        Self::TicketPickerStartSelected,
        Self::SetApplicationModeAutopilot,
        Self::SetApplicationModeManual,
        Self::ToggleWorkflowProfilesModal,
        Self::ToggleWorktreeDiffModal,
        Self::AdvanceTerminalWorkflowStage,
        Self::ArchiveSelectedSession,
        Self::OpenSessionOutputForSelectedInbox,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "ui.mode.normal",
            Self::EnterInsertMode => "ui.mode.insert",
            Self::ToggleGlobalSupervisorChat => "ui.supervisor_chat.toggle",
            Self::OpenTerminalForSelected => command_ids::UI_OPEN_TERMINAL_FOR_SELECTED,
            Self::OpenDiffInspectorForSelected => command_ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
            Self::OpenTestInspectorForSelected => command_ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
            Self::OpenPrInspectorForSelected => command_ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
            Self::OpenChatInspectorForSelected => command_ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
            Self::StartTerminalEscapeChord => "ui.mode.terminal_escape_prefix",
            Self::QuitShell => "ui.shell.quit",
            Self::FocusNextInbox => command_ids::UI_FOCUS_NEXT_INBOX,
            Self::FocusPreviousInbox => "ui.focus_previous_inbox",
            Self::CycleSidebarFocusNext => "ui.sidebar.focus_next",
            Self::CycleSidebarFocusPrevious => "ui.sidebar.focus_previous",
            Self::CycleBatchNext => "ui.cycle_batch_next",
            Self::CycleBatchPrevious => "ui.cycle_batch_previous",
            Self::JumpFirstInbox => "ui.jump_first_inbox",
            Self::JumpLastInbox => "ui.jump_last_inbox",
            Self::JumpBatchDecideOrUnblock => "ui.jump_batch.decide_or_unblock",
            Self::JumpBatchApprovals => "ui.jump_batch.approvals",
            Self::JumpBatchReviewReady => "ui.jump_batch.review_ready",
            Self::JumpBatchFyiDigest => "ui.jump_batch.fyi_digest",
            Self::OpenTicketPicker => "ui.ticket_picker.open",
            Self::CloseTicketPicker => "ui.ticket_picker.close",
            Self::TicketPickerMoveNext => "ui.ticket_picker.move_next",
            Self::TicketPickerMovePrevious => "ui.ticket_picker.move_previous",
            Self::TicketPickerFoldProject => "ui.ticket_picker.fold_project",
            Self::TicketPickerUnfoldProject => "ui.ticket_picker.unfold_project",
            Self::TicketPickerStartSelected => "ui.ticket_picker.start_selected",
            Self::SetApplicationModeAutopilot => "ui.mode.autopilot",
            Self::SetApplicationModeManual => "ui.mode.manual",
            Self::ToggleWorkflowProfilesModal => "ui.workflow_profiles.toggle_modal",
            Self::ToggleWorktreeDiffModal => "ui.worktree.diff.toggle",
            Self::AdvanceTerminalWorkflowStage => "ui.terminal.workflow.advance",
            Self::ArchiveSelectedSession => "ui.terminal.archive_selected_session",
            Self::OpenSessionOutputForSelectedInbox => "ui.open_session_output_for_selected_inbox",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::EnterNormalMode => "Return to Normal mode",
            Self::EnterInsertMode => "Enter Insert mode",
            Self::ToggleGlobalSupervisorChat => "Toggle global supervisor chat panel",
            Self::OpenTerminalForSelected => "Open terminal for selected item",
            Self::OpenDiffInspectorForSelected => "Open diff inspector for selected item",
            Self::OpenTestInspectorForSelected => "Open test inspector for selected item",
            Self::OpenPrInspectorForSelected => "Open PR inspector for selected item",
            Self::OpenChatInspectorForSelected => "Open chat inspector for selected item",
            Self::StartTerminalEscapeChord => "Terminal escape chord (Ctrl-\\ Ctrl-n)",
            Self::QuitShell => "Quit shell",
            Self::FocusNextInbox => "Focus next inbox item",
            Self::FocusPreviousInbox => "Focus previous inbox item",
            Self::CycleSidebarFocusNext => "Focus next sidebar panel",
            Self::CycleSidebarFocusPrevious => "Focus previous sidebar panel",
            Self::CycleBatchNext => "Cycle to next inbox lane",
            Self::CycleBatchPrevious => "Cycle to previous inbox lane",
            Self::JumpFirstInbox => "Jump to first inbox item",
            Self::JumpLastInbox => "Jump to last inbox item",
            Self::JumpBatchDecideOrUnblock => "Jump to Decide/Unblock lane",
            Self::JumpBatchApprovals => "Jump to Approvals lane",
            Self::JumpBatchReviewReady => "Jump to PR Reviews lane",
            Self::JumpBatchFyiDigest => "Jump to FYI Digest lane",
            Self::OpenTicketPicker => "Open ticket picker",
            Self::CloseTicketPicker => "Close ticket picker",
            Self::TicketPickerMoveNext => "Move to next ticket picker row",
            Self::TicketPickerMovePrevious => "Move to previous ticket picker row",
            Self::TicketPickerFoldProject => "Fold selected project in ticket picker",
            Self::TicketPickerUnfoldProject => "Unfold selected project in ticket picker",
            Self::TicketPickerStartSelected => "Start selected ticket",
            Self::SetApplicationModeAutopilot => "Set workflow mode to autopilot",
            Self::SetApplicationModeManual => "Set workflow mode to manual",
            Self::ToggleWorkflowProfilesModal => "Toggle workflow profiles modal",
            Self::ToggleWorktreeDiffModal => "Toggle worktree diff modal for selected session",
            Self::AdvanceTerminalWorkflowStage => "Advance terminal workflow stage",
            Self::ArchiveSelectedSession => "Archive selected terminal session",
            Self::OpenSessionOutputForSelectedInbox => {
                "Open session output for selected inbox item"
            }
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }

    fn is_registered(id: &str) -> bool {
        Self::from_id(id).is_some()
    }
}

fn describe_next_key_binding(hint: &keymap::PrefixHint) -> String {
    if let Some(command_id) = hint.command_id.as_deref() {
        return UiCommand::from_id(command_id)
            .map(UiCommand::description)
            .unwrap_or(command_id)
            .to_owned();
    }
    if let Some(prefix_label) = hint.prefix_label.as_deref() {
        return format!("{prefix_label} (prefix)");
    }
    "Prefix".to_owned()
}

fn default_keymap_config() -> KeymapConfig {
    let binding = |keys: &[&str], command: UiCommand| KeyBindingConfig {
        keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        command_id: command.id().to_owned(),
    };

    KeymapConfig {
        modes: vec![
            ModeKeymapConfig {
                mode: UiMode::Normal,
                bindings: vec![
                    binding(&["q"], UiCommand::QuitShell),
                    binding(&["down"], UiCommand::FocusNextInbox),
                    binding(&["j"], UiCommand::FocusNextInbox),
                    binding(&["up"], UiCommand::FocusPreviousInbox),
                    binding(&["k"], UiCommand::FocusPreviousInbox),
                    binding(&["tab"], UiCommand::CycleSidebarFocusPrevious),
                    binding(&["backtab"], UiCommand::CycleSidebarFocusNext),
                    binding(&["]"], UiCommand::CycleBatchNext),
                    binding(&["["], UiCommand::CycleBatchPrevious),
                    binding(&["g"], UiCommand::JumpFirstInbox),
                    binding(&["G"], UiCommand::JumpLastInbox),
                    binding(&["1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["2"], UiCommand::JumpBatchApprovals),
                    binding(&["3"], UiCommand::JumpBatchReviewReady),
                    binding(&["4"], UiCommand::JumpBatchFyiDigest),
                    binding(&["s"], UiCommand::OpenTicketPicker),
                    binding(&["c"], UiCommand::ToggleGlobalSupervisorChat),
                    binding(&["i"], UiCommand::EnterInsertMode),
                    binding(&["I"], UiCommand::OpenTerminalForSelected),
                    binding(&["o"], UiCommand::OpenSessionOutputForSelectedInbox),
                    binding(&["`"], UiCommand::ToggleWorkflowProfilesModal),
                    binding(&["D"], UiCommand::ToggleWorktreeDiffModal),
                    binding(&["w", "n"], UiCommand::AdvanceTerminalWorkflowStage),
                    binding(&["x"], UiCommand::ArchiveSelectedSession),
                    binding(&["z", "1"], UiCommand::JumpBatchDecideOrUnblock),
                    binding(&["z", "2"], UiCommand::JumpBatchApprovals),
                    binding(&["z", "3"], UiCommand::JumpBatchReviewReady),
                    binding(&["z", "4"], UiCommand::JumpBatchFyiDigest),
                    binding(&["v", "d"], UiCommand::OpenDiffInspectorForSelected),
                    binding(&["v", "t"], UiCommand::OpenTestInspectorForSelected),
                    binding(&["v", "p"], UiCommand::OpenPrInspectorForSelected),
                    binding(&["v", "c"], UiCommand::OpenChatInspectorForSelected),
                ],
                prefixes: vec![
                    KeyPrefixConfig {
                        keys: vec!["z".to_owned()],
                        label: "Batch jumps".to_owned(),
                    },
                    KeyPrefixConfig {
                        keys: vec!["v".to_owned()],
                        label: "Artifact inspectors".to_owned(),
                    },
                    KeyPrefixConfig {
                        keys: vec!["w".to_owned()],
                        label: "Workflow Actions".to_owned(),
                    },
                ],
            },
            ModeKeymapConfig {
                mode: UiMode::Insert,
                bindings: Vec::new(),
                prefixes: Vec::new(),
            },
            ModeKeymapConfig {
                mode: UiMode::Terminal,
                bindings: Vec::new(),
                prefixes: Vec::new(),
            },
        ],
    }
}

fn default_keymap_trie() -> &'static KeymapTrie {
    static KEYMAP: OnceLock<KeymapTrie> = OnceLock::new();
    KEYMAP.get_or_init(|| {
        KeymapTrie::compile(&default_keymap_config(), UiCommand::is_registered)
            .expect("default UI keymap must compile without conflicts")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoutedInput {
    Command(UiCommand),
    UnsupportedCommand { command_id: String },
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BottomBarHintGroup {
    label: &'static str,
    hints: &'static [&'static str],
}

fn bottom_bar_hint_groups(mode: UiMode) -> &'static [BottomBarHintGroup] {
    match mode {
        UiMode::Normal => &[
            BottomBarHintGroup {
                label: "Navigate:",
                hints: &["j/k", "g/G", "1-4 or z{1-4}", "[ ]"],
            },
            BottomBarHintGroup {
                label: "Focus:",
                hints: &["Tab", "Shift+Tab"],
            },
            BottomBarHintGroup {
                label: "Views:",
                hints: &["i/I", "o", "s", "c", "`", "v{d/t/p/c}", "D"],
            },
            BottomBarHintGroup {
                label: "Workflow:",
                hints: &["w n", "x", "q"],
            },
        ],
        UiMode::Insert => &[
            BottomBarHintGroup {
                label: "Insert:",
                hints: &["type/edit input"],
            },
            BottomBarHintGroup {
                label: "Back:",
                hints: &["Esc", "Ctrl-["],
            },
        ],
        UiMode::Terminal => &[
            BottomBarHintGroup {
                label: "Terminal:",
                hints: &["Type by default", "Ctrl+Enter send"],
            },
            BottomBarHintGroup {
                label: "Back:",
                hints: &["Esc", "Ctrl-\\ Ctrl-n"],
            },
            BottomBarHintGroup {
                label: "Workflow:",
                hints: &["w n"],
            },
        ],
    }
}

fn mode_help(mode: UiMode) -> String {
    bottom_bar_hint_groups(mode)
        .iter()
        .map(|group| format!("{} {}", group.label, group.hints.join(", ")))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn append_terminal_output(state: &mut TerminalViewState, bytes: Vec<u8>) {
    let chunk = sanitize_terminal_display_text(String::from_utf8_lossy(&bytes).as_ref());
    if chunk.is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

    let mut combined = String::new();
    combined.push_str(state.output_fragment.as_str());
    combined.push_str(chunk.as_str());
    state.output_fragment.clear();

    let mut lines = combined.split('\n').collect::<Vec<_>>();
    if lines.is_empty() {
        return;
    }

    if !combined.ends_with('\n') {
        state.output_fragment = lines.pop().unwrap_or_default().to_owned();
    }

    for raw_line in lines {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some((before, kind, content)) = parse_terminal_meta_line_embedded(line) {
            let before = before.trim();
            if !before.is_empty() {
                if is_outgoing_transcript_line(before) {
                    append_terminal_transcript_entry(
                        state,
                        TerminalTranscriptEntry::Message(format!(
                            "> {}",
                            normalize_outgoing_user_line(before)
                        )),
                    );
                } else {
                    append_terminal_transcript_entry(
                        state,
                        TerminalTranscriptEntry::Message(before.to_owned()),
                    );
                }
            }
            append_terminal_foldable_content(state, kind, content.as_str());
            continue;
        }
        if is_outgoing_transcript_line(line) {
            append_terminal_transcript_entry(
                state,
                TerminalTranscriptEntry::Message(format!(
                    "> {}",
                    normalize_outgoing_user_line(line)
                )),
            );
        } else {
            append_terminal_transcript_entry(
                state,
                TerminalTranscriptEntry::Message(line.to_owned()),
            );
        }
    }
}

fn append_terminal_assistant_output(state: &mut TerminalViewState, bytes: Vec<u8>) {
    append_terminal_output(state, bytes);
}

fn append_terminal_user_message(state: &mut TerminalViewState, message: &str) {
    flush_terminal_output_fragment(state);
    let text = sanitize_terminal_display_text(message);
    if text.trim().is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let _ = index;
        append_terminal_transcript_entry(
            state,
            TerminalTranscriptEntry::Message(format!("> {line}")),
        );
    }
}

fn append_terminal_system_message(state: &mut TerminalViewState, message: &str) {
    flush_terminal_output_fragment(state);
    let text = sanitize_terminal_display_text(message);
    if text.trim().is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if index == 0 {
            append_terminal_transcript_entry(
                state,
                TerminalTranscriptEntry::Message(format!("> system: {line}")),
            );
        } else {
            append_terminal_transcript_entry(
                state,
                TerminalTranscriptEntry::Message(format!("> system: {line}")),
            );
        }
    }
}

fn parse_terminal_meta_line_embedded(line: &str) -> Option<(String, TerminalFoldKind, String)> {
    const PREFIX: &str = "[[orchestrator-meta|";
    let start = line.find(PREFIX)?;
    let before = line[..start].to_owned();
    let suffix = &line[start + PREFIX.len()..];
    let closing = suffix.find("]]")?;
    let kind = match suffix[..closing].trim() {
        "reasoning" => TerminalFoldKind::Reasoning,
        "file-change" => TerminalFoldKind::FileChange,
        "tool-call" => TerminalFoldKind::ToolCall,
        "command" => TerminalFoldKind::CommandExecution,
        _ => TerminalFoldKind::Other,
    };
    let content = suffix[closing + 2..].trim().to_owned();
    Some((before, kind, content))
}

fn append_terminal_foldable_content(
    state: &mut TerminalViewState,
    kind: TerminalFoldKind,
    content: &str,
) {
    invalidate_terminal_render_cache(state);
    let entry_content = if content.trim().is_empty() {
        "(no details)"
    } else {
        content.trim()
    };

    if let Some(TerminalTranscriptEntry::Foldable(previous)) = state.entries.last_mut() {
        if previous.kind == kind {
            if !previous.content.is_empty() {
                previous.content.push('\n');
            }
            previous.content.push_str(entry_content);
            return;
        }
    }

    append_terminal_transcript_entry(
        state,
        TerminalTranscriptEntry::Foldable(TerminalFoldSection {
            kind,
            content: entry_content.to_owned(),
            folded: true,
        }),
    );
}

fn is_outgoing_transcript_line(line: &str) -> bool {
    let normalized = line.trim_start();
    normalized.starts_with("system:")
        || normalized.starts_with("you:")
        || normalized.starts_with("user:")
}

fn flush_terminal_output_fragment(state: &mut TerminalViewState) {
    let fragment = std::mem::take(&mut state.output_fragment);
    let line = fragment.trim_end_matches('\r');
    if line.trim().is_empty() {
        return;
    }
    invalidate_terminal_render_cache(state);

    if let Some((before, kind, content)) = parse_terminal_meta_line_embedded(line) {
        let before = before.trim();
        if !before.is_empty() {
            if is_outgoing_transcript_line(before) {
                append_terminal_transcript_entry(
                    state,
                    TerminalTranscriptEntry::Message(format!(
                        "> {}",
                        normalize_outgoing_user_line(before)
                    )),
                );
            } else {
                append_terminal_transcript_entry(
                    state,
                    TerminalTranscriptEntry::Message(before.to_owned()),
                );
            }
        }
        append_terminal_foldable_content(state, kind, content.as_str());
        return;
    }

    if is_outgoing_transcript_line(line) {
        append_terminal_transcript_entry(
            state,
            TerminalTranscriptEntry::Message(format!("> {}", normalize_outgoing_user_line(line))),
        );
    } else {
        append_terminal_transcript_entry(state, TerminalTranscriptEntry::Message(line.to_owned()));
    }
}

fn append_terminal_transcript_entry(state: &mut TerminalViewState, entry: TerminalTranscriptEntry) {
    state.entries.push(entry);
    let transcript_line_limit = transcript_line_limit_config_value();
    if state.entries.len() <= transcript_line_limit {
        return;
    }

    let dropped = state.entries.len() - transcript_line_limit;
    state.entries.drain(0..dropped);
    state.transcript_truncated = true;
    state.transcript_truncated_line_count = state
        .transcript_truncated_line_count
        .saturating_add(dropped);
}

fn normalize_outgoing_user_line(line: &str) -> &str {
    line.strip_prefix("you: ")
        .or_else(|| line.strip_prefix("user: "))
        .unwrap_or(line)
}

fn invalidate_terminal_render_cache(state: &mut TerminalViewState) {
    state.render_cache.invalidate_all();
    state.output_rendered_line_count = 0;
}

fn handle_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> bool {
    match route_key_press(shell_state, key) {
        RoutedInput::Command(command) => dispatch_command(shell_state, command),
        RoutedInput::UnsupportedCommand { command_id } => {
            shell_state.status_warning = Some(format!(
                "unsupported command mapping '{}' in {} mode keymap",
                command_id,
                shell_state.mode.label()
            ));
            false
        }
        RoutedInput::Ignore => false,
    }
}

fn route_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.worktree_diff_modal.is_some() {
        return route_worktree_diff_modal_key(shell_state, key);
    }
    if shell_state.workflow_profiles_modal.visible {
        return route_workflow_profiles_modal_key(shell_state, key);
    }
    if shell_state.is_ticket_picker_visible() {
        return route_ticket_picker_key(shell_state, key);
    }
    if shell_state.archive_session_confirm_session.is_some() {
        return route_archive_session_confirm_key(shell_state, key);
    }
    if shell_state.review_merge_confirm_session.is_some() {
        return route_review_merge_confirm_key(shell_state, key);
    }

    if shell_state.terminal_session_has_active_needs_input() && shell_state.is_right_pane_focused()
    {
        return route_needs_input_modal_key(shell_state, key);
    }
    if shell_state.terminal_session_has_any_needs_input()
        && !shell_state.terminal_session_has_active_needs_input()
        && shell_state.is_right_pane_focused()
        && shell_state.is_terminal_view_active()
        && key.modifiers.is_empty()
        && is_needs_input_interaction_key(key.code)
    {
        let _ = shell_state.activate_terminal_needs_input(false);
        return route_needs_input_modal_key(shell_state, key);
    }

    if matches!(key.code, KeyCode::Tab) {
        shell_state.cycle_pane_focus();
        return RoutedInput::Ignore;
    }

    if shell_state.mode == UiMode::Normal
        && shell_state.is_right_pane_focused()
        && shell_state.is_terminal_view_active()
    {
        match key.code {
            KeyCode::Char('j') | KeyCode::Char('J') => {
                shell_state.scroll_terminal_output_view(1);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                shell_state.scroll_terminal_output_view(-1);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('G') => {
                shell_state.scroll_terminal_output_to_bottom();
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    if shell_state.mode == UiMode::Terminal
        && shell_state.is_terminal_view_active()
        && key.code == KeyCode::Esc
        && key.modifiers.is_empty()
        && shell_state.apply_terminal_compose_key(key)
    {
        return RoutedInput::Ignore;
    }

    if is_escape_to_normal(key) {
        if shell_state.is_active_supervisor_stream_visible()
            && !shell_state.is_global_supervisor_chat_active()
        {
            shell_state.cancel_supervisor_stream();
        }
        return RoutedInput::Command(UiCommand::EnterNormalMode);
    }
    if is_ctrl_char(key, 'c') && shell_state.is_active_supervisor_stream_visible() {
        shell_state.cancel_supervisor_stream();
        return RoutedInput::Ignore;
    }
    if shell_state.apply_global_chat_insert_key(key) {
        return RoutedInput::Ignore;
    }
    if shell_state.mode == UiMode::Terminal && shell_state.terminal_escape_pending {
        return route_terminal_mode_key(shell_state, key);
    }
    if shell_state.apply_terminal_compose_key(key) {
        return RoutedInput::Ignore;
    }

    match shell_state.mode {
        UiMode::Normal | UiMode::Insert => route_configured_mode_key(shell_state, key),
        UiMode::Terminal => {
            if shell_state.is_terminal_view_active() {
                route_terminal_mode_key(shell_state, key)
            } else {
                RoutedInput::Command(UiCommand::EnterNormalMode)
            }
        }
    }
}

fn is_needs_input_interaction_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('i')
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Right
            | KeyCode::Char('l')
            | KeyCode::Left
            | KeyCode::Char('h')
            | KeyCode::Enter
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char(' ')
            | KeyCode::Char('j')
            | KeyCode::Char('k')
    )
}

fn route_worktree_diff_modal_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if is_escape_to_normal(key) {
        shell_state.close_worktree_diff_modal();
        return RoutedInput::Ignore;
    }

    if matches!(key.code, KeyCode::Char('D')) {
        shell_state.close_worktree_diff_modal();
        return RoutedInput::Ignore;
    }

    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Enter => {
                shell_state.insert_selected_worktree_diff_refs_into_compose();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('q') => {
                shell_state.close_worktree_diff_modal();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('g') => {
                shell_state.jump_worktree_diff_addition_block(false);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('G') => {
                shell_state.jump_worktree_diff_addition_block(true);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                shell_state.scroll_worktree_diff_modal(1);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                shell_state.scroll_worktree_diff_modal(-1);
                return RoutedInput::Ignore;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                shell_state.focus_worktree_diff_files_pane();
                return RoutedInput::Ignore;
            }
            KeyCode::Char('l') | KeyCode::Right => {
                shell_state.focus_worktree_diff_detail_pane();
                return RoutedInput::Ignore;
            }
            KeyCode::PageDown => {
                shell_state.scroll_worktree_diff_modal(12);
                return RoutedInput::Ignore;
            }
            KeyCode::PageUp => {
                shell_state.scroll_worktree_diff_modal(-12);
                return RoutedInput::Ignore;
            }
            _ => {}
        }
    }

    RoutedInput::Ignore
}

fn route_workflow_profiles_modal_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.workflow_profiles_modal.renaming {
        if is_escape_to_normal(key) {
            shell_state.cancel_workflow_profile_rename();
            return RoutedInput::Ignore;
        }
        if matches!(key.code, KeyCode::Enter) && key.modifiers.is_empty() {
            shell_state.submit_workflow_profile_rename();
            return RoutedInput::Ignore;
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Backspace => {
                    shell_state.pop_workflow_profile_rename_char();
                    return RoutedInput::Ignore;
                }
                KeyCode::Char(ch) => {
                    shell_state.append_workflow_profile_rename_char(ch);
                    return RoutedInput::Ignore;
                }
                _ => {}
            }
        }
        return RoutedInput::Ignore;
    }

    if is_escape_to_normal(key) {
        shell_state.close_workflow_profiles_modal();
        return RoutedInput::Ignore;
    }
    if !key.modifiers.is_empty() {
        return RoutedInput::Ignore;
    }

    match key.code {
        KeyCode::Char('`') | KeyCode::Char('q') => {
            shell_state.close_workflow_profiles_modal();
            RoutedInput::Ignore
        }
        KeyCode::Char('h') | KeyCode::Left => {
            shell_state.cycle_workflow_profile_selection(-1);
            RoutedInput::Ignore
        }
        KeyCode::Char('l') | KeyCode::Right => {
            shell_state.cycle_workflow_profile_selection(1);
            RoutedInput::Ignore
        }
        KeyCode::Char('j') | KeyCode::Down => {
            shell_state.move_workflow_profile_state_selection(1);
            RoutedInput::Ignore
        }
        KeyCode::Char('k') | KeyCode::Up => {
            shell_state.move_workflow_profile_state_selection(-1);
            RoutedInput::Ignore
        }
        KeyCode::Char(' ') | KeyCode::Enter => {
            shell_state.toggle_selected_workflow_profile_state_level();
            RoutedInput::Ignore
        }
        KeyCode::Char('c') => {
            shell_state.add_workflow_profile();
            RoutedInput::Ignore
        }
        KeyCode::Char('d') => {
            shell_state.delete_selected_workflow_profile();
            RoutedInput::Ignore
        }
        KeyCode::Char('g') => {
            shell_state.set_selected_workflow_profile_as_default();
            RoutedInput::Ignore
        }
        KeyCode::Char('r') => {
            shell_state.begin_workflow_profile_rename();
            RoutedInput::Ignore
        }
        KeyCode::Char('s') => {
            shell_state.save_workflow_profiles();
            RoutedInput::Ignore
        }
        _ => RoutedInput::Ignore,
    }
}

fn route_configured_mode_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    match shell_state.keymap.route_key_event(
        shell_state.mode,
        &mut shell_state.mode_key_buffer,
        key,
    ) {
        KeymapLookupResult::Command { command_id } => {
            shell_state.which_key_overlay = None;
            UiCommand::from_id(command_id.as_str())
                .map(RoutedInput::Command)
                .unwrap_or(RoutedInput::UnsupportedCommand { command_id })
        }
        KeymapLookupResult::Prefix { .. } => {
            shell_state.refresh_which_key_overlay();
            RoutedInput::Ignore
        }
        KeymapLookupResult::InvalidPrefix | KeymapLookupResult::NoMatch => {
            shell_state.which_key_overlay = None;
            RoutedInput::Ignore
        }
    }
}

fn route_terminal_mode_key(shell_state: &mut UiShellState, key: KeyEvent) -> RoutedInput {
    if shell_state.terminal_escape_pending {
        if is_ctrl_char(key, 'n') {
            return RoutedInput::Command(UiCommand::EnterNormalMode);
        }
        shell_state.terminal_escape_pending = false;
        return RoutedInput::Ignore;
    }

    if is_ctrl_char(key, '\\') {
        return RoutedInput::Command(UiCommand::StartTerminalEscapeChord);
    }

    RoutedInput::Ignore
}

fn dispatch_command(shell_state: &mut UiShellState, command: UiCommand) -> bool {
    let _command_id = command.id();
    match command {
        UiCommand::EnterNormalMode => {
            shell_state.enter_normal_mode();
            false
        }
        UiCommand::EnterInsertMode => {
            shell_state.enter_insert_mode_for_current_focus();
            false
        }
        UiCommand::ToggleGlobalSupervisorChat => {
            shell_state.toggle_global_supervisor_chat();
            false
        }
        UiCommand::OpenTerminalForSelected => {
            shell_state.open_terminal_and_enter_mode();
            false
        }
        UiCommand::OpenDiffInspectorForSelected => {
            shell_state.open_inspector_for_selected(ArtifactInspectorKind::Diff);
            false
        }
        UiCommand::OpenTestInspectorForSelected => {
            shell_state.open_inspector_for_selected(ArtifactInspectorKind::Test);
            false
        }
        UiCommand::OpenPrInspectorForSelected => {
            shell_state.open_inspector_for_selected(ArtifactInspectorKind::PullRequest);
            false
        }
        UiCommand::OpenChatInspectorForSelected => {
            shell_state.open_chat_inspector_for_selected();
            false
        }
        UiCommand::StartTerminalEscapeChord => {
            shell_state.begin_terminal_escape_chord();
            false
        }
        UiCommand::QuitShell => true,
        UiCommand::FocusNextInbox => {
            shell_state.move_selection(1);
            false
        }
        UiCommand::FocusPreviousInbox => {
            shell_state.move_selection(-1);
            false
        }
        UiCommand::CycleSidebarFocusNext => {
            shell_state.cycle_sidebar_focus(1);
            false
        }
        UiCommand::CycleSidebarFocusPrevious => {
            shell_state.cycle_sidebar_focus(-1);
            false
        }
        UiCommand::CycleBatchNext => {
            shell_state.cycle_batch(1);
            false
        }
        UiCommand::CycleBatchPrevious => {
            shell_state.cycle_batch(-1);
            false
        }
        UiCommand::JumpFirstInbox => {
            shell_state.jump_to_first_item();
            false
        }
        UiCommand::JumpLastInbox => {
            shell_state.jump_to_last_item();
            false
        }
        UiCommand::JumpBatchDecideOrUnblock => {
            shell_state.jump_to_batch(InboxBatchKind::DecideOrUnblock);
            false
        }
        UiCommand::JumpBatchApprovals => {
            shell_state.jump_to_batch(InboxBatchKind::Approvals);
            false
        }
        UiCommand::JumpBatchReviewReady => {
            shell_state.jump_to_batch(InboxBatchKind::ReviewReady);
            false
        }
        UiCommand::JumpBatchFyiDigest => {
            shell_state.jump_to_batch(InboxBatchKind::FyiDigest);
            false
        }
        UiCommand::OpenTicketPicker => {
            shell_state.open_ticket_picker();
            false
        }
        UiCommand::CloseTicketPicker => {
            shell_state.close_ticket_picker();
            false
        }
        UiCommand::TicketPickerMoveNext => {
            shell_state.move_ticket_picker_selection(1);
            false
        }
        UiCommand::TicketPickerMovePrevious => {
            shell_state.move_ticket_picker_selection(-1);
            false
        }
        UiCommand::TicketPickerFoldProject => {
            shell_state.fold_ticket_picker_selected_project();
            false
        }
        UiCommand::TicketPickerUnfoldProject => {
            shell_state.unfold_ticket_picker_selected_project();
            false
        }
        UiCommand::TicketPickerStartSelected => {
            shell_state.start_selected_ticket_from_picker();
            false
        }
        UiCommand::SetApplicationModeAutopilot => {
            shell_state.set_application_mode_autopilot();
            false
        }
        UiCommand::SetApplicationModeManual => {
            shell_state.set_application_mode_manual();
            false
        }
        UiCommand::ToggleWorkflowProfilesModal => {
            shell_state.toggle_workflow_profiles_modal();
            false
        }
        UiCommand::ToggleWorktreeDiffModal => {
            shell_state.toggle_worktree_diff_modal();
            false
        }
        UiCommand::AdvanceTerminalWorkflowStage => {
            shell_state.advance_terminal_workflow_stage();
            false
        }
        UiCommand::ArchiveSelectedSession => {
            shell_state.begin_archive_selected_session_confirmation();
            false
        }
        UiCommand::OpenSessionOutputForSelectedInbox => {
            shell_state.open_session_output_for_selected_inbox();
            false
        }
    }
}

fn is_escape_to_normal(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc) && key.modifiers.is_empty() || is_ctrl_char(key, '[')
}

fn is_ctrl_char(key: KeyEvent, ch: char) -> bool {
    matches!(key.code, KeyCode::Char(code_ch) if code_ch == ch)
        && key.modifiers == KeyModifiers::CONTROL
}

#[cfg(test)]
fn command_id(command: UiCommand) -> &'static str {
    command.id()
}

#[cfg(test)]
fn routed_command(route: RoutedInput) -> Option<UiCommand> {
    match route {
        RoutedInput::Command(command) => Some(command),
        _ => None,
    }
}
