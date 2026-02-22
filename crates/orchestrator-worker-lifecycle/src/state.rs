#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkerSessionState {
    #[default]
    Starting,
    Running,
    Done,
    Crashed,
    Killed,
}

impl WorkerSessionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Crashed | Self::Killed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkerSessionVisibility {
    #[default]
    Focused,
    Background,
}
