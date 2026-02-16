use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use orchestrator_core::{
    InboxItemId, InboxItemKind, ProjectionState, WorkItemId, WorkerSessionId, WorkerSessionStatus,
    WorkflowState,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CenterView {
    InboxView,
    FocusCardView { inbox_item_id: InboxItemId },
    TerminalView { session_id: WorkerSessionId },
}

impl CenterView {
    fn label(&self) -> String {
        match self {
            Self::InboxView => "Inbox".to_owned(),
            Self::FocusCardView { inbox_item_id } => {
                format!("FocusCard({})", inbox_item_id.as_str())
            }
            Self::TerminalView { session_id } => format!("Terminal({})", session_id.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStack {
    center: Vec<CenterView>,
}

impl Default for ViewStack {
    fn default() -> Self {
        Self {
            center: vec![CenterView::InboxView],
        }
    }
}

impl ViewStack {
    pub fn active_center(&self) -> Option<&CenterView> {
        self.center.last()
    }

    pub fn center_views(&self) -> &[CenterView] {
        &self.center
    }

    pub fn replace_center(&mut self, view: CenterView) {
        self.center.clear();
        self.center.push(view);
    }

    pub fn push_center(&mut self, view: CenterView) -> bool {
        if self.active_center() == Some(&view) {
            return false;
        }
        self.center.push(view);
        true
    }

    pub fn pop_center(&mut self) -> bool {
        if self.center.len() <= 1 {
            return false;
        }
        self.center.pop();
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiInboxRow {
    pub inbox_item_id: InboxItemId,
    pub work_item_id: WorkItemId,
    pub kind: InboxItemKind,
    pub priority_score: i32,
    pub priority_band: InboxPriorityBand,
    pub batch_kind: InboxBatchKind,
    pub title: String,
    pub resolved: bool,
    pub workflow_state: Option<WorkflowState>,
    pub session_id: Option<WorkerSessionId>,
    pub session_status: Option<WorkerSessionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CenterPaneState {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InboxPriorityBand {
    Urgent,
    Attention,
    Background,
}

impl InboxPriorityBand {
    fn label(self) -> &'static str {
        match self {
            Self::Urgent => "Urgent",
            Self::Attention => "Attention",
            Self::Background => "Background",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Urgent => 0,
            Self::Attention => 1,
            Self::Background => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxBatchKind {
    DecideOrUnblock,
    Approvals,
    ReviewReady,
    FyiDigest,
}

impl InboxBatchKind {
    const ORDERED: [InboxBatchKind; 4] = [
        InboxBatchKind::DecideOrUnblock,
        InboxBatchKind::Approvals,
        InboxBatchKind::ReviewReady,
        InboxBatchKind::FyiDigest,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::DecideOrUnblock => "Decide / Unblock",
            Self::Approvals => "Approvals",
            Self::ReviewReady => "PR Reviews",
            Self::FyiDigest => "FYI Digest",
        }
    }

    fn hotkey(self) -> char {
        match self {
            Self::DecideOrUnblock => '1',
            Self::Approvals => '2',
            Self::ReviewReady => '3',
            Self::FyiDigest => '4',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiBatchSurface {
    pub kind: InboxBatchKind,
    pub unresolved_count: usize,
    pub total_count: usize,
    pub first_unresolved_index: Option<usize>,
    pub first_any_index: Option<usize>,
}

impl UiBatchSurface {
    fn selection_index(&self) -> Option<usize> {
        self.first_unresolved_index.or(self.first_any_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    pub status: String,
    pub inbox_rows: Vec<UiInboxRow>,
    pub inbox_batch_surfaces: Vec<UiBatchSurface>,
    pub selected_inbox_index: Option<usize>,
    pub selected_inbox_item_id: Option<InboxItemId>,
    pub selected_work_item_id: Option<WorkItemId>,
    pub selected_session_id: Option<WorkerSessionId>,
    pub center_view_stack: Vec<CenterView>,
    pub center_pane: CenterPaneState,
}

impl UiState {
    pub fn center_stack_label(&self) -> String {
        self.center_view_stack
            .iter()
            .map(CenterView::label)
            .collect::<Vec<_>>()
            .join(" > ")
    }
}

pub fn project_ui_state(
    status: &str,
    domain: &ProjectionState,
    view_stack: &ViewStack,
    preferred_selected_inbox: Option<usize>,
    preferred_selected_inbox_item_id: Option<&InboxItemId>,
) -> UiState {
    let mut inbox_rows = domain
        .inbox_items
        .values()
        .map(|item| {
            let work_item = domain.work_items.get(&item.work_item_id);
            let session_id = work_item.and_then(|entry| entry.session_id.clone());
            let session_status = session_id
                .as_ref()
                .and_then(|id| domain.sessions.get(id))
                .and_then(|session| session.status.clone());
            let workflow_state = work_item.and_then(|entry| entry.workflow_state.clone());
            let priority_score = inbox_priority_score(
                &item.kind,
                item.resolved,
                workflow_state.as_ref(),
                session_status.as_ref(),
            );
            let priority_band = inbox_priority_band(priority_score, item.resolved);
            UiInboxRow {
                inbox_item_id: item.id.clone(),
                work_item_id: item.work_item_id.clone(),
                kind: item.kind.clone(),
                priority_score,
                priority_band,
                batch_kind: inbox_batch_kind(&item.kind),
                title: item.title.clone(),
                resolved: item.resolved,
                workflow_state,
                session_id,
                session_status,
            }
        })
        .collect::<Vec<_>>();
    inbox_rows.sort_by(|a, b| {
        a.priority_band
            .rank()
            .cmp(&b.priority_band.rank())
            .then_with(|| b.priority_score.cmp(&a.priority_score))
            .then_with(|| a.resolved.cmp(&b.resolved))
            .then_with(|| a.inbox_item_id.as_str().cmp(b.inbox_item_id.as_str()))
    });
    let inbox_batch_surfaces = build_batch_surfaces(&inbox_rows);

    let selected_inbox_index = resolve_selected_inbox(
        preferred_selected_inbox,
        preferred_selected_inbox_item_id,
        &inbox_rows,
    );
    let selected_row = selected_inbox_index.map(|idx| &inbox_rows[idx]);
    let selected_inbox_item_id = selected_row.map(|row| row.inbox_item_id.clone());
    let selected_work_item_id = selected_row.map(|row| row.work_item_id.clone());
    let selected_session_id = selected_row.and_then(|row| row.session_id.clone());

    let center_view_stack = view_stack.center_views().to_vec();
    let active_center = view_stack
        .active_center()
        .cloned()
        .unwrap_or(CenterView::InboxView);
    let center_pane =
        project_center_pane(&active_center, &inbox_rows, &inbox_batch_surfaces, domain);

    UiState {
        status: status.to_owned(),
        inbox_rows,
        inbox_batch_surfaces,
        selected_inbox_index,
        selected_inbox_item_id,
        selected_work_item_id,
        selected_session_id,
        center_view_stack,
        center_pane,
    }
}

fn resolve_selected_inbox(
    preferred_index: Option<usize>,
    preferred_inbox_item_id: Option<&InboxItemId>,
    inbox_rows: &[UiInboxRow],
) -> Option<usize> {
    if inbox_rows.is_empty() {
        return None;
    }

    if let Some(preferred_inbox_item_id) = preferred_inbox_item_id {
        if let Some(index) = inbox_rows
            .iter()
            .position(|row| &row.inbox_item_id == preferred_inbox_item_id)
        {
            return Some(index);
        }
    }

    Some(preferred_index.unwrap_or(0).min(inbox_rows.len() - 1))
}

fn inbox_priority_score(
    kind: &InboxItemKind,
    resolved: bool,
    workflow_state: Option<&WorkflowState>,
    session_status: Option<&WorkerSessionStatus>,
) -> i32 {
    let mut score = match kind {
        InboxItemKind::NeedsDecision => 100,
        InboxItemKind::Blocked => 95,
        InboxItemKind::NeedsApproval => 75,
        InboxItemKind::ReadyForReview => 70,
        InboxItemKind::FYI => 30,
    };

    if resolved {
        score -= 500;
    }

    if matches!(session_status, Some(WorkerSessionStatus::WaitingForUser)) {
        score += 20;
    }
    if matches!(session_status, Some(WorkerSessionStatus::Blocked)) {
        score += 15;
    }

    if matches!(
        workflow_state,
        Some(WorkflowState::AwaitingYourReview | WorkflowState::ReadyForReview)
    ) {
        score += 10;
    }

    score
}

fn inbox_priority_band(score: i32, resolved: bool) -> InboxPriorityBand {
    if resolved {
        return InboxPriorityBand::Background;
    }

    if score >= 95 {
        InboxPriorityBand::Urgent
    } else if score >= 60 {
        InboxPriorityBand::Attention
    } else {
        InboxPriorityBand::Background
    }
}

fn inbox_batch_kind(kind: &InboxItemKind) -> InboxBatchKind {
    match kind {
        InboxItemKind::NeedsDecision | InboxItemKind::Blocked => InboxBatchKind::DecideOrUnblock,
        InboxItemKind::NeedsApproval => InboxBatchKind::Approvals,
        InboxItemKind::ReadyForReview => InboxBatchKind::ReviewReady,
        InboxItemKind::FYI => InboxBatchKind::FyiDigest,
    }
}

fn build_batch_surfaces(rows: &[UiInboxRow]) -> Vec<UiBatchSurface> {
    InboxBatchKind::ORDERED
        .iter()
        .map(|kind| UiBatchSurface {
            kind: *kind,
            unresolved_count: rows
                .iter()
                .filter(|row| row.batch_kind == *kind && !row.resolved)
                .count(),
            total_count: rows.iter().filter(|row| row.batch_kind == *kind).count(),
            first_unresolved_index: rows
                .iter()
                .position(|row| row.batch_kind == *kind && !row.resolved),
            first_any_index: rows.iter().position(|row| row.batch_kind == *kind),
        })
        .collect()
}

fn project_center_pane(
    active_center: &CenterView,
    inbox_rows: &[UiInboxRow],
    inbox_batch_surfaces: &[UiBatchSurface],
    domain: &ProjectionState,
) -> CenterPaneState {
    match active_center {
        CenterView::InboxView => {
            let unresolved = inbox_rows.iter().filter(|item| !item.resolved).count();
            let mut lines = vec![format!("{unresolved} unresolved inbox items")];
            lines.extend(
                inbox_batch_surfaces
                    .iter()
                    .filter(|surface| surface.total_count > 0)
                    .map(|surface| {
                        format!(
                            "[{}] {}: {} unresolved / {} total",
                            surface.kind.hotkey(),
                            surface.kind.label(),
                            surface.unresolved_count,
                            surface.total_count
                        )
                    }),
            );
            lines.push("j/k or arrows: move selection".to_owned());
            lines.push("Tab/Shift+Tab: cycle batch lanes".to_owned());
            lines.push("g/G: jump first/last item".to_owned());
            lines.push("Enter: open focus card".to_owned());
            lines.push("t: open terminal for selected item".to_owned());
            lines.push("Backspace: minimize top view".to_owned());
            CenterPaneState {
                title: "Inbox View".to_owned(),
                lines,
            }
        }
        CenterView::FocusCardView { inbox_item_id } => {
            if let Some(item) = inbox_rows
                .iter()
                .find(|row| &row.inbox_item_id == inbox_item_id)
            {
                let workflow = item
                    .workflow_state
                    .as_ref()
                    .map(|state| format!("{state:?}"))
                    .unwrap_or_else(|| "Unknown".to_owned());
                let session = match (&item.session_id, &item.session_status) {
                    (Some(session_id), Some(status)) => {
                        format!("{} ({status:?})", session_id.as_str())
                    }
                    (Some(session_id), None) => format!("{} (Unknown)", session_id.as_str()),
                    (None, _) => "None".to_owned(),
                };
                CenterPaneState {
                    title: format!("Focus Card {}", inbox_item_id.as_str()),
                    lines: vec![
                        format!("Title: {}", item.title),
                        format!("Kind: {:?}", item.kind),
                        format!("Work item: {}", item.work_item_id.as_str()),
                        format!("Workflow: {workflow}"),
                        format!("Session: {session}"),
                    ],
                }
            } else {
                CenterPaneState {
                    title: format!("Focus Card {}", inbox_item_id.as_str()),
                    lines: vec!["Selected inbox item is not available.".to_owned()],
                }
            }
        }
        CenterView::TerminalView { session_id } => {
            let lines = if let Some(session) = domain.sessions.get(session_id) {
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
                vec![
                    format!("Session: {}", session_id.as_str()),
                    format!("Status: {status}"),
                    format!("Latest checkpoint: {checkpoint}"),
                    "PTY embedding lands in a later ticket.".to_owned(),
                ]
            } else {
                vec![
                    format!("Session: {}", session_id.as_str()),
                    "Session state is unavailable in current projection.".to_owned(),
                ]
            };

            CenterPaneState {
                title: format!("Terminal {}", session_id.as_str()),
                lines,
            }
        }
    }
}

#[derive(Debug, Clone)]
struct UiShellState {
    status: String,
    domain: ProjectionState,
    selected_inbox_index: Option<usize>,
    selected_inbox_item_id: Option<InboxItemId>,
    view_stack: ViewStack,
}

impl UiShellState {
    fn new(status: String, domain: ProjectionState) -> Self {
        Self {
            status,
            domain,
            selected_inbox_index: None,
            selected_inbox_item_id: None,
            view_stack: ViewStack::default(),
        }
    }

    fn ui_state(&self) -> UiState {
        project_ui_state(
            &self.status,
            &self.domain,
            &self.view_stack,
            self.selected_inbox_index,
            self.selected_inbox_item_id.as_ref(),
        )
    }

    fn move_selection(&mut self, delta: isize) {
        let ui_state = self.ui_state();
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
            self.set_selection(None, &ui_state.inbox_rows);
            return;
        }
        self.set_selection(Some(0), &ui_state.inbox_rows);
    }

    fn jump_to_last_item(&mut self) {
        let ui_state = self.ui_state();
        if ui_state.inbox_rows.is_empty() {
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
        if let Some(session_id) = ui_state.selected_session_id {
            self.view_stack
                .push_center(CenterView::TerminalView { session_id });
        }
    }

    fn minimize_center_view(&mut self) {
        let _ = self.view_stack.pop_center();
    }

    fn set_selection(&mut self, selected_index: Option<usize>, rows: &[UiInboxRow]) {
        let valid_selected_index = selected_index.filter(|index| *index < rows.len());
        self.selected_inbox_index = valid_selected_index;
        self.selected_inbox_item_id =
            valid_selected_index.map(|index| rows[index].inbox_item_id.clone());
    }
}

pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Ui {
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    pub fn run(&mut self, status: &str, projection: &ProjectionState) -> io::Result<()> {
        let mut shell_state = UiShellState::new(status.to_owned(), projection.clone());
        loop {
            let ui_state = shell_state.ui_state();
            self.terminal.draw(|frame| {
                let area = frame.area();
                let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
                let [main, footer] = layout.areas(area);
                let main_layout =
                    Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]);
                let [inbox_area, center_area] = main_layout.areas(main);

                let inbox_text = render_inbox_panel(&ui_state);
                frame.render_widget(
                    Paragraph::new(inbox_text)
                        .block(Block::default().title("inbox").borders(Borders::ALL)),
                    inbox_area,
                );

                let center_text = render_center_panel(&ui_state);
                frame.render_widget(
                    Paragraph::new(center_text).block(
                        Block::default()
                            .title(ui_state.center_pane.title.as_str())
                            .borders(Borders::ALL),
                    ),
                    center_area,
                );

                let footer_text = format!(
                    "status: {} | j/k: select | Tab/S-Tab: batch cycle | 1-4: batch jump | g/G: first/last | Enter: focus | t: terminal | Backspace: pop | q/esc: quit",
                    ui_state.status
                );
                frame.render_widget(
                    Paragraph::new(footer_text)
                        .block(Block::default().title("shell").borders(Borders::ALL)),
                    footer,
                );
            })?;

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
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

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

fn render_center_panel(ui_state: &UiState) -> String {
    let mut lines = Vec::with_capacity(ui_state.center_pane.lines.len() + 2);
    lines.push(format!("Stack: {}", ui_state.center_stack_label()));
    lines.push(String::new());
    lines.extend(ui_state.center_pane.lines.iter().cloned());
    lines.join("\n")
}

fn handle_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> bool {
    let modifiers = key.modifiers;
    let no_modifiers = modifiers.is_empty();
    let shift_only = modifiers == KeyModifiers::SHIFT;

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Char('q') if no_modifiers => true,
        KeyCode::Down if no_modifiers => {
            shell_state.move_selection(1);
            false
        }
        KeyCode::Char('j') if no_modifiers => {
            shell_state.move_selection(1);
            false
        }
        KeyCode::Up if no_modifiers => {
            shell_state.move_selection(-1);
            false
        }
        KeyCode::Char('k') if no_modifiers => {
            shell_state.move_selection(-1);
            false
        }
        KeyCode::Tab if no_modifiers => {
            shell_state.cycle_batch(1);
            false
        }
        KeyCode::BackTab if no_modifiers => {
            shell_state.cycle_batch(-1);
            false
        }
        KeyCode::Char('g') if no_modifiers => {
            shell_state.jump_to_first_item();
            false
        }
        KeyCode::Char('G') if shift_only || no_modifiers => {
            shell_state.jump_to_last_item();
            false
        }
        KeyCode::Char('g') if shift_only => {
            shell_state.jump_to_last_item();
            false
        }
        KeyCode::Char('1') if no_modifiers => {
            shell_state.jump_to_batch(InboxBatchKind::DecideOrUnblock);
            false
        }
        KeyCode::Char('2') if no_modifiers => {
            shell_state.jump_to_batch(InboxBatchKind::Approvals);
            false
        }
        KeyCode::Char('3') if no_modifiers => {
            shell_state.jump_to_batch(InboxBatchKind::ReviewReady);
            false
        }
        KeyCode::Char('4') if no_modifiers => {
            shell_state.jump_to_batch(InboxBatchKind::FyiDigest);
            false
        }
        KeyCode::Enter if no_modifiers => {
            shell_state.open_focus_card_for_selected();
            false
        }
        KeyCode::Char('t') if no_modifiers => {
            shell_state.open_terminal_for_selected();
            false
        }
        KeyCode::Backspace if no_modifiers => {
            shell_state.minimize_center_view();
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        InboxItemProjection, SessionProjection, WorkItemProjection, WorkflowState,
    };

    fn sample_projection(with_session: bool) -> ProjectionState {
        let work_item_id = WorkItemId::new("wi-1");
        let session_id = WorkerSessionId::new("sess-1");
        let inbox_item_id = InboxItemId::new("inbox-1");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_id.clone(),
            WorkItemProjection {
                id: work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: with_session.then_some(session_id.clone()),
                worktree_id: None,
                inbox_items: vec![inbox_item_id.clone()],
                artifacts: vec![],
            },
        );

        if with_session {
            projection.sessions.insert(
                session_id.clone(),
                SessionProjection {
                    id: session_id,
                    work_item_id: Some(work_item_id.clone()),
                    status: Some(WorkerSessionStatus::Running),
                    latest_checkpoint: None,
                },
            );
        }

        projection.inbox_items.insert(
            inbox_item_id.clone(),
            InboxItemProjection {
                id: inbox_item_id,
                work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Review PR readiness".to_owned(),
                resolved: false,
            },
        );

        projection
    }

    fn triage_projection() -> ProjectionState {
        let mut projection = ProjectionState::default();
        let rows = vec![
            (
                "wi-decision",
                "inbox-decision",
                InboxItemKind::NeedsDecision,
                "Pick API shape",
            ),
            (
                "wi-approval",
                "inbox-approval",
                InboxItemKind::NeedsApproval,
                "Approve PR ready",
            ),
            (
                "wi-review",
                "inbox-review",
                InboxItemKind::ReadyForReview,
                "Review draft PR",
            ),
            ("wi-fyi", "inbox-fyi", InboxItemKind::FYI, "Progress digest"),
        ];

        for (work_item_raw, inbox_item_raw, kind, title) in rows {
            let work_item_id = WorkItemId::new(work_item_raw);
            let inbox_item_id = InboxItemId::new(inbox_item_raw);

            projection.work_items.insert(
                work_item_id.clone(),
                WorkItemProjection {
                    id: work_item_id.clone(),
                    ticket_id: None,
                    project_id: None,
                    workflow_state: Some(WorkflowState::Implementing),
                    session_id: None,
                    worktree_id: None,
                    inbox_items: vec![inbox_item_id.clone()],
                    artifacts: vec![],
                },
            );

            projection.inbox_items.insert(
                inbox_item_id.clone(),
                InboxItemProjection {
                    id: inbox_item_id,
                    work_item_id,
                    kind,
                    title: title.to_owned(),
                    resolved: false,
                },
            );
        }

        projection
    }

    #[test]
    fn center_stack_replace_push_and_pop_behavior() {
        let mut stack = ViewStack::default();
        assert_eq!(stack.active_center(), Some(&CenterView::InboxView));

        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-1"),
        });
        assert_eq!(stack.center_views().len(), 1);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::FocusCardView { .. })
        ));

        stack.push_center(CenterView::TerminalView {
            session_id: WorkerSessionId::new("sess-1"),
        });
        assert_eq!(stack.center_views().len(), 2);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));

        assert!(stack.pop_center());
        assert_eq!(stack.center_views().len(), 1);
        assert!(matches!(
            stack.active_center(),
            Some(CenterView::FocusCardView { .. })
        ));
        assert!(!stack.pop_center());
    }

    #[test]
    fn ui_state_projects_from_domain_and_center_stack() {
        let projection = sample_projection(true);
        let mut stack = ViewStack::default();
        stack.replace_center(CenterView::FocusCardView {
            inbox_item_id: InboxItemId::new("inbox-1"),
        });

        let ui_state = project_ui_state("ready", &projection, &stack, None, None);
        assert_eq!(ui_state.selected_inbox_index, Some(0));
        assert_eq!(
            ui_state
                .selected_inbox_item_id
                .as_ref()
                .map(|id| id.as_str())
                .expect("selected inbox"),
            "inbox-1"
        );
        assert_eq!(
            ui_state.center_stack_label(),
            "FocusCard(inbox-1)".to_owned()
        );
        assert!(ui_state
            .center_pane
            .lines
            .iter()
            .any(|line| line.contains("Review PR readiness")));
    }

    #[test]
    fn open_terminal_pushes_only_with_session_context() {
        let mut without_session = UiShellState::new("ready".to_owned(), sample_projection(false));
        without_session.open_focus_card_for_selected();
        let before = without_session.view_stack.center_views().to_vec();
        without_session.open_terminal_for_selected();
        assert_eq!(without_session.view_stack.center_views(), before.as_slice());

        let mut with_session = UiShellState::new("ready".to_owned(), sample_projection(true));
        with_session.open_focus_card_for_selected();
        with_session.open_terminal_for_selected();
        assert_eq!(with_session.view_stack.center_views().len(), 2);
        assert!(matches!(
            with_session.view_stack.active_center(),
            Some(CenterView::TerminalView { .. })
        ));

        with_session.open_terminal_for_selected();
        assert_eq!(with_session.view_stack.center_views().len(), 2);
    }

    #[test]
    fn projection_prefers_selected_inbox_item_id_over_stale_index() {
        let work_item_a = WorkItemId::new("wi-a");
        let work_item_b = WorkItemId::new("wi-b");
        let inbox_item_a = InboxItemId::new("inbox-a");
        let inbox_item_b = InboxItemId::new("inbox-b");

        let mut projection = ProjectionState::default();
        projection.work_items.insert(
            work_item_a.clone(),
            WorkItemProjection {
                id: work_item_a.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_a.clone()],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            work_item_b.clone(),
            WorkItemProjection {
                id: work_item_b.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![inbox_item_b.clone()],
                artifacts: vec![],
            },
        );
        projection.inbox_items.insert(
            inbox_item_a.clone(),
            InboxItemProjection {
                id: inbox_item_a.clone(),
                work_item_id: work_item_a,
                kind: InboxItemKind::NeedsApproval,
                title: "First".to_owned(),
                resolved: false,
            },
        );
        projection.inbox_items.insert(
            inbox_item_b.clone(),
            InboxItemProjection {
                id: inbox_item_b.clone(),
                work_item_id: work_item_b,
                kind: InboxItemKind::NeedsApproval,
                title: "Second".to_owned(),
                resolved: false,
            },
        );

        let ui_state = project_ui_state(
            "ready",
            &projection,
            &ViewStack::default(),
            Some(0),
            Some(&inbox_item_b),
        );
        assert_eq!(ui_state.selected_inbox_index, Some(1));
        assert_eq!(
            ui_state
                .selected_inbox_item_id
                .as_ref()
                .map(|item| item.as_str()),
            Some("inbox-b")
        );
    }

    #[test]
    fn inbox_view_projects_priority_bands_and_batch_surfaces() {
        let ui_state = project_ui_state(
            "ready",
            &triage_projection(),
            &ViewStack::default(),
            None,
            None,
        );
        let ordered_kinds = ui_state
            .inbox_rows
            .iter()
            .map(|row| row.kind.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_kinds,
            vec![
                InboxItemKind::NeedsDecision,
                InboxItemKind::NeedsApproval,
                InboxItemKind::ReadyForReview,
                InboxItemKind::FYI,
            ]
        );

        assert_eq!(
            ui_state
                .inbox_rows
                .iter()
                .map(|row| row.priority_band)
                .collect::<Vec<_>>(),
            vec![
                InboxPriorityBand::Urgent,
                InboxPriorityBand::Attention,
                InboxPriorityBand::Attention,
                InboxPriorityBand::Background,
            ]
        );

        assert_eq!(ui_state.inbox_batch_surfaces.len(), 4);
        assert_eq!(ui_state.inbox_batch_surfaces[0].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[1].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[2].unresolved_count, 1);
        assert_eq!(ui_state.inbox_batch_surfaces[3].unresolved_count, 1);

        let rendered = render_inbox_panel(&ui_state);
        assert!(rendered.contains("Batch lanes:"));
        assert!(rendered.contains("Urgent:"));
        assert!(rendered.contains("Attention:"));
        assert!(rendered.contains("Background:"));
    }

    #[test]
    fn batch_navigation_can_jump_and_cycle_surfaces() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());

        shell_state.jump_to_batch(InboxBatchKind::ReviewReady);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);

        shell_state.cycle_batch(1);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        shell_state.cycle_batch(-1);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);
    }

    #[test]
    fn keyboard_shortcuts_support_batch_and_range_navigation() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        handle_key_press(&mut shell_state, key(KeyCode::Char('4')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        handle_key_press(&mut shell_state, key(KeyCode::Char('g')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, key(KeyCode::Char('G')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::FYI);

        handle_key_press(&mut shell_state, key(KeyCode::BackTab));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::ReadyForReview);
    }

    #[test]
    fn keyboard_shortcuts_ignore_control_modified_chars() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let ctrl_key = |code| KeyEvent::new(code, KeyModifiers::CONTROL);

        let should_quit = handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('q')));
        assert!(!should_quit);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);

        handle_key_press(&mut shell_state, ctrl_key(KeyCode::Char('j')));
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsDecision);
    }

    #[test]
    fn batch_jump_prefers_unresolved_then_falls_back_to_first_any() {
        let mut projection = ProjectionState::default();
        let resolved_approval = InboxItemId::new("inbox-approval-resolved");
        let unresolved_approval = InboxItemId::new("inbox-approval-unresolved");
        let resolved_work_item_id = WorkItemId::new("wi-approval-resolved");
        let unresolved_work_item_id = WorkItemId::new("wi-approval-unresolved");

        projection.work_items.insert(
            resolved_work_item_id.clone(),
            WorkItemProjection {
                id: resolved_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![resolved_approval.clone()],
                artifacts: vec![],
            },
        );
        projection.work_items.insert(
            unresolved_work_item_id.clone(),
            WorkItemProjection {
                id: unresolved_work_item_id.clone(),
                ticket_id: None,
                project_id: None,
                workflow_state: Some(WorkflowState::Implementing),
                session_id: None,
                worktree_id: None,
                inbox_items: vec![unresolved_approval.clone()],
                artifacts: vec![],
            },
        );
        projection.inbox_items.insert(
            resolved_approval.clone(),
            InboxItemProjection {
                id: resolved_approval,
                work_item_id: resolved_work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Resolved approval".to_owned(),
                resolved: true,
            },
        );
        projection.inbox_items.insert(
            unresolved_approval.clone(),
            InboxItemProjection {
                id: unresolved_approval.clone(),
                work_item_id: unresolved_work_item_id,
                kind: InboxItemKind::NeedsApproval,
                title: "Unresolved approval".to_owned(),
                resolved: false,
            },
        );

        let mut shell_state = UiShellState::new("ready".to_owned(), projection.clone());
        shell_state.jump_to_batch(InboxBatchKind::Approvals);
        let selected = shell_state.ui_state();
        assert_eq!(
            selected
                .selected_inbox_item_id
                .as_ref()
                .map(|id| id.as_str())
                .expect("selected approval"),
            unresolved_approval.as_str()
        );

        projection
            .inbox_items
            .get_mut(&unresolved_approval)
            .expect("unresolved approval exists")
            .resolved = true;
        let mut shell_state = UiShellState::new("ready".to_owned(), projection);
        shell_state.jump_to_batch(InboxBatchKind::Approvals);
        let selected = shell_state.ui_state();
        let selected_kind = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .kind
            .clone();
        assert_eq!(selected_kind, InboxItemKind::NeedsApproval);
        let selected_title = selected.inbox_rows[selected.selected_inbox_index.expect("selected")]
            .title
            .clone();
        assert!(selected_title.contains("approval"));
    }

    #[test]
    fn set_selection_ignores_out_of_bounds_index() {
        let mut shell_state = UiShellState::new("ready".to_owned(), triage_projection());
        let rows = shell_state.ui_state().inbox_rows;
        shell_state.set_selection(Some(usize::MAX), &rows);
        assert_eq!(shell_state.selected_inbox_index, None);
        assert_eq!(shell_state.selected_inbox_item_id, None);
    }
}
