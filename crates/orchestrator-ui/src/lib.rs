use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use orchestrator_core::OrchestrationState;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

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

    pub fn run(&mut self, state: &OrchestrationState) -> io::Result<()> {
        loop {
            self.terminal.draw(|frame| {
                let area = frame.area();
                let layout = Layout::vertical([Constraint::Percentage(100)]);
                let [main] = layout.areas(area);
                let body = Paragraph::new(format!(
                    "Orchestrator status: {}\n\nPress q to quit.",
                    state.status
                ))
                .block(Block::default().title("orchestrator").borders(Borders::ALL))
                .cyan();
                frame.render_widget(body, main);
            })?;

            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press
                        && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    {
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
