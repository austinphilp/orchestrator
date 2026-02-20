fn render_inbox_panel(ui_state: &UiState) -> String {
    if ui_state.inbox_rows.is_empty() {
        return "No inbox items.".to_owned();
    }

    let mut lines = Vec::new();

    let batch_lane_summary = ui_state
        .inbox_batch_surfaces
        .iter()
        .filter(|surface| surface.total_count > 0)
        .map(|surface| {
            format!(
                "[{}] {} {}",
                surface.kind.hotkey(),
                surface.kind.label(),
                surface.unresolved_count
            )
        })
        .collect::<Vec<_>>();
    if !batch_lane_summary.is_empty() {
        lines.push(format!("Batch lanes: {}", batch_lane_summary.join(" | ")));
        lines.push(String::new());
    }

    let mut active_band = None;
    for (index, row) in ui_state.inbox_rows.iter().enumerate() {
        if active_band != Some(row.priority_band) {
            if active_band.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("{}:", row.priority_band.label()));
            active_band = Some(row.priority_band);
        }

        let selected = if Some(index) == ui_state.selected_inbox_index {
            ">"
        } else {
            " "
        };
        let resolved = if row.resolved { "x" } else { " " };
        lines.push(format!(
            "{selected} [{resolved}] {:?}: {}",
            row.kind, row.title
        ));
    }

    lines.join("\n")
}

fn session_panel_rows(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
) -> Vec<SessionPanelRow> {
    let work_item_repo = work_item_repository_labels(domain);
    let ticket_labels = ticket_labels_by_ticket_id(domain);

    let mut rows = Vec::new();
    for session in domain.sessions.values() {
        if !is_open_session_status(session.status.as_ref()) {
            continue;
        }
        if let Some(work_item_id) = session.work_item_id.as_ref() {
            if domain
                .work_items
                .get(work_item_id)
                .and_then(|work_item| work_item.session_id.as_ref())
                != Some(&session.id)
            {
                continue;
            }
        }

        let project = project_label_for_session(session, domain, &work_item_repo);
        let ticket = ticket_label_for_session(session, domain, &ticket_labels);
        let badge = workflow_badge_for_session(session, domain);
        let group = session_state_group_for_session(session, domain, badge.as_str());
        let is_turn_active = session_turn_is_running(terminal_session_states, session);
        rows.push(SessionPanelRow {
            session_id: session.id.clone(),
            project,
            group,
            ticket_label: ticket,
            badge,
            is_turn_active,
        });
    }

    rows.sort_unstable_by(|left, right| {
        left.project
            .cmp(&right.project)
            .then_with(|| left.group.sort_key().cmp(&right.group.sort_key()))
            .then_with(|| {
                left.ticket_label
                    .to_ascii_lowercase()
                    .cmp(&right.ticket_label.to_ascii_lowercase())
            })
            .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
    });

    rows
}

fn render_sessions_panel(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    selected_session_id: Option<&WorkerSessionId>,
) -> String {
    let session_rows = session_panel_rows(domain, terminal_session_states);
    if session_rows.is_empty() {
        return "No open sessions.".to_owned();
    }

    let mut lines = Vec::new();
    let mut previous_project: Option<String> = None;
    let mut previous_group: Option<SessionStateGroup> = None;
    for row in session_rows {
        let is_selected = selected_session_id == Some(&row.session_id);
        let marker = if is_selected { ">" } else { " " };
        if previous_project.as_deref() != Some(row.project.as_str()) {
            if previous_project.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("{}:", row.project));
            previous_project = Some(row.project.clone());
            previous_group = None;
        }
        if previous_group.as_ref() != Some(&row.group) {
            lines.push(format!("  {}:", row.group.display_label()));
            previous_group = Some(row.group.clone());
        }
        let spinner = if row.is_turn_active {
            format!(" {}", loading_spinner_frame())
        } else {
            String::new()
        };
        let line = if row.group.is_other() {
            format!("    [{}] {}{}", row.badge, row.ticket_label, spinner)
        } else {
            format!("    {}{}", row.ticket_label, spinner)
        };
        lines.push(format!("{marker}{line}"));
    }

    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionPanelRow {
    session_id: WorkerSessionId,
    project: String,
    group: SessionStateGroup,
    ticket_label: String,
    badge: String,
    is_turn_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionStateGroup {
    Planning,
    Implementation,
    Review,
    Other(String),
}

impl SessionStateGroup {
    fn sort_key(&self) -> (u8, &str) {
        match self {
            Self::Planning => (0, ""),
            Self::Implementation => (1, ""),
            Self::Review => (2, ""),
            Self::Other(label) => (3, label.as_str()),
        }
    }

    fn display_label(&self) -> &'static str {
        match self {
            Self::Planning => "Planning",
            Self::Implementation => "Implementation",
            Self::Review => "Review",
            Self::Other(_) => "Other",
        }
    }

    fn is_other(&self) -> bool {
        matches!(self, Self::Other(_))
    }
}

fn render_center_panel(ui_state: &UiState) -> String {
    let mut lines = Vec::with_capacity(ui_state.center_pane.lines.len() + 2);
    lines.push(format!("Stack: {}", ui_state.center_stack_label()));
    lines.push(String::new());
    lines.extend(ui_state.center_pane.lines.iter().cloned());
    lines.join("\n")
}

fn render_terminal_top_bar(domain: &ProjectionState, session_id: &WorkerSessionId) -> String {
    let labels = session_display_labels(domain, session_id);
    let text = if let Some(session) = domain.sessions.get(session_id) {
        let status = session
            .status
            .as_ref()
            .map(|entry| format!("{entry:?}"))
            .unwrap_or_else(|| "Unknown".to_owned());
        let checkpoint = session
            .latest_checkpoint
            .as_ref()
            .map(|entry| entry.as_str().to_owned())
            .unwrap_or_else(|| "none".to_owned());
        format!(
            "ticket: {} | status: {} | checkpoint: {}",
            labels.compact_label,
            status,
            checkpoint
        )
    } else {
        format!(
            "ticket: {} | status: unavailable | checkpoint: none",
            labels.compact_label
        )
    };
    sanitize_terminal_display_text(text.as_str())
}

fn render_terminal_transcript_lines(state: &TerminalViewState) -> Vec<String> {
    render_terminal_transcript_entries(state)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

fn render_terminal_transcript_entries(state: &TerminalViewState) -> Vec<RenderedTerminalLine> {
    let mut lines = Vec::new();
    for (index, entry) in state.entries.iter().enumerate() {
        let previous_is_foldable = index
            .checked_sub(1)
            .and_then(|previous| state.entries.get(previous))
            .map(|previous| matches!(previous, TerminalTranscriptEntry::Foldable(_)))
            .unwrap_or(false);
        let current_is_foldable = matches!(entry, TerminalTranscriptEntry::Foldable(_));
        if index > 0 && (previous_is_foldable || current_is_foldable) {
            lines.push(RenderedTerminalLine {
                text: String::new(),
            });
        }
        match entry {
            TerminalTranscriptEntry::Message(line) => {
                if is_user_outgoing_terminal_message(line)
                    && lines
                        .last()
                        .map(|previous| !previous.text.trim().is_empty())
                        .unwrap_or(false)
                {
                    lines.push(RenderedTerminalLine {
                        text: String::new(),
                    });
                }
                lines.push(RenderedTerminalLine {
                    text: line.clone(),
                });
                if is_user_outgoing_terminal_message(line) {
                    lines.push(RenderedTerminalLine {
                        text: String::new(),
                    });
                }
            }
            TerminalTranscriptEntry::Foldable(section) => {
                let fold_marker = if section.folded { "[+]" } else { "[-]" };
                let summary = summarize_folded_terminal_content(section.content.as_str());
                lines.push(RenderedTerminalLine {
                    text: format!("  {fold_marker} {}: {summary}", section.kind.label()),
                });
                if !section.folded {
                    for content_line in section.content.lines() {
                        let trimmed = content_line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        lines.push(RenderedTerminalLine {
                            text: format!("  {trimmed}"),
                        });
                    }
                }
            }
        }
    }
    if !state.output_fragment.is_empty() {
        lines.push(RenderedTerminalLine {
            text: state.output_fragment.clone(),
        });
    }
    lines
}

fn is_user_outgoing_terminal_message(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("> ") && !trimmed.starts_with("> system:")
}

fn summarize_folded_terminal_content(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "(no details)".to_owned();
    }
    const MAX_CHARS: usize = 96;
    if compact.chars().count() > MAX_CHARS {
        let truncated = compact.chars().take(MAX_CHARS).collect::<String>();
        format!("{truncated}...")
    } else {
        compact
    }
}

