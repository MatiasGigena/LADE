//! A bounded VT100-style screen: the grid, scrollback and the escape sequences a shell and
//! common line editors emit.

use std::collections::VecDeque;

use super::{MAX_COLS, MAX_PARAMS, MAX_ROWS, MIN_COLS, MIN_ROWS, SCROLLBACK_LINES};

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

    #[cfg(test)]
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    #[cfg(test)]
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
                    .max(self.top.min(self.row));
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
            'L' => self.insert_lines(arg(0, 1)),
            'M' => self.delete_lines(arg(0, 1)),
            'P' => self.delete_chars(arg(0, 1)),
            '@' => self.insert_chars(arg(0, 1)),
            'X' => {
                let end = (self.col + arg(0, 1)).min(self.cols);
                self.grid[self.row][self.col..end].fill(' ');
            }
            'S' => self.scroll_up(arg(0, 1)),
            'T' => self.scroll_down(arg(0, 1)),
            'r' => self.set_scroll_region(arg(0, 1) - 1, arg(1, self.rows) - 1),
            's' => self.saved_cursor = (self.row, self.col),
            'u' => self.restore_cursor(),
            'h' | 'l' if private => self.set_private_modes(&values, command == 'h'),
            _ => {}
        }
    }

    /// Inserts blank lines at the cursor inside the scroll region, pushing lines off its bottom.
    fn insert_lines(&mut self, n: usize) {
        if (self.top..=self.bottom).contains(&self.row) {
            for _ in 0..n.min(self.bottom - self.row + 1) {
                self.grid.remove(self.bottom);
                self.grid.insert(self.row, blank(self.cols));
            }
        }
    }

    /// Deletes lines at the cursor inside the scroll region, pulling blank lines in at its bottom.
    fn delete_lines(&mut self, n: usize) {
        if (self.top..=self.bottom).contains(&self.row) {
            for _ in 0..n.min(self.bottom - self.row + 1) {
                self.grid.remove(self.row);
                self.grid.insert(self.bottom, blank(self.cols));
            }
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let line = &mut self.grid[self.row];
        for _ in 0..n.min(self.cols - self.col) {
            line.remove(self.col);
            line.push(' ');
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let line = &mut self.grid[self.row];
        for _ in 0..n.min(self.cols - self.col) {
            line.pop();
            line.insert(self.col, ' ');
        }
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        if top < bottom && bottom < self.rows {
            self.top = top;
            self.bottom = bottom;
            self.row = 0;
            self.col = 0;
        }
    }

    fn set_private_modes(&mut self, modes: &[usize], on: bool) {
        for mode in modes {
            match mode {
                1 => self.application_cursor = on,
                25 => self.cursor_visible = on,
                47 | 1047 | 1049 => self.set_alternate(on),
                2004 => self.bracketed_paste = on,
                _ => {}
            }
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
