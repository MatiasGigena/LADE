//! Keyboard and clipboard input encoded for the terminal.

use super::{INPUT_CHUNK_CHARS, MAX_PASTE_BYTES};

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