fn render_terminal_output_panel(ui_state: &UiState, width: u16) -> Text<'static> {
    if ui_state.center_pane.lines.is_empty() {
        return Text::raw("No terminal output available yet.");
    }
    render_terminal_output_with_accents(&ui_state.center_pane.lines, width.max(1))
}

fn render_terminal_output_with_accents(lines: &[String], width: u16) -> Text<'static> {
    let mut rendered = Vec::new();
    let mut active_fold_kind: Option<TerminalFoldKind> = None;

    for raw_line in lines {
        let line = sanitize_terminal_display_text(raw_line);
        if line.trim().is_empty() {
            active_fold_kind = None;
            rendered.push(Line::from(String::new()));
            continue;
        }

        if let Some((kind, folded)) = parse_fold_header_line(&line) {
            active_fold_kind = if folded { None } else { Some(kind) };
            rendered.push(Line::from(Span::styled(
                line,
                fold_accent_style(kind, true),
            )));
            continue;
        }

        if let Some(kind) = active_fold_kind {
            rendered.push(Line::from(Span::styled(
                line,
                fold_accent_style(kind, false),
            )));
            continue;
        }

        if line.starts_with("> ") {
            rendered.push(Line::from(Span::styled(
                line,
                Style::default().fg(Color::LightBlue),
            )));
            continue;
        }

        if !line_looks_like_markdown(line.as_str()) {
            rendered.push(Line::from(line));
            continue;
        }

        let markdown = render_markdown_for_terminal(line.as_str(), width);
        if markdown.lines.is_empty() {
            rendered.push(Line::from(String::new()));
        } else {
            rendered.extend(markdown.lines.into_iter());
        }
    }

    Text::from(rendered)
}

fn line_looks_like_markdown(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#')
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("> ")
        || trimmed.starts_with("```")
        || trimmed.starts_with('`')
        || trimmed.contains("**")
        || trimmed.contains("__")
        || trimmed.contains("`")
        || (trimmed.contains('[') && trimmed.contains("]("))
}

fn parse_fold_header_line(line: &str) -> Option<(TerminalFoldKind, bool)> {
    let trimmed = line.trim_start();
    let without_cursor = trimmed.strip_prefix("=> ").unwrap_or(trimmed);
    let (folded, remainder) = if let Some(rest) = without_cursor.strip_prefix("[+]") {
        (true, rest)
    } else if let Some(rest) = without_cursor.strip_prefix("[-]") {
        (false, rest)
    } else {
        return None;
    };

    let remainder = remainder.trim_start();
    for kind in [
        TerminalFoldKind::Reasoning,
        TerminalFoldKind::FileChange,
        TerminalFoldKind::ToolCall,
        TerminalFoldKind::CommandExecution,
        TerminalFoldKind::Other,
    ] {
        let prefix = format!("{}:", kind.label());
        if remainder.starts_with(prefix.as_str()) {
            return Some((kind, folded));
        }
    }
    None
}

fn fold_accent_style(kind: TerminalFoldKind, header: bool) -> Style {
    let color = match kind {
        TerminalFoldKind::Reasoning => Color::Rgb(163, 190, 140),
        TerminalFoldKind::FileChange => Color::Rgb(208, 135, 112),
        TerminalFoldKind::ToolCall => Color::Rgb(180, 142, 173),
        TerminalFoldKind::CommandExecution => Color::Rgb(129, 161, 193),
        TerminalFoldKind::Other => Color::Rgb(143, 188, 187),
    };
    let mut style = Style::default().fg(color);
    if header {
        style = style.add_modifier(Modifier::BOLD);
    } else {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalActivityIndicator {
    None,
    Working,
    AwaitingInput,
}

fn terminal_activity_indicator(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> TerminalActivityIndicator {
    if terminal_session_is_running(domain, terminal_session_states, session_id) {
        return TerminalActivityIndicator::Working;
    }
    if terminal_session_is_awaiting_input(domain, terminal_session_states, session_id) {
        return TerminalActivityIndicator::AwaitingInput;
    }
    TerminalActivityIndicator::None
}

fn terminal_session_is_running(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    if !matches!(
        domain
            .sessions
            .get(session_id)
            .and_then(|session| session.status.as_ref()),
        Some(WorkerSessionStatus::Running)
    ) {
        return false;
    }
    session_turn_is_active(terminal_session_states, session_id)
}

fn terminal_session_is_awaiting_input(
    domain: &ProjectionState,
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    if session_turn_is_active(terminal_session_states, session_id) {
        return false;
    }
    let waiting_for_user = matches!(
        domain
            .sessions
            .get(session_id)
            .and_then(|session| session.status.as_ref()),
        Some(WorkerSessionStatus::WaitingForUser)
    );
    waiting_for_user
        && terminal_session_states
            .get(session_id)
            .and_then(|state| state.active_needs_input.as_ref())
            .is_some()
}

fn session_turn_is_active(
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session_id: &WorkerSessionId,
) -> bool {
    terminal_session_states
        .get(session_id)
        .map(|state| state.turn_active)
        .unwrap_or(false)
}

fn append_terminal_loading_indicator(
    mut text: Text<'static>,
    indicator: TerminalActivityIndicator,
) -> Text<'static> {
    if matches!(indicator, TerminalActivityIndicator::None) {
        return text;
    }
    if !text.lines.is_empty() {
        text.lines.push(Line::from(String::new()));
    }
    match indicator {
        TerminalActivityIndicator::None => {}
        TerminalActivityIndicator::Working => {
            let frame = loading_spinner_frame();
            text.lines.push(Line::from(Span::styled(
                format!("{frame} agent working..."),
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::DIM),
            )));
        }
        TerminalActivityIndicator::AwaitingInput => {
            text.lines.push(Line::from(Span::styled(
                "󰥔 awaiting input",
                Style::default().fg(Color::Yellow),
            )));
        }
    }
    text
}

fn loading_spinner_frame() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let elapsed_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as usize)
        .unwrap_or(0);
    let index = (elapsed_ms / 200) % FRAMES.len();
    FRAMES[index]
}

