use std::collections::VecDeque;

use super::model::{Lifecycle, ObjectiveId, ObjectiveSummary};

pub const SCROLLBACK_LINES: usize = 2000;
pub const MAX_PARAMS: usize = 16;
pub const MIN_COLS: usize = 20;
pub const MAX_COLS: usize = 500;
pub const MIN_ROWS: usize = 5;
pub const MAX_ROWS: usize = 200;
pub const INPUT_CHUNK_CHARS: usize = 4096;
pub const MAX_PASTE_BYTES: usize = 256 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Parse {
    Ground,
    Escape,
    Charset,
    Csi,
    Osc,
    OscEscape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Saved {
    grid: Vec<Vec<char>>,
    row: usize,
    col: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    cols: usize,
    rows: usize,
    grid: Vec<Vec<char>>,
    scrollback: VecDeque<String>,
    row: usize,
    col: usize,
    wrap_pending: bool,
    saved_cursor: (usize, usize),
    top: usize,
    bottom: usize,
    alternate: Option<Saved>,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    state: Parse,
    params: String,
    pub skipped_bytes: u64,
}

fn blank(cols: usize) -> Vec<char> {
    vec![' '; cols]
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.clamp(MIN_COLS, MAX_COLS);
        let rows = rows.clamp(MIN_ROWS, MAX_ROWS);
        Self {
            cols,
            rows,
            grid: vec![blank(cols); rows],
            scrollback: VecDeque::new(),
            row: 0,
            col: 0,
            wrap_pending: false,
            saved_cursor: (0, 0),
            top: 0,
            bottom: rows - 1,
            alternate: None,
            application_cursor: false,
            bracketed_paste: false,
            cursor_visible: true,
            state: Parse::Ground,
            params: String::new(),
            skipped_bytes: 0,
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.clamp(MIN_COLS, MAX_COLS);
        let rows = rows.clamp(MIN_ROWS, MAX_ROWS);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        for line in &mut self.grid {
            line.resize(cols, ' ');
        }
        while self.grid.len() > rows
            && self.grid.len() > self.row + 1
            && self
                .grid
                .last()
                .is_some_and(|l| l.iter().all(|&c| c == ' '))
        {
            self.grid.pop();
        }
        while self.grid.len() > rows && self.row > 0 {
            let line = self.grid.remove(0);
            self.push_scrollback(&line);
            self.row -= 1;
        }
        self.grid.truncate(rows);
        while self.grid.len() < rows {
            self.grid.push(blank(cols));
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.wrap_pending = false;
    }

    pub fn feed(&mut self, text: &str) {
        for c in text.chars() {
            self.step(c);
        }
    }

    pub fn mark_gap(&mut self, bytes: u64) {
        self.skipped_bytes += bytes;
        let note = if bytes > 0 {
            format!("[… {bytes} bytes of output skipped …]")
        } else {
            "[… output skipped …]".to_owned()
        };
        self.state = Parse::Ground;
        self.feed("\r\n");
        self.feed(&note);
        self.feed("\r\n");
    }

    fn push_scrollback(&mut self, line: &[char]) {
        if self.alternate.is_some() {
            return;
        }
        let text: String = line.iter().collect();
        self.scrollback.push_back(text.trim_end().to_owned());
        while self.scrollback.len() > SCROLLBACK_LINES {
            self.scrollback.pop_front();
        }
    }

    fn step(&mut self, c: char) {
        match self.state {
            Parse::Ground => self.ground(c),
            Parse::Escape => self.escape(c),
            Parse::Charset => self.state = Parse::Ground,
            Parse::Csi => self.csi_char(c),
            Parse::Osc => match c {
                '\x07' => self.state = Parse::Ground,
                '\x1b' => self.state = Parse::OscEscape,
                _ => {}
            },
            Parse::OscEscape => {
                self.state = if c == '\\' { Parse::Ground } else { Parse::Osc };
            }
        }
    }

    fn ground(&mut self, c: char) {
        match c {
            '\x1b' => self.state = Parse::Escape,
            '\r' => {
                self.col = 0;
                self.wrap_pending = false;
            }
            '\n' | '\x0b' | '\x0c' => self.line_feed(),
            '\x08' => {
                self.col = self.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            '\t' => {
                let next = (self.col / 8 + 1) * 8;
                self.col = next.min(self.cols - 1);
            }
            c if c.is_control() => {}
            c => self.print(c),
        }
    }

    fn print(&mut self, c: char) {
        if self.wrap_pending {
            self.col = 0;
            self.line_feed();
            self.wrap_pending = false;
        }
        self.grid[self.row][self.col] = c;
        if self.col + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.col += 1;
        }
    }

    fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.row == self.bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            let line = self.grid.remove(self.top);
            if self.top == 0 {
                self.push_scrollback(&line);
            }
            self.grid.insert(self.bottom, blank(self.cols));
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            self.grid.remove(self.bottom);
            self.grid.insert(self.top, blank(self.cols));
        }
    }

    fn escape(&mut self, c: char) {
        self.state = Parse::Ground;
        match c {
            '[' => {
                self.params.clear();
                self.state = Parse::Csi;
            }
            ']' => self.state = Parse::Osc,
            '(' | ')' | '*' | '+' => self.state = Parse::Charset,
            '7' => self.saved_cursor = (self.row, self.col),
            '8' => self.restore_cursor(),
            'D' => self.line_feed(),
            'E' => {
                self.col = 0;
                self.line_feed();
            }
            'M' => {
                if self.row == self.top {
                    self.scroll_down(1);
                } else {
                    self.row = self.row.saturating_sub(1);
                }
            }
            'c' => {
                let (cols, rows) = (self.cols, self.rows);
                let scrollback = std::mem::take(&mut self.scrollback);
                *self = Self::new(cols, rows);
                self.scrollback = scrollback;
            }
            _ => {}
        }
    }

    fn csi_char(&mut self, c: char) {
        match c {
            '0'..='9' | ';' | '?' | '>' | '!' | ' ' | '"' | '\'' | '$' => {
                if self.params.len() < MAX_PARAMS * 6 {
                    self.params.push(c);
                }
            }
            '\x1b' => self.state = Parse::Escape,
            c if c.is_control() => self.ground(c),
            c => {
                self.state = Parse::Ground;
                let params = std::mem::take(&mut self.params);
                self.csi(&params, c);
            }
        }
    }

    fn csi(&mut self, raw: &str, command: char) {
        let private = raw.starts_with('?');
        let values: Vec<usize> = raw
            .trim_start_matches(['?', '>', '!'])
            .split(';')
            .take(MAX_PARAMS)
            .map(|p| {
                p.chars()
                    .filter(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<usize>()
                    .unwrap_or(0)
                    .min(9999)
            })
            .collect();
        let arg = |i: usize, default: usize| -> usize {
            match values.get(i).copied() {
                Some(0) | None => default,
                Some(v) => v,
            }
        };
        let last_col = self.cols - 1;
        let last_row = self.rows - 1;
        if command != 'm' {
            self.wrap_pending = false;
        }
        match command {
            'A' => {
                self.row = self
                    .row
                    .saturating_sub(arg(0, 1))
                    .max(self.top.min(self.row))
            }
            'B' => self.row = (self.row + arg(0, 1)).min(last_row),
            'C' => self.col = (self.col + arg(0, 1)).min(last_col),
            'D' => self.col = self.col.saturating_sub(arg(0, 1)),
            'E' => {
                self.row = (self.row + arg(0, 1)).min(last_row);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(arg(0, 1));
                self.col = 0;
            }
            'G' | '`' => self.col = (arg(0, 1) - 1).min(last_col),
            'd' => self.row = (arg(0, 1) - 1).min(last_row),
            'H' | 'f' => {
                self.row = (arg(0, 1) - 1).min(last_row);
                self.col = (arg(1, 1) - 1).min(last_col);
            }
            'J' => self.erase_display(values.first().copied().unwrap_or(0)),
            'K' => self.erase_line(values.first().copied().unwrap_or(0)),
            'L' => {
                if (self.top..=self.bottom).contains(&self.row) {
                    for _ in 0..arg(0, 1).min(self.bottom - self.row + 1) {
                        self.grid.remove(self.bottom);
                        self.grid.insert(self.row, blank(self.cols));
                    }
                }
            }
            'M' => {
                if (self.top..=self.bottom).contains(&self.row) {
                    for _ in 0..arg(0, 1).min(self.bottom - self.row + 1) {
                        self.grid.remove(self.row);
                        self.grid.insert(self.bottom, blank(self.cols));
                    }
                }
            }
            'P' => {
                let line = &mut self.grid[self.row];
                for _ in 0..arg(0, 1).min(self.cols - self.col) {
                    line.remove(self.col);
                    line.push(' ');
                }
            }
            '@' => {
                let line = &mut self.grid[self.row];
                for _ in 0..arg(0, 1).min(self.cols - self.col) {
                    line.pop();
                    line.insert(self.col, ' ');
                }
            }
            'X' => {
                let end = (self.col + arg(0, 1)).min(self.cols);
                self.grid[self.row][self.col..end].fill(' ');
            }
            'S' => self.scroll_up(arg(0, 1)),
            'T' => self.scroll_down(arg(0, 1)),
            'r' => {
                let top = arg(0, 1) - 1;
                let bottom = arg(1, self.rows) - 1;
                if top < bottom && bottom <= last_row {
                    self.top = top;
                    self.bottom = bottom;
                    self.row = 0;
                    self.col = 0;
                }
            }
            's' => self.saved_cursor = (self.row, self.col),
            'u' => self.restore_cursor(),
            'h' | 'l' if private => {
                let on = command == 'h';
                for mode in &values {
                    match mode {
                        1 => self.application_cursor = on,
                        25 => self.cursor_visible = on,
                        47 | 1047 | 1049 => self.set_alternate(on),
                        2004 => self.bracketed_paste = on,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn restore_cursor(&mut self) {
        let (row, col) = self.saved_cursor;
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line(0);
                for line in &mut self.grid[self.row + 1..] {
                    line.fill(' ');
                }
            }
            1 => {
                self.erase_line(1);
                for line in &mut self.grid[..self.row] {
                    line.fill(' ');
                }
            }
            2 => {
                for line in &mut self.grid {
                    line.fill(' ');
                }
            }
            3 => self.scrollback.clear(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let (row, col, cols) = (self.row, self.col, self.cols);
        let line = &mut self.grid[row];
        match mode {
            0 => line[col..].fill(' '),
            1 => line[..=col.min(cols - 1)].fill(' '),
            2 => line.fill(' '),
            _ => {}
        }
    }

    fn set_alternate(&mut self, on: bool) {
        match (on, self.alternate.is_some()) {
            (true, false) => {
                self.alternate = Some(Saved {
                    grid: std::mem::replace(&mut self.grid, vec![blank(self.cols); self.rows]),
                    row: self.row,
                    col: self.col,
                });
                self.row = 0;
                self.col = 0;
            }
            (false, true) => {
                if let Some(saved) = self.alternate.take() {
                    self.grid = saved.grid;
                    self.grid.resize(self.rows, blank(self.cols));
                    for line in &mut self.grid {
                        line.resize(self.cols, ' ');
                    }
                    self.row = saved.row.min(self.rows - 1);
                    self.col = saved.col.min(self.cols - 1);
                }
            }
            _ => {}
        }
    }

    pub fn screen_lines(&self) -> Vec<String> {
        self.grid
            .iter()
            .map(|line| line.iter().collect::<String>().trim_end().to_owned())
            .collect()
    }

    pub fn render(&self) -> (String, usize) {
        let mut text = String::new();
        let mut offset = 0;
        for line in &self.scrollback {
            text.push_str(line);
            text.push('\n');
        }
        let screen = self.screen_lines();
        for (row, line) in screen.iter().enumerate() {
            if row == self.row {
                let start = text.chars().count();
                text.push_str(line);
                let len = line.chars().count();
                text.extend(std::iter::repeat_n(' ', self.col.saturating_sub(len)));
                offset = start + self.col;
            } else {
                text.push_str(line);
            }
            if row + 1 < screen.len() {
                text.push('\n');
            }
        }
        (text, offset)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermKey {
    Char(char),
    Enter,
    Backspace,
    Tab,
    BackTab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

pub fn encode_key(key: TermKey, mods: Mods, application_cursor: bool) -> Option<String> {
    let cursor = |c: char| {
        if application_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let base = match key {
        TermKey::Char(c) if mods.control => control_char(c)?.to_string(),
        TermKey::Char(c) => c.to_string(),
        TermKey::Enter => "\r".into(),
        TermKey::Backspace => "\x7f".into(),
        TermKey::Tab => "\t".into(),
        TermKey::BackTab => "\x1b[Z".into(),
        TermKey::Escape => "\x1b".into(),
        TermKey::Up => cursor('A'),
        TermKey::Down => cursor('B'),
        TermKey::Right => cursor('C'),
        TermKey::Left => cursor('D'),
        TermKey::Home => "\x1b[H".into(),
        TermKey::End => "\x1b[F".into(),
        TermKey::PageUp => "\x1b[5~".into(),
        TermKey::PageDown => "\x1b[6~".into(),
        TermKey::Insert => "\x1b[2~".into(),
        TermKey::Delete => "\x1b[3~".into(),
        TermKey::Function(n) => match n {
            1..=4 => format!("\x1bO{}", char::from(b'P' + n - 1)),
            5 => "\x1b[15~".into(),
            6..=10 => format!("\x1b[{}~", 11 + n),
            11 | 12 => format!("\x1b[{}~", 12 + n),
            _ => return None,
        },
    };
    Some(if mods.alt {
        format!("\x1b{base}")
    } else {
        base
    })
}

fn control_char(c: char) -> Option<char> {
    let upper = c.to_ascii_uppercase();
    match upper {
        'A'..='Z' => Some(char::from(upper as u8 - b'A' + 1)),
        '[' | '3' => Some('\x1b'),
        '\\' | '4' => Some('\x1c'),
        ']' | '5' => Some('\x1d'),
        '^' | '6' => Some('\x1e'),
        '_' | '7' | '/' => Some('\x1f'),
        '?' | '8' => Some('\x7f'),
        _ => None,
    }
}

pub fn paste_text(text: &str, bracketed: bool) -> Result<String, String> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(format!(
            "The clipboard holds {} KiB; the terminal accepts pastes up to {} KiB. Nothing was pasted.",
            text.len() / 1024,
            MAX_PASTE_BYTES / 1024
        ));
    }
    let clean: String = text.chars().filter(|&c| c != '\0').collect();
    Ok(if bracketed {
        format!("\x1b[200~{clean}\x1b[201~")
    } else {
        clean
    })
}

pub fn input_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for (i, c) in text.chars().enumerate() {
        if i > 0 && i % INPUT_CHUNK_CHARS == 0 {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

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
mod tests {
    use super::*;

    fn screen(text: &str) -> Screen {
        let mut s = Screen::new(20, 5);
        s.feed(text);
        s
    }

    #[test]
    fn prints_wraps_and_scrolls_into_bounded_scrollback() {
        let s = screen("hello\r\nworld");
        assert_eq!(s.screen_lines()[..2], ["hello", "world"]);
        assert_eq!(s.cursor(), (1, 5));
        let s = screen(&"x".repeat(25));
        assert_eq!(s.screen_lines()[0], "x".repeat(20));
        assert_eq!(s.screen_lines()[1], "xxxxx");
        let mut s = Screen::new(20, 5);
        for i in 0..(SCROLLBACK_LINES + 50) {
            s.feed(&format!("line {i}\r\n"));
        }
        assert_eq!(s.scrollback_len(), SCROLLBACK_LINES);
        let (text, _) = s.render();
        assert!(text.contains(&format!("line {}", SCROLLBACK_LINES + 49)));
        assert!(!text.contains("line 0\n"));
    }

    #[test]
    fn readline_editing_sequences_render_the_final_line() {
        let s = screen("$ ls\x08 \x08s -l\x1b[3D\x1b[1@x");
        assert_eq!(s.screen_lines()[0], "$ lsx -l");
        let s = screen("abcdef\r\x1b[K");
        assert_eq!(s.screen_lines()[0], "");
        let s = screen("abcdef\x1b[3G\x1b[2P");
        assert_eq!(s.screen_lines()[0], "abef");
        let s = screen("one\r\ntwo\x1b[2J\x1b[H!");
        assert_eq!(s.screen_lines()[..2], ["!", ""]);
    }

    #[test]
    fn a_colour_reset_at_the_right_margin_preserves_pending_wrap() {
        let mut s = Screen::new(20, 5);
        s.feed(&"x".repeat(20));
        s.feed("\x1b[0mY");
        assert_eq!(s.screen_lines()[..2], ["x".repeat(20), "Y".into()]);
        assert_eq!(s.cursor(), (1, 1));
    }

    #[test]
    fn positioning_regions_and_alternate_screen() {
        let s = screen("\x1b[3;4HX\x1b[1;1HY");
        assert_eq!(s.screen_lines()[2], "   X");
        assert_eq!(s.screen_lines()[0], "Y");
        let s = screen("prompt\x1b[?1049h\x1b[Hfull screen\x1b[?1049l");
        assert_eq!(s.screen_lines()[0], "prompt");
        let s = screen("a\r\nb\r\nc\r\nd\r\ne\x1b[2;4r\x1b[4;1H\nX");
        assert_eq!(s.screen_lines(), ["a", "c", "d", "X", "e"]);
        let s = screen("\x1b[1;31mred\x1b[0m\x1b]0;title\x07!");
        assert_eq!(s.screen_lines()[0], "red!");
    }

    #[test]
    fn a_cursor_saved_before_a_shrinking_resize_is_restored_inside_the_grid() {
        let mut s = Screen::new(80, 40);
        s.feed("\x1b[40;80H\x1b7\x1b[s");
        s.resize(20, 5);
        s.feed("\x1b8X");
        assert_eq!(s.cursor(), (4, 19));
        s.feed("\x1b[uY");
        assert_eq!(s.screen_lines()[4], format!("{}Y", " ".repeat(19)));
    }

    const REDRAW: &str = "\r\x1b[K\0\0\0\0\0\0\0\0\0\0\0\0\r[lade] worktree$ ";

    fn prompts(s: &Screen) -> usize {
        s.render().0.matches("[lade] worktree$").count()
    }

    #[test]
    fn a_shrinking_resize_keeps_the_cursor_on_its_line() {
        let mut s = Screen::new(80, 24);
        s.feed("[lade] worktree$ ls\r\nREADME.md\r\n[lade] worktree$ ");
        assert_eq!(s.cursor(), (2, 17));
        s.resize(40, 10);
        assert_eq!(s.cursor(), (2, 17));
        assert_eq!(s.scrollback_len(), 0);
        assert_eq!(s.screen_lines()[2], "[lade] worktree$");
        s.feed(REDRAW);
        assert_eq!(prompts(&s), 2, "{:?}", s.render().0);
        let (text, offset) = s.render();
        assert!(
            text[..offset].ends_with("README.md\n[lade] worktree$ "),
            "{text:?}"
        );

        let mut s = Screen::new(80, 24);
        for i in 0..23 {
            s.feed(&format!("line {i}\r\n"));
        }
        s.feed("[lade] worktree$ ");
        assert_eq!(s.cursor(), (23, 17));
        s.resize(80, 10);
        assert_eq!(s.cursor(), (9, 17));
        assert_eq!(s.scrollback_len(), 14);
        assert_eq!(s.screen_lines()[9], "[lade] worktree$");

        let mut s = Screen::new(80, 24);
        s.feed("\x1b[24;1Hstatus\x1b[1;1H[lade] worktree$ ");
        s.resize(80, 10);
        assert_eq!(s.cursor(), (0, 17));
        assert_eq!(s.screen_lines()[0], "[lade] worktree$");
    }

    #[test]
    fn the_caret_sits_after_the_prompts_trailing_space() {
        let s = screen("abc\r\nbash-5.2$ ");
        let (text, offset) = s.render();
        assert_eq!(
            text.chars().take(offset).collect::<String>(),
            "abc\nbash-5.2$ "
        );
        let s = screen("x   \r\n\x1b[5C");
        let (text, offset) = s.render();
        assert!(text.starts_with("x\n     "), "{text:?}");
        assert_eq!(offset, "x\n     ".len());
    }

    #[test]
    fn modes_are_tracked() {
        let mut s = screen("\x1b[?1h\x1b[?2004h\x1b[?25l");
        assert!(s.application_cursor && s.bracketed_paste && !s.cursor_visible);
        s.feed("\x1b[?1l\x1b[?2004l");
        assert!(!s.application_cursor && !s.bracketed_paste);
    }

    #[test]
    fn hostile_sequences_stay_bounded() {
        let mut s = Screen::new(20, 5);
        let long = format!("\x1b[{}m", "1;".repeat(100_000));
        s.feed(&long);
        s.feed("\x1b[99999;99999HZ\x1b[99999A\x1b[99999@\x1b[99999L");
        assert_eq!(s.size(), (20, 5));
        let (row, col) = s.cursor();
        assert!(row < 5 && col < 20);
        s.resize(1, 1);
        assert_eq!(s.size(), (MIN_COLS, MIN_ROWS));
        s.resize(10_000, 10_000);
        assert_eq!(s.size(), (MAX_COLS, MAX_ROWS));
    }

    #[test]
    fn gaps_are_marked_visibly() {
        let mut session = TerminalSession::opening(ObjectiveId::new("o"), 40, 5);
        session.status = TerminalStatus::Open;
        session.output(1, "a", 0);
        session.output(2, "b", 5000);
        session.output(4, "c", 0);
        session.output(3, "late", 0);
        let (text, _) = session.screen.render();
        assert!(
            text.contains("[… 5000 bytes of output skipped …]"),
            "{text}"
        );
        assert!(text.contains("[… output skipped …]"), "{text}");
        assert!(!text.contains("late"));
        assert_eq!(session.screen.skipped_bytes, 5000);
    }

    #[test]
    fn only_a_defined_objective_points_to_activity_for_its_terminal() {
        use crate::presentation::model::{ConnectionState, ExecutionState};
        use crate::presentation::store::PresentationState;
        let mut state = PresentationState::new(ConnectionState::Connected);
        for update in crate::presentation::test_fixture::fixture_updates() {
            state.apply(update);
        }
        let mut summary = state
            .details
            .values()
            .find_map(|d| d.loaded().cloned())
            .unwrap()
            .summary;
        summary.lifecycle = Lifecycle::Defined;
        summary.execution.state = ExecutionState::None;
        assert!(awaits_first_start(&summary));
        for lifecycle in [Lifecycle::Active, Lifecycle::Frozen, Lifecycle::Discarded] {
            summary.lifecycle = lifecycle;
            assert!(!awaits_first_start(&summary), "{lifecycle:?}");
        }
    }

    #[test]
    fn keys_encode_as_vt100() {
        let none = Mods::default();
        let ctrl = Mods {
            control: true,
            ..Mods::default()
        };
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(
            encode_key(TermKey::Char('a'), none, false).as_deref(),
            Some("a")
        );
        assert_eq!(
            encode_key(TermKey::Char('c'), ctrl, false).as_deref(),
            Some("\x03")
        );
        assert_eq!(
            encode_key(TermKey::Char('D'), ctrl, false).as_deref(),
            Some("\x04")
        );
        assert_eq!(
            encode_key(TermKey::Char('['), ctrl, false).as_deref(),
            Some("\x1b")
        );
        assert_eq!(encode_key(TermKey::Char(' '), ctrl, false), None);
        assert_eq!(encode_key(TermKey::Char('1'), ctrl, false), None);
        assert_eq!(
            encode_key(TermKey::Char('b'), alt, false).as_deref(),
            Some("\x1bb")
        );
        assert_eq!(
            encode_key(TermKey::Enter, none, false).as_deref(),
            Some("\r")
        );
        assert_eq!(
            encode_key(TermKey::Backspace, none, false).as_deref(),
            Some("\x7f")
        );
        assert_eq!(
            encode_key(TermKey::Up, none, false).as_deref(),
            Some("\x1b[A")
        );
        assert_eq!(
            encode_key(TermKey::Up, none, true).as_deref(),
            Some("\x1bOA")
        );
        assert_eq!(
            encode_key(TermKey::Function(1), none, false).as_deref(),
            Some("\x1bOP")
        );
        assert_eq!(
            encode_key(TermKey::Function(5), none, false).as_deref(),
            Some("\x1b[15~")
        );
        assert_eq!(
            encode_key(TermKey::Function(10), none, false).as_deref(),
            Some("\x1b[21~")
        );
        assert_eq!(
            encode_key(TermKey::Function(12), none, false).as_deref(),
            Some("\x1b[24~")
        );
        assert_eq!(encode_key(TermKey::Function(13), none, false), None);
    }

    #[test]
    fn paste_and_chunks_are_bounded() {
        assert_eq!(paste_text("a\0b", false).unwrap(), "ab");
        assert_eq!(paste_text("x", true).unwrap(), "\x1b[200~x\x1b[201~");
        assert!(paste_text(&"x".repeat(MAX_PASTE_BYTES + 1), false).is_err());
        let chunks = input_chunks(&"é".repeat(INPUT_CHUNK_CHARS * 2 + 3));
        assert_eq!(chunks.len(), 3);
        assert!(
            chunks
                .iter()
                .all(|c| c.chars().count() <= INPUT_CHUNK_CHARS)
        );
        assert!(input_chunks("").is_empty());
    }
}
