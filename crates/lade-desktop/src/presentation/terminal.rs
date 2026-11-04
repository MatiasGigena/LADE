use super::state::model::{Lifecycle, ObjectiveId, ObjectiveSummary};

pub const SCROLLBACK_LINES: usize = 2000;
pub const MAX_PARAMS: usize = 16;
pub const MIN_COLS: usize = 20;
pub const MAX_COLS: usize = 500;
pub const MIN_ROWS: usize = 5;
pub const MAX_ROWS: usize = 200;
pub const INPUT_CHUNK_CHARS: usize = 4096;
pub const MAX_PASTE_BYTES: usize = 256 << 10;

mod input;
mod screen;

pub use input::{Mods, TermKey, encode_key, input_chunks, paste_text};
pub use screen::Screen;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalMode {
    ReadWrite,
    ReadOnly,
}

impl TerminalMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadWrite => "Read-write: the Objective worktree",
            Self::ReadOnly => "Read-only: the worktree is frozen for review",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEnd {
    Exited {
        code: Option<u64>,
        signal: Option<u64>,
    },
    Closed,
    ReviewFreeze,
    Lifecycle,
    Failed,
    ConnectionLost,
}

impl TerminalEnd {
    pub fn message(&self) -> String {
        match self {
            Self::Exited {
                code: Some(code), ..
            } => format!("The shell exited with status {code}."),
            Self::Exited {
                signal: Some(signal),
                ..
            } => format!("The shell was ended by signal {signal}."),
            Self::Exited { .. } => "The shell exited.".to_owned(),
            Self::Closed => "Terminal closed.".to_owned(),
            Self::ReviewFreeze => "Closed for review: a frozen Change Set excludes writers. Open a read-only terminal to inspect it.".to_owned(),
            Self::Lifecycle => "Closed: the Objective no longer admits this terminal (its lifecycle or worktree changed).".to_owned(),
            Self::Failed => "The terminal failed.".to_owned(),
            Self::ConnectionLost => "Closed: the connection to the service was lost. Terminals end with the window's control of the service.".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Opening,
    Open,
    Ended(TerminalEnd),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSession {
    pub terminal_id: Option<String>,
    pub objective_id: ObjectiveId,
    pub mode: Option<TerminalMode>,
    pub status: TerminalStatus,
    pub screen: Screen,
    pub last_seq: u64,
}

impl TerminalSession {
    pub fn opening(objective_id: ObjectiveId, cols: usize, rows: usize) -> Self {
        Self {
            terminal_id: None,
            objective_id,
            mode: None,
            status: TerminalStatus::Opening,
            screen: Screen::new(cols, rows),
            last_seq: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.status == TerminalStatus::Open
    }

    pub fn output(&mut self, seq: u64, data: &str, dropped: u64) {
        if seq <= self.last_seq {
            return;
        }
        if seq != self.last_seq + 1 && self.last_seq != 0 {
            self.screen.mark_gap(0);
        }
        self.last_seq = seq;
        if dropped > 0 {
            self.screen.mark_gap(dropped);
        }
        self.screen.feed(data);
    }
}

pub fn awaits_first_start(summary: &ObjectiveSummary) -> bool {
    summary.lifecycle == Lifecycle::Defined
}

#[cfg(test)]
mod tests;