fn render_markdown_for_terminal(input: &str, width: u16) -> Text<'static> {
    if input.is_empty() {
        return Text::raw(String::new());
    }

    let source = preprocess_markdown_layout(input);
    let lines = terminal_markdown_skin().parse(RatSkin::parse_text(source.as_str()), width);
    if lines.is_empty() {
        Text::raw(String::new())
    } else {
        Text::from(
            lines
                .into_iter()
                .map(|line| {
                    Line::from(
                        line.spans
                            .into_iter()
                            .map(|span| Span::styled(span.content.into_owned(), span.style))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    }
}

fn preprocess_markdown_layout(input: &str) -> String {
    let mut output = Vec::new();
    for raw_line in input.lines() {
        let line = raw_line.trim_end_matches('\r');
        if is_markdown_heading(line.trim_start()) {
            if output
                .last()
                .map(|entry: &String| !entry.trim().is_empty())
                .unwrap_or(false)
            {
                output.push(String::new());
            }
            output.push(line.to_owned());
            output.push(String::new());
            continue;
        }
        output.push(line.to_owned());
    }
    output.join("\n")
}

fn is_markdown_heading(line: &str) -> bool {
    let mut level = 0usize;
    for ch in line.chars() {
        if ch == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level == 0 || level > 6 {
        return false;
    }
    line.get(level..)
        .map(str::trim_start)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

fn terminal_markdown_skin() -> &'static RatSkin {
    static SKIN: OnceLock<RatSkin> = OnceLock::new();
    SKIN.get_or_init(|| {
        let mut skin = RatSkin::default();
        if matches!(ui_theme_from_env(), UiTheme::Nord) {
            apply_nord_markdown_theme(&mut skin);
        }
        skin.skin.paragraph.right_margin = 0;
        skin.skin.code_block.left_margin = 0;
        for header in &mut skin.skin.headers {
            header.left_margin = 0;
            header.right_margin = 0;
        }
        skin
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiTheme {
    Default,
    Nord,
}

fn ui_theme_from_env() -> UiTheme {
    let value = ui_theme_config_value().trim().to_ascii_lowercase();
    match value.as_str() {
        "default" => UiTheme::Default,
        "nord" => UiTheme::Nord,
        _ => UiTheme::Nord,
    }
}

fn apply_nord_markdown_theme(skin: &mut RatSkin) {
    // Nord-inspired palette
    // Polar Night:  #2E3440 #3B4252 #434C5E #4C566A
    // Snow Storm:   #D8DEE9 #E5E9F0 #ECEFF4
    // Frost:        #8FBCBB #88C0D0 #81A1C1 #5E81AC
    // Aurora:       #BF616A #D08770 #EBCB8B #A3BE8C #B48EAD
    skin.skin.paragraph.set_fg((216, 222, 233).into()); // nord4
    skin.skin.bold.set_fg((236, 239, 244).into()); // nord6
    skin.skin.italic.set_fg((136, 192, 208).into()); // nord8
    skin.skin.strikeout.set_fg((180, 142, 173).into()); // nord15

    skin.skin.headers[0].set_fg((235, 203, 139).into()); // nord13
    skin.skin.headers[1].set_fg((143, 188, 187).into()); // nord7
    skin.skin.headers[2].set_fg((129, 161, 193).into()); // nord9
    skin.skin.headers[3].set_fg((136, 192, 208).into()); // nord8
    skin.skin.headers[4].set_fg((163, 190, 140).into()); // nord14
    skin.skin.headers[5].set_fg((208, 135, 112).into()); // nord12

    skin.skin
        .inline_code
        .set_fgbg((235, 203, 139).into(), (46, 52, 64).into()); // nord13 on nord0
    skin.skin
        .code_block
        .set_fgbg((236, 239, 244).into(), (59, 66, 82).into()); // nord6 on nord1

    skin.skin.bullet.set_fg((136, 192, 208).into()); // nord8
    skin.skin.quote_mark.set_fg((94, 129, 172).into()); // nord10
    skin.skin.horizontal_rule.set_fg((76, 86, 106).into()); // nord3
    skin.skin
        .table
        .compound_style
        .set_fg((129, 161, 193).into()); // nord9
}

fn estimate_wrapped_line_count(text: &Text<'_>, _area_width: u16) -> u16 {
    let count = text.lines.len();
    if count == 0 {
        1
    } else {
        count.min(u16::MAX as usize) as u16
    }
}

fn sanitize_terminal_display_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\t' => output.push(ch),
            '\r' => output.push('\n'),
            '\u{2400}' => {}
            _ if ch.is_control() => {}
            _ => output.push(ch),
        }
    }
    output
}

fn repository_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| "unknown-repo".to_owned(), ToOwned::to_owned)
}

fn work_item_repository_labels(domain: &ProjectionState) -> HashMap<WorkItemId, String> {
    let mut work_item_repo = HashMap::new();
    for event in &domain.events {
        if let OrchestrationEventPayload::WorktreeCreated(payload) = &event.payload {
            work_item_repo.insert(
                payload.work_item_id.clone(),
                repository_name_from_path(payload.path.as_str()),
            );
        }
    }
    work_item_repo
}

fn ticket_labels_by_ticket_id(domain: &ProjectionState) -> HashMap<String, String> {
    let mut ticket_labels = HashMap::new();
    for event in &domain.events {
        if let OrchestrationEventPayload::TicketSynced(payload) = &event.payload {
            let title = payload.title.trim();
            let identifier = payload.identifier.trim();
            let label = if title.is_empty() {
                if identifier.is_empty() {
                    payload.ticket_id.as_str().to_owned()
                } else {
                    identifier.to_owned()
                }
            } else {
                title.to_owned()
            };
            ticket_labels.insert(payload.ticket_id.as_str().to_owned(), label);
        }
    }
    ticket_labels
}

fn project_label_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    work_item_repo: &HashMap<WorkItemId, String>,
) -> String {
    session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.project_id.as_ref())
        .map(|project_id: &ProjectId| project_id.as_str().trim().to_owned())
        .filter(|project_id| !project_id.is_empty())
        .or_else(|| {
            session
                .work_item_id
                .as_ref()
                .and_then(|work_item_id| work_item_repo.get(work_item_id).cloned())
        })
        .unwrap_or_else(|| "unknown-repo".to_owned())
}

fn ticket_label_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    ticket_labels: &HashMap<String, String>,
) -> String {
    let fallback = format!("session {}", session.id.as_str());
    let Some(work_item_id) = session.work_item_id.as_ref() else {
        return fallback;
    };
    let Some(work_item) = domain.work_items.get(work_item_id) else {
        return fallback;
    };
    let Some(ticket_id) = work_item.ticket_id.as_ref() else {
        return fallback;
    };
    ticket_labels
        .get(ticket_id.as_str())
        .cloned()
        .or_else(|| {
            let fallback = ticket_id.as_str().trim();
            if fallback.is_empty() {
                None
            } else {
                Some(fallback.to_owned())
            }
        })
        .unwrap_or(fallback)
}

fn is_open_session_status(status: Option<&WorkerSessionStatus>) -> bool {
    !matches!(
        status,
        Some(WorkerSessionStatus::Done) | Some(WorkerSessionStatus::Crashed)
    )
}

fn session_status_label(status: Option<&WorkerSessionStatus>) -> &'static str {
    match status {
        Some(WorkerSessionStatus::Running) => "running",
        Some(WorkerSessionStatus::WaitingForUser) => "waiting",
        Some(WorkerSessionStatus::Blocked) => "blocked",
        Some(WorkerSessionStatus::Done) => "done",
        Some(WorkerSessionStatus::Crashed) => "crashed",
        None => "unknown",
    }
}

