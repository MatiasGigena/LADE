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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn control_maps_the_punctuation_and_digit_aliases_of_c0_codes() {
        let ctrl = Mods {
            control: true,
            ..Mods::default()
        };
        for (keys, code) in [
            (&['3', '['][..], "\x1b"),
            (&['4', '\\'], "\x1c"),
            (&['5', ']'], "\x1d"),
            (&['6', '^'], "\x1e"),
            (&['7', '_', '/'], "\x1f"),
            (&['8', '?'], "\x7f"),
        ] {
            for &key in keys {
                assert_eq!(
                    encode_key(TermKey::Char(key), ctrl, false).as_deref(),
                    Some(code),
                    "ctrl+{key}"
                );
            }
        }
    }

    #[test]
    fn alt_prefixes_escape_to_any_encoded_key_and_shift_changes_nothing() {
        let both = Mods {
            control: true,
            alt: true,
            shift: false,
        };
        assert_eq!(
            encode_key(TermKey::Char('a'), both, false).as_deref(),
            Some("\x1b\x01")
        );
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(
            encode_key(TermKey::Left, alt, true).as_deref(),
            Some("\x1b\x1bOD")
        );
        assert_eq!(encode_key(TermKey::Char('1'), both, false), None);
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        assert_eq!(
            encode_key(TermKey::Delete, shift, false),
            encode_key(TermKey::Delete, Mods::default(), false)
        );
        assert_eq!(
            encode_key(TermKey::BackTab, shift, false).as_deref(),
            Some("\x1b[Z")
        );
    }

    #[test]
    fn only_function_keys_one_to_twelve_are_encoded() {
        let none = Mods::default();
        let encoded: Vec<String> = (1..=12)
            .map(|n| encode_key(TermKey::Function(n), none, false).expect("encoded"))
            .collect();
        assert_eq!(
            encoded,
            [
                "\x1bOP", "\x1bOQ", "\x1bOR", "\x1bOS", "\x1b[15~", "\x1b[17~", "\x1b[18~",
                "\x1b[19~", "\x1b[20~", "\x1b[21~", "\x1b[23~", "\x1b[24~",
            ]
        );
        assert_eq!(encode_key(TermKey::Function(0), none, false), None);
    }

    #[test]
    fn a_paste_of_exactly_the_limit_is_accepted() {
        let limit = "x".repeat(MAX_PASTE_BYTES);
        assert_eq!(
            paste_text(&limit, false).map(|p| p.len()),
            Ok(MAX_PASTE_BYTES)
        );
        let error = paste_text(&format!("{limit}é"), true).unwrap_err();
        assert_eq!(
            error,
            "The clipboard holds 256 KiB; the terminal accepts pastes up to 256 KiB. Nothing \
             was pasted."
        );
    }

    #[test]
    fn input_splits_on_character_not_byte_boundaries() {
        let exact = "é".repeat(INPUT_CHUNK_CHARS * 2);
        let chunks = input_chunks(&exact);
        assert_eq!(chunks.len(), 2, "an exact multiple leaves no empty chunk");
        assert_eq!(chunks.concat(), exact);
        assert_eq!(input_chunks("ls\r"), ["ls\r"]);
    }
}
