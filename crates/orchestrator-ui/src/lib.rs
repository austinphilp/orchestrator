use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    pub status: String,
    pub inbox_rows: Vec<UiInboxRow>,
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
            UiInboxRow {
                inbox_item_id: item.id.clone(),
                work_item_id: item.work_item_id.clone(),
                kind: item.kind.clone(),
                title: item.title.clone(),
                resolved: item.resolved,
                workflow_state: work_item.and_then(|entry| entry.workflow_state.clone()),
                session_id,
                session_status,
            }
        })
        .collect::<Vec<_>>();
    inbox_rows.sort_by(|a, b| {
        a.resolved
            .cmp(&b.resolved)
            .then_with(|| a.inbox_item_id.as_str().cmp(b.inbox_item_id.as_str()))
    });

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
    let center_pane = project_center_pane(&active_center, &inbox_rows, domain);

    UiState {
        status: status.to_owned(),
        inbox_rows,
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

fn project_center_pane(
    active_center: &CenterView,
    inbox_rows: &[UiInboxRow],
    domain: &ProjectionState,
) -> CenterPaneState {
    match active_center {
        CenterView::InboxView => {
            let unresolved = inbox_rows.iter().filter(|item| !item.resolved).count();
            let lines = vec![
                format!("{unresolved} unresolved inbox items"),
                "Enter: open focus card".to_owned(),
                "t: open terminal for selected item".to_owned(),
                "Backspace: minimize top view".to_owned(),
            ];
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
        self.selected_inbox_index = selected_index;
        self.selected_inbox_item_id = selected_index.map(|index| rows[index].inbox_item_id.clone());
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
                    "status: {} | j/k or arrows: select | Enter: focus | t: terminal push | Backspace: pop | q/esc: quit",
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

    ui_state
        .inbox_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let selected = if Some(index) == ui_state.selected_inbox_index {
                ">"
            } else {
                " "
            };
            let resolved = if row.resolved { "x" } else { " " };
            format!("{selected} [{resolved}] {:?}: {}", row.kind, row.title)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_center_panel(ui_state: &UiState) -> String {
    let mut lines = Vec::with_capacity(ui_state.center_pane.lines.len() + 2);
    lines.push(format!("Stack: {}", ui_state.center_stack_label()));
    lines.push(String::new());
    lines.extend(ui_state.center_pane.lines.iter().cloned());
    lines.join("\n")
}

fn handle_key_press(shell_state: &mut UiShellState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => true,
        KeyCode::Down | KeyCode::Char('j') => {
            shell_state.move_selection(1);
            false
        }
        KeyCode::Up | KeyCode::Char('k') => {
            shell_state.move_selection(-1);
            false
        }
        KeyCode::Enter => {
            shell_state.open_focus_card_for_selected();
            false
        }
        KeyCode::Char('t') => {
            shell_state.open_terminal_for_selected();
            false
        }
        KeyCode::Backspace => {
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
}