fn workflow_badge_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
) -> String {
    session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.workflow_state.as_ref())
        .map(workflow_state_to_badge_label)
        .or_else(|| {
            if matches!(session.status.as_ref(), Some(WorkerSessionStatus::Running)) {
                Some("planning".to_owned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| session_status_label(session.status.as_ref()).to_owned())
}

fn session_state_group_for_session(
    session: &SessionProjection,
    domain: &ProjectionState,
    fallback_badge: &str,
) -> SessionStateGroup {
    let workflow_state = session
        .work_item_id
        .as_ref()
        .and_then(|work_item_id| domain.work_items.get(work_item_id))
        .and_then(|work_item| work_item.workflow_state.as_ref());
    match workflow_state {
        Some(WorkflowState::New | WorkflowState::Planning) => SessionStateGroup::Planning,
        Some(WorkflowState::Implementing | WorkflowState::Testing | WorkflowState::PRDrafted) => {
            SessionStateGroup::Implementation
        }
        Some(
            WorkflowState::AwaitingYourReview
            | WorkflowState::ReadyForReview
            | WorkflowState::InReview
            | WorkflowState::Merging,
        ) => SessionStateGroup::Review,
        Some(WorkflowState::Done | WorkflowState::Abandoned) => {
            SessionStateGroup::Other("complete".to_owned())
        }
        None => SessionStateGroup::Other(fallback_badge.to_owned()),
    }
}

fn session_turn_is_running(
    terminal_session_states: &HashMap<WorkerSessionId, TerminalViewState>,
    session: &SessionProjection,
) -> bool {
    matches!(session.status.as_ref(), Some(WorkerSessionStatus::Running))
        && session_turn_is_active(terminal_session_states, &session.id)
}

fn workflow_state_to_badge_label(state: &WorkflowState) -> String {
    match state {
        WorkflowState::New | WorkflowState::Planning => "planning",
        WorkflowState::Implementing | WorkflowState::Testing | WorkflowState::PRDrafted => {
            "implementation"
        }
        WorkflowState::AwaitingYourReview
        | WorkflowState::ReadyForReview
        | WorkflowState::InReview
        | WorkflowState::Merging => "review",
        WorkflowState::Done | WorkflowState::Abandoned => "complete",
    }
    .to_owned()
}

fn render_ticket_picker_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    overlay: &TicketPickerOverlayState,
) {
    let content = render_ticket_picker_overlay_text(overlay);
    let Some(popup) = ticket_picker_popup(anchor_area) else {
        return;
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("start ticket").borders(Borders::ALL)),
        popup,
    );
    if popup.width > 4 && popup.height > 4 {
        let input_height: u16 = if overlay.new_ticket_mode { 4 } else { 3 };
        if popup.height <= input_height.saturating_add(1) {
            return;
        }
        let input_area = Rect {
            x: popup.x.saturating_add(1),
            y: popup
                .y
                .saturating_add(popup.height.saturating_sub(input_height.saturating_add(1))),
            width: popup.width.saturating_sub(2),
            height: input_height,
        };
        if overlay.new_ticket_mode {
            let project_area = Rect {
                x: popup.x.saturating_add(1),
                y: input_area.y.saturating_sub(1),
                width: popup.width.saturating_sub(2),
                height: 1,
            };
            let selected_project = overlay
                .selected_project_name()
                .unwrap_or_else(|| "No Project".to_owned());
            frame.render_widget(
                Paragraph::new(format!("Assigned project: {selected_project}")),
                project_area,
            );
            let mut state = overlay.new_ticket_brief_input.clone();
            state.focused = true;
            TextArea::new()
                .label("describe ticket")
                .placeholder("ticket summary")
                .wrap_mode(WrapMode::Soft)
                .render_stateful(frame, input_area, &mut state);
        } else if overlay.has_repository_prompt() {
            let mut state = overlay.repository_prompt_input.clone();
            state.focused = true;
            Input::new(&state)
                .label("path")
                .placeholder("repository path")
                .render_stateful(frame, input_area);
        }
    }
}

const DEFAULT_TERMINAL_INPUT_HEIGHT: u16 = 6;
const INPUT_PANEL_OUTER_BORDER_HEIGHT: u16 = 2;
const NEEDS_INPUT_NOTE_HEIGHT: u16 = 3;
const NEEDS_INPUT_HELP_HEIGHT: u16 = 1;
const NEEDS_INPUT_ERROR_HEIGHT: u16 = 1;
const NEEDS_INPUT_QUESTION_MIN_HEIGHT: u16 = 3;
const NEEDS_INPUT_QUESTION_MAX_EXPANDED_HEIGHT: u16 = 7;
const NEEDS_INPUT_QUESTION_MAX_DEFAULT_HEIGHT: u16 = 4;
const NEEDS_INPUT_CHOICE_MIN_ROWS_DEFAULT: usize = 1;
const NEEDS_INPUT_CHOICE_MAX_ROWS_DEFAULT: usize = 4;
const NEEDS_INPUT_CHOICE_MAX_ROWS_EXPANDED: usize = 12;

fn terminal_input_pane_height(
    center_height: u16,
    center_width: u16,
    prompt: Option<&NeedsInputComposerState>,
) -> u16 {
    let Some(prompt) = prompt else {
        return DEFAULT_TERMINAL_INPUT_HEIGHT;
    };
    let Some(question) = prompt.current_question() else {
        return DEFAULT_TERMINAL_INPUT_HEIGHT;
    };
    let is_plan_prompt = expanded_needs_input_layout_active(prompt);
    let question_height = needs_input_question_height(question, center_width, is_plan_prompt);
    let choice_height = needs_input_choice_height(question, center_width, is_plan_prompt);
    let mut inner_required = question_height
        .saturating_add(choice_height)
        .saturating_add(NEEDS_INPUT_NOTE_HEIGHT)
        .saturating_add(NEEDS_INPUT_HELP_HEIGHT);
    if prompt.error.is_some() {
        inner_required = inner_required.saturating_add(NEEDS_INPUT_ERROR_HEIGHT);
    }
    let mut target_height = inner_required.saturating_add(INPUT_PANEL_OUTER_BORDER_HEIGHT);
    if is_plan_prompt {
        target_height = target_height.max(DEFAULT_TERMINAL_INPUT_HEIGHT.saturating_mul(2));
    }
    let max_height = center_height.saturating_sub(4).max(3);
    target_height.clamp(3, max_height)
}

fn expanded_needs_input_layout_active(prompt: &NeedsInputComposerState) -> bool {
    if prompt.questions.len() > 1 {
        return true;
    }

    if prompt
        .prompt_id
        .to_ascii_lowercase()
        .contains("plan")
    {
        return true;
    }

    prompt.questions.iter().any(|question| {
        let id_has_plan = question.id.to_ascii_lowercase().contains("plan");
        let header_has_plan = question.header.to_ascii_lowercase().contains("plan");
        let question_has_plan = question.question.to_ascii_lowercase().contains("plan");
        id_has_plan || header_has_plan || question_has_plan
    })
}

fn needs_input_question_height(
    question: &BackendNeedsInputQuestion,
    panel_width: u16,
    expanded_layout: bool,
) -> u16 {
    let question_text = format!("{} | {}", question.header, question.question);
    let content_width = panel_width.saturating_sub(6);
    let wrapped_rows = wrapped_row_count(question_text.as_str(), content_width);
    let max_height = if expanded_layout {
        NEEDS_INPUT_QUESTION_MAX_EXPANDED_HEIGHT
    } else {
        NEEDS_INPUT_QUESTION_MAX_DEFAULT_HEIGHT
    };
    wrapped_rows
        .saturating_add(2)
        .clamp(NEEDS_INPUT_QUESTION_MIN_HEIGHT, max_height)
}

