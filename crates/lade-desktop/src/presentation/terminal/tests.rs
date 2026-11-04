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
    use crate::presentation::state::model::{ConnectionState, ExecutionState};
    use crate::presentation::state::store::PresentationState;
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
