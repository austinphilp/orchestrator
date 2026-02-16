use crate::{RuntimeError, RuntimeResult, TerminalSnapshot};

pub(crate) struct TerminalEmulator {
    parser: vt100::Parser,
}

impl TerminalEmulator {
    pub(crate) fn new(cols: u16, rows: u16, scrollback_limit: usize) -> RuntimeResult<Self> {
        if cols == 0 || rows == 0 {
            return Err(RuntimeError::Configuration(
                "terminal emulator requires non-zero rows and columns".to_owned(),
            ));
        }

        Ok(Self {
            parser: vt100::Parser::new(rows, cols, scrollback_limit),
        })
    }

    pub(crate) fn process(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parser.process(bytes);
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> RuntimeResult<()> {
        if cols == 0 || rows == 0 {
            return Err(RuntimeError::Configuration(
                "terminal emulator resize requires non-zero rows and columns".to_owned(),
            ));
        }

        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> TerminalSnapshot {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let lines = screen.rows(0, cols).collect();

        TerminalSnapshot {
            cols,
            rows,
            cursor_col,
            cursor_row,
            lines,
        }
    }

    #[cfg(test)]
    fn max_scrollback_rows(&mut self) -> usize {
        let screen = self.parser.screen_mut();
        let previous_position = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let max = screen.scrollback();
        screen.set_scrollback(previous_position);
        max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_line(snapshot: &TerminalSnapshot, row: usize) -> &str {
        snapshot
            .lines
            .get(row)
            .map_or("", String::as_str)
            .trim_end()
    }

    #[test]
    fn renders_cursor_movement_and_overwrite() {
        let mut emulator = TerminalEmulator::new(20, 4, 64).expect("create emulator");
        emulator.process(b"hello\x1b[2DXY");

        let snapshot = emulator.snapshot();
        assert_eq!(snapshot_line(&snapshot, 0), "helXY");
        assert_eq!(snapshot.cursor_row, 0);
        assert_eq!(snapshot.cursor_col, 5);
    }

    #[test]
    fn renders_clear_line_sequences() {
        let mut emulator = TerminalEmulator::new(20, 4, 64).expect("create emulator");
        emulator.process(b"abc\r\x1b[2Kz");

        let snapshot = emulator.snapshot();
        assert_eq!(snapshot_line(&snapshot, 0), "z");
        assert_eq!(snapshot.cursor_col, 1);
    }

    #[test]
    fn tracks_alternate_screen_buffer_switches() {
        let mut emulator = TerminalEmulator::new(20, 4, 64).expect("create emulator");
        emulator.process(b"main\r\nline2");
        emulator.process(b"\x1b[?1049halt");

        let alternate = emulator.snapshot();
        assert_eq!(snapshot_line(&alternate, 0), "alt");
        assert_eq!(snapshot_line(&alternate, 1), "");

        emulator.process(b"\x1b[?1049l");
        let primary = emulator.snapshot();
        assert_eq!(snapshot_line(&primary, 0), "main");
        assert_eq!(snapshot_line(&primary, 1), "line2");
    }

    #[test]
    fn limits_scrollback_rows() {
        let mut emulator = TerminalEmulator::new(10, 2, 3).expect("create emulator");
        for i in 0..20 {
            emulator.process(format!("line-{i}\n").as_bytes());
        }

        assert_eq!(emulator.max_scrollback_rows(), 3);
    }
}