fn needs_input_choice_height(
    question: &BackendNeedsInputQuestion,
    panel_width: u16,
    expanded_layout: bool,
) -> u16 {
    let content_width = panel_width.saturating_sub(10);
    let Some(options) = question.options.as_ref().filter(|options| !options.is_empty()) else {
        return 3;
    };

    let mut rows = 0usize;
    for option in options {
        let label = compact_focus_card_text(option.label.as_str());
        let line = if option.description.trim().is_empty() {
            format!("> [ ] {label}")
        } else {
            let description = compact_focus_card_text(option.description.as_str());
            format!("> [ ] {label} - {description}")
        };
        rows = rows.saturating_add(usize::from(wrapped_row_count(line.as_str(), content_width)));
    }
    let max_rows = if expanded_layout {
        NEEDS_INPUT_CHOICE_MAX_ROWS_EXPANDED
    } else {
        NEEDS_INPUT_CHOICE_MAX_ROWS_DEFAULT
    };
    let clamped_rows = rows.clamp(NEEDS_INPUT_CHOICE_MIN_ROWS_DEFAULT, max_rows);
    u16::try_from(clamped_rows).unwrap_or(NEEDS_INPUT_CHOICE_MAX_EXPANDED_HEIGHT_U16)
        .saturating_add(2)
}

const NEEDS_INPUT_CHOICE_MAX_EXPANDED_HEIGHT_U16: u16 = 12;

fn wrapped_row_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let mut rows = 0usize;
    for segment in text.split('\n') {
        let chars = segment.chars().count();
        let wrapped = if chars == 0 {
            1
        } else {
            chars.div_ceil(width)
        };
        rows = rows.saturating_add(wrapped.max(1));
    }
    u16::try_from(rows).unwrap_or(u16::MAX)
}

fn render_terminal_needs_input_panel(
    frame: &mut ratatui::Frame<'_>,
    input_area: Rect,
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
    prompt: &NeedsInputComposerState,
    focused: bool,
) {
    if input_area.height < 3 {
        return;
    }
    let Some(question) = prompt.current_question() else {
        return;
    };

    let labels = session_display_labels(domain, session_id);
    let title = format!(
        "input required | {} | {} / {}",
        labels.compact_label,
        prompt.current_question_index + 1,
        prompt.questions.len()
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(input_area);
    frame.render_widget(block, input_area);

    let options_len = question
        .options
        .as_ref()
        .map(|options| options.len())
        .unwrap_or(0);
    let is_plan_prompt = expanded_needs_input_layout_active(prompt);
    let question_height = needs_input_question_height(question, input_area.width, is_plan_prompt);
    let choice_height = needs_input_choice_height(question, input_area.width, is_plan_prompt);

    let mut constraints = vec![
        Constraint::Length(question_height),
        Constraint::Min(choice_height),
        Constraint::Length(NEEDS_INPUT_NOTE_HEIGHT),
    ];
    if prompt.error.is_some() {
        constraints.push(Constraint::Length(NEEDS_INPUT_ERROR_HEIGHT));
    }
    constraints.push(Constraint::Length(NEEDS_INPUT_HELP_HEIGHT));
    let sections = Layout::vertical(constraints).split(inner);
    let question_area = sections[0];
    let choice_area = sections[1];
    let note_area = sections[2];

    let header = format!("{} | {}", question.header, question.question);
    frame.render_widget(
        Paragraph::new(compact_focus_card_text(header.as_str())).block(
            Block::default()
                .title("question")
                .borders(Borders::ALL),
        ),
        question_area,
    );

    if let Some(options) = question.options.as_ref().filter(|options| !options.is_empty()) {
        let selected_index = prompt.select_state.selected_index;
        let highlighted_index = prompt
            .select_state
            .highlighted_index
            .min(options.len().saturating_sub(1));
        let visible_rows = usize::from(choice_area.height.saturating_sub(2)).max(1);
        let mut start = highlighted_index.saturating_sub(visible_rows / 2);
        if start + visible_rows > options.len() {
            start = options.len().saturating_sub(visible_rows);
        }
        let end = (start + visible_rows).min(options.len());

        let mut lines = Vec::new();
        for (idx, option) in options[start..end].iter().enumerate() {
            let option_index = start + idx;
            let cursor = if option_index == highlighted_index {
                ">"
            } else {
                " "
            };
            let selected = if selected_index == Some(option_index) {
                "[x]"
            } else {
                "[ ]"
            };
            let label = compact_focus_card_text(option.label.as_str());
            let line = if option.description.trim().is_empty() {
                format!("{cursor} {selected} {label}")
            } else {
                let description = compact_focus_card_text(option.description.as_str());
                format!("{cursor} {selected} {label} - {description}")
            };
            let style = if option_index == highlighted_index {
                Style::default().fg(Color::LightCyan)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(line, style)));
        }

        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().title("choices").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            choice_area,
        );
    } else {
        frame.render_widget(
            Paragraph::new("No predefined options. Enter a response note.")
                .block(Block::default().title("choices").borders(Borders::ALL)),
            choice_area,
        );
    }

    let mut note_input_state = prompt.note_input_state.clone();
    note_input_state.focused = prompt.interaction_active && prompt.note_insert_mode && focused;
    let note_input = Input::new(&note_input_state)
        .label("note")
        .placeholder("Optional with selection; required when no options")
        .with_border(true);
    let _ = note_input.render_stateful(frame, note_area);
    if prompt.interaction_active && prompt.note_insert_mode && focused {
        if let Some((x, y)) = needs_input_note_cursor(note_area, prompt) {
            frame.set_cursor_position((x, y));
        }
    }

    let mut index = 3usize;
    if let Some(error) = prompt.error.as_ref() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                compact_focus_card_text(error.as_str()),
                Style::default().fg(Color::LightRed),
            )),
            sections[index],
        );
        index += 1;
    }
    let help = if prompt.interaction_active {
        format!(
            "{}Enter: {} | h/l or arrows: question | i: note insert | Esc: normal mode",
            if options_len > 0 {
                "j/k: option | "
            } else {
                ""
            },
            if options_len > 0 {
                if prompt.has_next_question() {
                    "select option + next question"
                } else {
                    "select option + submit"
                }
            } else if prompt.has_next_question() {
                "next question"
            } else {
                "submit"
            },
        )
    } else {
        "Press i or use j/k + Enter to activate input | Esc: normal mode".to_owned()
    };
    frame.render_widget(
        Paragraph::new(help).wrap(Wrap { trim: false }),
        sections[index],
    );
}

fn needs_input_note_cursor(
    note_area: Rect,
    prompt: &NeedsInputComposerState,
) -> Option<(u16, u16)> {
    if !prompt.note_insert_mode {
        return None;
    }

    let inner_x = note_area.x.saturating_add(1);
    let inner_y = note_area.y.saturating_add(1);
    let line_prefix = 1u16;
    let note_len = prompt.note_input_state.text.chars().count();
    let inner_width = note_area.width.saturating_sub(2);
    let max_offset = usize::from(inner_width.saturating_sub(2));
    let x_offset = note_len.min(max_offset);
    let cursor_x = inner_x
        .saturating_add(line_prefix)
        .saturating_add(u16::try_from(x_offset).ok()?);
    let cursor_y = inner_y;
    Some((cursor_x, cursor_y))
}

fn render_worktree_diff_modal(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    domain: &ProjectionState,
    modal: &WorktreeDiffModalState,
) {
    let Some(popup) = worktree_diff_modal_popup(anchor_area) else {
        return;
    };

    let labels = session_display_labels(domain, &modal.session_id);
    let title = format!(
        "diff | ticket: {} | base: {}",
        labels.compact_label,
        modal.base_branch
    );
    frame.render_widget(Clear, popup);
    if modal.loading {
        frame.render_widget(
            Paragraph::new(Text::from(Line::from(Span::styled(
                "Loading diff...",
                Style::default().fg(Color::LightBlue),
            ))))
            .block(Block::default().title(title).borders(Borders::ALL)),
            popup,
        );
        return;
    }

    if let Some(error) = modal.error.as_deref() {
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(Span::styled(
                    "Failed to load diff:",
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(error.to_owned()),
            ]))
            .block(Block::default().title(title).borders(Borders::ALL)),
            popup,
        );
        return;
    }

    if modal.content.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Text::from(Line::from(Span::styled(
                "(No diff against base branch.)",
                Style::default().fg(Color::DarkGray),
            ))))
            .block(Block::default().title(title).borders(Borders::ALL)),
            popup,
        );
        return;
    }

    let outer = Block::default().title(title).borders(Borders::ALL);
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);
    if inner.width < 20 || inner.height < 6 {
        return;
    }

    let panes = if inner.width < 90 {
        Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(inner)
    } else {
        Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(inner)
    };
    if panes.len() < 2 {
        return;
    }

    let files = parse_diff_file_summaries(modal.content.as_str());
    let left = render_diff_file_list(modal, files.as_slice());
    frame.render_widget(
        Paragraph::new(left).block(Block::default().title("files").borders(Borders::ALL)),
        panes[0],
    );

    let right = render_selected_file_diff(modal, files.as_slice());
    let selected_path = files
        .get(modal.selected_file_index.min(files.len().saturating_sub(1)))
        .map(|entry| entry.path.as_str())
        .unwrap_or("(no file)");
    let right_title = format!("diff | {selected_path}");
    let diff_viewport_rows = usize::from(panes[1].height.saturating_sub(2)).max(1);
    frame.render_widget(
        Paragraph::new(right)
            .scroll((
                worktree_diff_modal_scroll(modal, files.as_slice(), diff_viewport_rows),
                0,
            ))
            .block(Block::default().title(right_title).borders(Borders::ALL)),
        panes[1],
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffRenderedLineKind {
    FileHeader,
    HunkHeader,
    Addition,
    Deletion,
    Context,
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffRenderedLine {
    text: String,
    kind: DiffRenderedLineKind,
    file_path: Option<String>,
    new_line_no: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffAdditionBlock {
    start_index: usize,
    end_index: usize,
    file_path: String,
    start_new_line: usize,
    end_new_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffFileSummary {
    path: String,
    added: usize,
    removed: usize,
    start_index: usize,
    end_index: usize,
    addition_blocks: Vec<DiffAdditionBlock>,
}

fn parse_rendered_diff_lines(content: &str) -> Vec<DiffRenderedLine> {
    let mut lines = Vec::new();
    let mut current_file: Option<String> = None;
    let mut old_line: Option<usize> = None;
    let mut new_line: Option<usize> = None;

    for raw_line in content.lines() {
        let line = sanitize_terminal_display_text(raw_line);
        if let Some(path) = parse_diff_file_header_path(line.as_str()) {
            current_file = Some(path);
            old_line = None;
            new_line = None;
        } else if let Some(path) = parse_diff_plus_plus_plus_path(line.as_str()) {
            current_file = Some(path);
        }

        if let Some((old_start, new_start)) = parse_unified_diff_hunk_header(line.as_str()) {
            old_line = Some(old_start);
            new_line = Some(new_start);
            lines.push(DiffRenderedLine {
                text: format!("{:>6} {:>6} {}", "", "", line),
                kind: DiffRenderedLineKind::HunkHeader,
                file_path: current_file.clone(),
                new_line_no: None,
            });
            continue;
        }

        let (old_cell, new_cell, kind, new_line_no) =
            if line.starts_with('+') && !line.starts_with("+++") {
                let new_cell = format_line_no(new_line);
                let selected_new_line = new_line;
                if let Some(value) = new_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                (
                    String::new(),
                    new_cell,
                    DiffRenderedLineKind::Addition,
                    selected_new_line,
                )
            } else if line.starts_with('-') && !line.starts_with("---") {
                let old_cell = format_line_no(old_line);
                if let Some(value) = old_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                (
                    old_cell,
                    String::new(),
                    DiffRenderedLineKind::Deletion,
                    None,
                )
            } else if line.starts_with(' ') {
                let old_cell = format_line_no(old_line);
                let new_cell = format_line_no(new_line);
                let selected_new_line = new_line;
                if let Some(value) = old_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                if let Some(value) = new_line.as_mut() {
                    *value = value.saturating_add(1);
                }
                (
                    old_cell,
                    new_cell,
                    DiffRenderedLineKind::Context,
                    selected_new_line,
                )
            } else if line.starts_with("diff --git ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
            {
                (
                    String::new(),
                    String::new(),
                    DiffRenderedLineKind::FileHeader,
                    None,
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    DiffRenderedLineKind::Metadata,
                    None,
                )
            };

        lines.push(DiffRenderedLine {
            text: format!("{:>6} {:>6} {}", old_cell, new_cell, line),
            kind,
            file_path: current_file.clone(),
            new_line_no,
        });
    }

    lines
}

fn parse_diff_addition_blocks(content: &str) -> Vec<DiffAdditionBlock> {
    let lines = parse_rendered_diff_lines(content);
    let mut blocks = Vec::new();
    let mut active: Option<DiffAdditionBlock> = None;

    for (index, line) in lines.into_iter().enumerate() {
        if line.kind == DiffRenderedLineKind::Addition {
            let Some(path) = line.file_path else {
                continue;
            };
            let Some(new_line_no) = line.new_line_no else {
                continue;
            };
            if let Some(current) = active.as_mut() {
                if current.file_path == path && current.end_index + 1 == index {
                    current.end_index = index;
                    current.end_new_line = new_line_no;
                    continue;
                }
                blocks.push(current.clone());
            }
            active = Some(DiffAdditionBlock {
                start_index: index,
                end_index: index,
                file_path: path,
                start_new_line: new_line_no,
                end_new_line: new_line_no,
            });
        } else if let Some(current) = active.take() {
            blocks.push(current);
        }
    }

    if let Some(current) = active {
        blocks.push(current);
    }
    blocks
}

fn parse_diff_file_summaries(content: &str) -> Vec<DiffFileSummary> {
    let lines = parse_rendered_diff_lines(content);
    if lines.is_empty() {
        return Vec::new();
    }

    let blocks = parse_diff_addition_blocks(content);
    let mut files = Vec::<DiffFileSummary>::new();

    for (index, line) in lines.iter().enumerate() {
        let Some(path) = line.file_path.as_deref() else {
            continue;
        };
        let needs_new = files
            .last()
            .map(|entry| entry.path.as_str() != path)
            .unwrap_or(true);
        if needs_new {
            files.push(DiffFileSummary {
                path: path.to_owned(),
                added: 0,
                removed: 0,
                start_index: index,
                end_index: index,
                addition_blocks: Vec::new(),
            });
        }
        if let Some(current) = files.last_mut() {
            current.end_index = index;
            match line.kind {
                DiffRenderedLineKind::Addition => current.added = current.added.saturating_add(1),
                DiffRenderedLineKind::Deletion => {
                    current.removed = current.removed.saturating_add(1)
                }
                _ => {}
            }
        }
    }

    for file in files.iter_mut() {
        file.addition_blocks = blocks
            .iter()
            .filter(|block| block.file_path == file.path)
            .cloned()
            .collect();
    }

    files
}

fn render_diff_file_list(modal: &WorktreeDiffModalState, files: &[DiffFileSummary]) -> Text<'static> {
    if files.is_empty() {
        return Text::from(Line::from(Span::styled(
            "(No changed files.)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let mut rendered = Vec::with_capacity(files.len());
    let selected = modal.selected_file_index.min(files.len().saturating_sub(1));
    for (index, file) in files.iter().enumerate() {
        let mut base = Style::default().fg(Color::Gray);
        if index == selected {
            base = if modal.focus == DiffPaneFocus::Files {
                base.bg(Color::Rgb(59, 66, 82)).add_modifier(Modifier::BOLD)
            } else {
                base.bg(Color::Rgb(47, 52, 63))
            };
        }
        rendered.push(Line::from(vec![
            Span::styled(format!("{:<36}", file.path), base),
            Span::styled(format!(" +{}", file.added), base.fg(Color::LightGreen)),
            Span::styled(format!(" -{}", file.removed), base.fg(Color::LightRed)),
        ]));
    }
    Text::from(rendered)
}

fn selected_file_and_hunk_range(
    modal: &WorktreeDiffModalState,
    files: &[DiffFileSummary],
) -> Option<(usize, usize, Option<(usize, usize)>)> {
    let file = files.get(modal.selected_file_index.min(files.len().saturating_sub(1)))?;
    let hunk = file
        .addition_blocks
        .get(
            modal
                .selected_hunk_index
                .min(file.addition_blocks.len().saturating_sub(1)),
        )
        .map(|block| (block.start_index, block.end_index));
    Some((file.start_index, file.end_index, hunk))
}

fn render_selected_file_diff(modal: &WorktreeDiffModalState, files: &[DiffFileSummary]) -> Text<'static> {
    let parsed = parse_rendered_diff_lines(modal.content.as_str());
    let Some((start, end, selected_hunk)) = selected_file_and_hunk_range(modal, files) else {
        return Text::from(Line::from(Span::styled(
            "(No diff content for selected file.)",
            Style::default().fg(Color::DarkGray),
        )));
    };

    let mut rendered = Vec::with_capacity(end.saturating_sub(start).saturating_add(1));
    for (global_index, line) in parsed.iter().enumerate().skip(start).take(end - start + 1) {
        let mut style = diff_rendered_line_style(line.kind);
        if let Some((hunk_start, hunk_end)) = selected_hunk {
            if global_index >= hunk_start && global_index <= hunk_end {
                style = if modal.focus == DiffPaneFocus::Diff {
                    style.bg(Color::Rgb(59, 66, 82)).add_modifier(Modifier::BOLD)
                } else {
                    style.bg(Color::Rgb(47, 52, 63))
                };
            }
        }
        rendered.push(Line::from(Span::styled(line.text.clone(), style)));
    }
    Text::from(rendered)
}

fn worktree_diff_modal_scroll(
    modal: &WorktreeDiffModalState,
    files: &[DiffFileSummary],
    viewport_rows: usize,
) -> u16 {
    let Some((file_start, file_end, selected_hunk)) = selected_file_and_hunk_range(modal, files) else {
        return 0;
    };
    let center = selected_hunk
        .map(|(start, end)| start + (end.saturating_sub(start) / 2))
        .unwrap_or(file_start);
    let local_line = center.saturating_sub(file_start);
    let preferred_scroll = local_line.saturating_sub(3);
    let line_count = file_end.saturating_sub(file_start).saturating_add(1);
    let max_scroll = line_count.saturating_sub(viewport_rows.max(1));
    preferred_scroll.min(max_scroll).min(u16::MAX as usize) as u16
}

fn diff_rendered_line_style(kind: DiffRenderedLineKind) -> Style {
    match kind {
        DiffRenderedLineKind::FileHeader => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::HunkHeader => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::Addition => Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::Deletion => Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD),
        DiffRenderedLineKind::Context => Style::default().fg(Color::Gray),
        DiffRenderedLineKind::Metadata => Style::default().fg(Color::DarkGray),
    }
}

fn format_line_no(value: Option<usize>) -> String {
    value.map(|entry| entry.to_string()).unwrap_or_default()
}

fn parse_diff_file_header_path(line: &str) -> Option<String> {
    if !line.starts_with("diff --git ") {
        return None;
    }
    let mut parts = line.split_whitespace();
    let _diff = parts.next()?;
    let _git = parts.next()?;
    let _a_path = parts.next()?;
    let b_path = parts.next()?;
    normalize_diff_path(b_path)
}

fn parse_diff_plus_plus_plus_path(line: &str) -> Option<String> {
    let raw = line.strip_prefix("+++ ")?;
    normalize_diff_path(raw.trim())
}

fn normalize_diff_path(raw: &str) -> Option<String> {
    let path = raw.trim();
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    let normalized = path.strip_prefix("b/").unwrap_or(path).trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_owned())
    }
}

fn worktree_diff_modal_line_count(modal: &WorktreeDiffModalState) -> usize {
    if modal.loading || modal.error.is_some() || modal.content.trim().is_empty() {
        return 1;
    }
    let files = parse_diff_file_summaries(modal.content.as_str());
    if let Some((start, end, _)) = selected_file_and_hunk_range(modal, files.as_slice()) {
        return end.saturating_sub(start).saturating_add(1).max(1);
    }
    parse_rendered_diff_lines(modal.content.as_str()).len().max(1)
}

fn collect_selected_worktree_diff_refs(
    modal: &WorktreeDiffModalState,
) -> Result<Vec<String>, String> {
    if modal.loading {
        return Err("diff selection unavailable while loading".to_owned());
    }
    if modal.error.is_some() {
        return Err("diff selection unavailable: fix diff load error first".to_owned());
    }
    if modal.content.trim().is_empty() {
        return Err("diff selection unavailable: no diff content loaded".to_owned());
    }
    if parse_rendered_diff_lines(modal.content.as_str()).is_empty() {
        return Err("diff selection unavailable: no diff content loaded".to_owned());
    }
    let files = parse_diff_file_summaries(modal.content.as_str());
    let Some(file) = files.get(modal.selected_file_index.min(files.len().saturating_sub(1))) else {
        return Err("diff selection unavailable: no changed files in diff".to_owned());
    };
    let Some(block) = file.addition_blocks.get(
        modal
            .selected_hunk_index
            .min(file.addition_blocks.len().saturating_sub(1)),
    ) else {
        return Err("diff selection unavailable: no addition blocks in diff".to_owned());
    };

    Ok(vec![format!(
        "[{} {}:{}]",
        block.file_path, block.start_new_line, block.end_new_line
    )])
}

fn parse_unified_diff_hunk_header(line: &str) -> Option<(usize, usize)> {
    if !line.starts_with("@@") {
        return None;
    }
    let rest = line.strip_prefix("@@")?;
    let (header, _) = rest.split_once("@@")?;
    let mut parts = header.split_whitespace();
    let old_part = parts.next()?;
    let new_part = parts.next()?;
    let old_start = parse_unified_diff_hunk_coord(old_part, '-')?;
    let new_start = parse_unified_diff_hunk_coord(new_part, '+')?;
    Some((old_start, new_start))
}

fn parse_unified_diff_hunk_coord(part: &str, expected_prefix: char) -> Option<usize> {
    let raw = part.strip_prefix(expected_prefix)?;
    let start = raw.split_once(',').map(|(left, _)| left).unwrap_or(raw);
    start.parse::<usize>().ok()
}

fn worktree_diff_modal_popup(anchor_area: Rect) -> Option<Rect> {
    if anchor_area.width < 60 || anchor_area.height < 12 {
        return None;
    }

    let width = ((anchor_area.width as f32) * 0.90).round() as u16;
    let height = ((anchor_area.height as f32) * 0.86).round() as u16;
    let width = width.clamp(60, anchor_area.width.saturating_sub(2));
    let height = height.clamp(12, anchor_area.height.saturating_sub(2));

    Some(Rect {
        x: anchor_area.x + (anchor_area.width.saturating_sub(width)) / 2,
        y: anchor_area.y + (anchor_area.height.saturating_sub(height)) / 2,
        width,
        height,
    })
}

fn render_review_merge_confirm_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
) {
    let labels = session_display_labels(domain, session_id);
    let content = format!(
        "Merge pull request and reconcile completion?\n\nTicket: {}\n\nEnter/y: confirm merge\nEsc/n: cancel",
        labels.full_label
    );
    let Some(popup) = review_merge_confirm_popup(anchor_area) else {
        return;
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("review merge").borders(Borders::ALL)),
        popup,
    );
}

fn render_archive_session_confirm_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    domain: &ProjectionState,
    session_id: &WorkerSessionId,
) {
    let labels = session_display_labels(domain, session_id);
    let content = format!(
        "Archive selected session and clean up worktree/branch?\n\nTicket: {}\n\nEnter/y: confirm archive\nEsc/n: cancel",
        labels.full_label
    );
    let Some(popup) = review_merge_confirm_popup(anchor_area) else {
        return;
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content)
            .block(Block::default().title("archive session").borders(Borders::ALL)),
        popup,
    );
}

fn render_ticket_archive_confirm_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    ticket: &TicketSummary,
) {
    let content = format!(
        "Archive ticket from start ticket window?\n\nTicket: {} - {}\n\nEnter/y: confirm archive\nEsc/n: cancel",
        ticket.identifier,
        compact_focus_card_text(ticket.title.as_str())
    );
    let Some(popup) = review_merge_confirm_popup(anchor_area) else {
        return;
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content)
            .block(Block::default().title("archive ticket").borders(Borders::ALL)),
        popup,
    );
}

fn review_merge_confirm_popup(anchor_area: Rect) -> Option<Rect> {
    if anchor_area.width < 40 || anchor_area.height < 8 {
        return None;
    }
    let width = ((anchor_area.width as f32) * 0.55).round() as u16;
    let height = 8u16;
    let width = width.clamp(40, anchor_area.width.saturating_sub(2));
    let height = height.min(anchor_area.height.saturating_sub(2));
    Some(Rect {
        x: anchor_area.x + (anchor_area.width.saturating_sub(width)) / 2,
        y: anchor_area.y + (anchor_area.height.saturating_sub(height)) / 2,
        width,
        height,
    })
}

fn ticket_picker_popup(anchor_area: Rect) -> Option<Rect> {
    if anchor_area.width < 20 || anchor_area.height < 10 {
        return None;
    }

    let width = ((anchor_area.width as f32) * 0.78).round() as u16;
    let height = ((anchor_area.height as f32) * 0.82).round() as u16;
    let width = width.clamp(20, anchor_area.width.saturating_sub(2));
    let height = height.clamp(10, anchor_area.height.saturating_sub(2));

    Some(Rect {
        x: anchor_area.x + (anchor_area.width.saturating_sub(width)) / 2,
        y: anchor_area.y + (anchor_area.height.saturating_sub(height)) / 2,
        width,
        height,
    })
}

fn render_which_key_overlay(
    frame: &mut ratatui::Frame<'_>,
    anchor_area: Rect,
    overlay: &WhichKeyOverlayState,
) {
    let content = render_which_key_overlay_text(overlay);
    let Some(popup) = which_key_overlay_popup(anchor_area, content.as_str()) else {
        return;
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(content).block(Block::default().title("which-key").borders(Borders::ALL)),
        popup,
    );
}

fn which_key_overlay_popup(anchor_area: Rect, content: &str) -> Option<Rect> {
    let line_count = content.lines().count();
    if line_count == 0 {
        return None;
    }

    let max_width = content.lines().map(|line| line.chars().count()).max()?;
    let desired_width = u16::try_from(max_width)
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let desired_height = u16::try_from(line_count)
        .unwrap_or(u16::MAX)
        .saturating_add(2);

    let width = desired_width.min(anchor_area.width);
    let height = desired_height.min(anchor_area.height);
    if width < 4 || height < 3 {
        return None;
    }

    Some(Rect {
        x: anchor_area
            .x
            .saturating_add(anchor_area.width.saturating_sub(width)),
        y: anchor_area
            .y
            .saturating_add(anchor_area.height.saturating_sub(height)),
        width,
        height,
    })
}

fn render_which_key_overlay_text(overlay: &WhichKeyOverlayState) -> String {
    let mut lines = Vec::with_capacity(overlay.hints.len() + 2);
    let prefix = overlay
        .prefix
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(label) = overlay.group_label.as_deref() {
        lines.push(format!("{prefix}  ({label})"));
    } else {
        lines.push(prefix);
    }

    lines.extend(
        overlay
            .hints
            .iter()
            .map(|hint| format!("{:>8}  {}", hint.key, hint.description)),
    );
    lines.join("\n")
}

fn ticket_picker_priority_states_from_env() -> Vec<String> {
    ticket_picker_priority_states_config_value()
}

fn normalize_ticket_state(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_ticket_project(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn is_unfinished_ticket_state(state: &str) -> bool {
    let normalized = normalize_ticket_state(state);
    !matches!(
        normalized.as_str(),
        "done" | "completed" | "canceled" | "cancelled"
    )
}

fn ticket_project_name(ticket: &TicketSummary) -> String {
    ticket
        .project
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("No Project")
        .to_owned()
}

fn group_tickets_by_project(
    tickets: Vec<TicketSummary>,
    project_names: Vec<String>,
    priority_states: &[String],
    collapsed_projects: &HashSet<String>,
) -> Vec<TicketProjectGroup> {
    let mut projects = tickets
        .into_iter()
        .filter(|ticket| is_unfinished_ticket_state(ticket.state.as_str()))
        .fold(
            Vec::<(String, Vec<TicketSummary>)>::new(),
            |mut project_buckets, ticket| {
                let project_name = ticket_project_name(&ticket);
                if let Some(existing) = project_buckets
                    .iter_mut()
                    .find(|(name, _)| name.eq_ignore_ascii_case(project_name.as_str()))
                {
                    existing.1.push(ticket);
                } else {
                    project_buckets.push((project_name, vec![ticket]));
                }
                project_buckets
            },
        );

    for project_name in project_names {
        let project_name = project_name.trim().to_owned();
        if project_name.is_empty() {
            continue;
        }
        if !projects
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(project_name.as_str()))
        {
            projects.push((project_name, Vec::new()));
        }
    }

    projects.sort_by(|left, right| {
        normalize_ticket_project(left.0.as_str()).cmp(&normalize_ticket_project(right.0.as_str()))
    });

    projects
        .into_iter()
        .map(|(project, project_tickets)| TicketProjectGroup {
            collapsed: collapsed_projects.contains(&normalize_ticket_project(project.as_str())),
            project,
            status_groups: group_tickets_by_status(project_tickets, priority_states),
        })
        .collect()
}

fn group_tickets_by_status(
    tickets: Vec<TicketSummary>,
    priority_states: &[String],
) -> Vec<TicketStatusGroup> {
    let mut groups =
        tickets
            .into_iter()
            .fold(Vec::<TicketStatusGroup>::new(), |mut groups, ticket| {
                if let Some(existing) = groups
                    .iter_mut()
                    .find(|group| group.status.eq_ignore_ascii_case(ticket.state.as_str()))
                {
                    existing.tickets.push(ticket);
                } else {
                    groups.push(TicketStatusGroup {
                        status: ticket.state.clone(),
                        tickets: vec![ticket],
                    });
                }
                groups
            });

    for group in &mut groups {
        group.tickets.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.identifier.cmp(&right.identifier))
        });
    }

    groups.sort_by(|left, right| {
        left.status
            .to_ascii_lowercase()
            .cmp(&right.status.to_ascii_lowercase())
    });

    let mut ordered = Vec::with_capacity(groups.len());
    for priority_state in priority_states {
        if let Some(index) = groups.iter().position(|group| {
            normalize_ticket_state(group.status.as_str())
                == normalize_ticket_state(priority_state.as_str())
        }) {
            ordered.push(groups.remove(index));
        }
    }
    ordered.extend(groups);
    ordered
}
