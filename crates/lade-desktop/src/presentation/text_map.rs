//! Mapping between what the read-only viewer displays and the exact source bytes it represents.
//!
//! GTK text positions are (line, character offset) where a character is a Unicode scalar value. The
//! viewer shows one buffer line per source line; characters GTK would treat as paragraph delimiters
//! (`\r`, U+2029) are replaced with visible symbols so buffer lines never drift from source lines.
//! Every display character keeps the UTF-8 byte offset of the source character it stands for.

use super::model::{DiffLineKind, FileContent, FileDiff, Side};

/// Visible stand-in for a carriage return that is not part of a CRLF terminator.
pub const CR_SYMBOL: char = '\u{240D}';
/// Visible stand-in for U+2029 PARAGRAPH SEPARATOR.
pub const PARAGRAPH_SYMBOL: char = '\u{00B6}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewLineKind {
    Context,
    Add,
    Del,
    /// A line of a full file.
    Text,
    /// DiffHunk header / omitted-lines separator; represents no source bytes.
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewLine {
    pub kind: ViewLineKind,
    pub display: String,
    pub old_no: Option<u64>,
    pub new_no: Option<u64>,
    pub old_start: Option<u64>,
    pub new_start: Option<u64>,
    /// Source text without the `\n` terminator (CR retained).
    pub source: String,
    /// DiffHunk index; `None` for full-file lines (one contiguous region).
    pub hunk: Option<usize>,
    /// `char_bytes[i]` is the source byte offset (within `source`) of display character `i`;
    /// the final element is the byte offset of the display line's end.
    char_bytes: Vec<usize>,
}

impl ViewLine {
    fn source_line(
        kind: ViewLineKind,
        source: &str,
        numbers: (Option<u64>, Option<u64>),
        starts: (Option<u64>, Option<u64>),
        hunk: Option<usize>,
    ) -> Self {
        let (display, char_bytes) = display_text(source);
        Self {
            kind,
            display,
            old_no: numbers.0,
            new_no: numbers.1,
            old_start: starts.0,
            new_start: starts.1,
            source: source.to_owned(),
            hunk,
            char_bytes,
        }
    }

    fn separator(label: String) -> Self {
        let char_bytes = (0..=label.chars().count()).map(|_| 0).collect();
        Self {
            kind: ViewLineKind::Separator,
            display: label,
            old_no: None,
            new_no: None,
            old_start: None,
            new_start: None,
            source: String::new(),
            hunk: None,
            char_bytes,
        }
    }

    pub fn start(&self, side: Side) -> Option<u64> {
        match side {
            Side::Old => self.old_start,
            Side::New => self.new_start,
        }
    }

    pub fn number(&self, side: Side) -> Option<u64> {
        match side {
            Side::Old => self.old_no,
            Side::New => self.new_no,
        }
    }

    /// Source byte offset (within this line) for a display character offset, clamped to the line.
    pub fn byte_at(&self, char_offset: usize) -> usize {
        let last = self.char_bytes.len() - 1;
        self.char_bytes[char_offset.min(last)]
    }

    pub fn display_chars(&self) -> usize {
        self.char_bytes.len() - 1
    }
}

/// Builds the display text and its char → source-byte map.
pub fn display_text(source: &str) -> (String, Vec<usize>) {
    let mut display = String::with_capacity(source.len());
    let mut map = Vec::with_capacity(source.len() + 1);
    let crlf_cr = source.ends_with('\r').then(|| source.len() - 1);
    for (byte, ch) in source.char_indices() {
        if Some(byte) == crlf_cr {
            // The CR of a CRLF terminator: hidden, the line end maps to just before it.
            break;
        }
        map.push(byte);
        display.push(match ch {
            '\r' => CR_SYMBOL,
            '\u{2029}' => PARAGRAPH_SYMBOL,
            other => other,
        });
    }
    map.push(crlf_cr.unwrap_or(source.len()));
    (display, map)
}

pub fn diff_lines(diff: &FileDiff) -> Vec<ViewLine> {
    let mut out = Vec::new();
    for (index, hunk) in diff.hunks.iter().enumerate() {
        out.push(ViewLine::separator(format!(
            "@@ −{},{} +{},{} @@",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        )));
        for line in &hunk.lines {
            let kind = match line.kind {
                DiffLineKind::Context => ViewLineKind::Context,
                DiffLineKind::Add => ViewLineKind::Add,
                DiffLineKind::Del => ViewLineKind::Del,
            };
            out.push(ViewLine::source_line(
                kind,
                &line.text,
                (line.old_no, line.new_no),
                (line.old_byte_start, line.new_byte_start),
                Some(index),
            ));
        }
    }
    out
}

pub fn file_lines(content: &FileContent) -> Vec<ViewLine> {
    let mut out = Vec::new();
    let mut start = 0u64;
    let text = content.text.as_str();
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        // The empty remainder after a final newline is not a line.
        lines.pop();
    }
    for (index, source) in lines.into_iter().enumerate() {
        let number = Some(index as u64 + 1);
        let (numbers, starts) = match content.side {
            Side::Old => ((number, None), (Some(start), None)),
            Side::New => ((None, number), (None, Some(start))),
        };
        out.push(ViewLine::source_line(
            ViewLineKind::Text,
            source,
            numbers,
            starts,
            None,
        ));
        start += source.len() as u64 + 1;
    }
    out
}

/// Display position of source byte `byte` on `side` — the inverse of the selection map — when a
/// displayed line holds it.
pub fn position_at(lines: &[ViewLine], side: Side, byte: u64) -> Option<TextPos> {
    lines.iter().enumerate().find_map(|(index, line)| {
        let within = usize::try_from(byte.checked_sub(line.start(side)?)?).ok()?;
        if within > line.source.len() {
            return None;
        }
        let offset = line.char_bytes.iter().position(|b| *b >= within)?;
        Some(TextPos::new(index, offset))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub line: usize,
    pub char_offset: usize,
}

impl TextPos {
    pub fn new(line: usize, char_offset: usize) -> Self {
        Self { line, char_offset }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedSelection {
    pub side: Side,
    pub start: u64,
    pub end: u64,
    pub selected_text: String,
    pub line_hint: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    Empty,
    NoSourceLines,
    SpansHunks,
}

impl SelectionError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Select text in the viewer to add feedback",
            Self::NoSourceLines => "The selection contains no file content",
            Self::SpansHunks => {
                "The selection spans separate hunks — select within one hunk or open the full file"
            }
        }
    }
}

/// Maps a viewer selection to exact source bytes `[start, end)` on one side.
///
/// The new side is preferred whenever the selection touches new-side content; a selection made only
/// of deleted lines refers to the old side.
pub fn map_selection(
    lines: &[ViewLine],
    a: TextPos,
    b: TextPos,
) -> Result<MappedSelection, SelectionError> {
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    if from == to || lines.is_empty() || from.line >= lines.len() {
        return Err(SelectionError::Empty);
    }
    let to = if to.line >= lines.len() {
        let last = lines.len() - 1;
        TextPos::new(last, lines[last].display_chars())
    } else {
        to
    };
    // A selection ending at column 0 does not include that line's characters.
    let covered: Vec<usize> = (from.line..=to.line)
        .filter(|&i| !(i == to.line && to.char_offset == 0 && i != from.line))
        .collect();
    let side = [Side::New, Side::Old]
        .into_iter()
        .find(|&side| covered.iter().any(|&i| lines[i].start(side).is_some()))
        .ok_or(SelectionError::NoSourceLines)?;
    let side_lines: Vec<usize> = covered
        .iter()
        .copied()
        .filter(|&i| lines[i].start(side).is_some())
        .collect();
    let hunk = lines[side_lines[0]].hunk;
    if side_lines.iter().any(|&i| lines[i].hunk != hunk) {
        return Err(SelectionError::SpansHunks);
    }
    let line_start = |i: usize| lines[i].start(side).unwrap_or(0);
    let first = side_lines[0];
    let last = *side_lines.last().unwrap_or(&first);
    let start = if first == from.line {
        line_start(first) + lines[first].byte_at(from.char_offset) as u64
    } else {
        line_start(first)
    };
    let end = if last == to.line {
        line_start(last) + lines[last].byte_at(to.char_offset) as u64
    } else {
        // Selection continues past `last` on lines without this side: include its terminator when
        // the next same-side line in this hunk proves where the terminator ends.
        let next = (last + 1..lines.len())
            .find(|&i| lines[i].start(side).is_some() && lines[i].hunk == hunk);
        match next {
            Some(n) => line_start(n),
            None => line_start(last) + lines[last].source.len() as u64,
        }
    };
    if end <= start {
        return Err(SelectionError::Empty);
    }
    // Reconstruct the exact selected source text from contiguous same-side lines.
    let base = line_start(first);
    let mut contiguous = String::new();
    let mut expected = base;
    let mut i = first;
    while i < lines.len() && expected < end {
        if let Some(line_begin) = lines[i].start(side) {
            if lines[i].hunk != hunk {
                break;
            }
            if line_begin != expected {
                return Err(SelectionError::SpansHunks);
            }
            contiguous.push_str(&lines[i].source);
            contiguous.push('\n');
            expected = line_begin + lines[i].source.len() as u64 + 1;
        }
        i += 1;
    }
    let lo = (start - base) as usize;
    let hi = ((end - base) as usize).min(contiguous.len());
    let selected_text = contiguous.get(lo..hi).unwrap_or_default().to_owned();
    Ok(MappedSelection {
        side,
        start,
        end,
        selected_text,
        line_hint: lines[first].number(side),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::model::{
        ArtifactDigest, Basis, DiffHunk, DiffLine, FileEncoding, ObjectiveId, RepoPath,
    };

    #[test]
    fn search_ranges_map_back_to_the_same_bytes() {
        let text = "ab\r\nαβ Needle x\n";
        let lines = file(text);
        let start = text.find("Needle").unwrap() as u64;
        let from = position_at(&lines, Side::New, start).unwrap();
        let to = position_at(&lines, Side::New, start + 6).unwrap();
        assert_eq!((from, to), (TextPos::new(1, 3), TextPos::new(1, 9)));
        let mapped = map_selection(&lines, from, to).unwrap();
        assert_eq!((mapped.start, mapped.end), (start, start + 6));
        assert_eq!(mapped.selected_text, "Needle");
        assert_eq!(position_at(&lines, Side::Old, start), None);
        assert_eq!(position_at(&lines, Side::New, 1_000), None);
    }

    fn file(text: &str) -> Vec<ViewLine> {
        file_lines(&FileContent {
            objective_id: ObjectiveId::new("o"),
            basis: Basis::Working,
            path: RepoPath::utf8("a.txt"),
            side: Side::New,
            artifact_digest: ArtifactDigest::new("d"),
            total_bytes: text.len() as u64,
            encoding: FileEncoding::Utf8,
            text: text.to_owned(),
            loaded_bytes: text.len() as u64,
            eof: true,
            version_label: "Working".into(),
            chunked: false,
        })
    }

    fn select(text: &str, a: TextPos, b: TextPos) -> MappedSelection {
        let lines = file(text);
        let mapped = map_selection(&lines, a, b).expect("selection maps");
        assert_eq!(
            &text.as_bytes()[mapped.start as usize..mapped.end as usize],
            mapped.selected_text.as_bytes(),
            "selected text must equal the exact source bytes"
        );
        mapped
    }

    #[test]
    fn ascii_selection_is_byte_exact() {
        let m = select("hello world\n", TextPos::new(0, 6), TextPos::new(0, 11));
        assert_eq!((m.start, m.end), (6, 11));
        assert_eq!(m.selected_text, "world");
        assert_eq!(m.line_hint, Some(1));
    }

    #[test]
    fn multibyte_characters_convert_char_offsets_to_bytes() {
        // "é" = 2 bytes, "漢" = 3 bytes, "🎉" = 4 bytes.
        let text = "aé漢🎉z\n";
        let m = select(text, TextPos::new(0, 1), TextPos::new(0, 4));
        assert_eq!((m.start, m.end), (1, 10));
        assert_eq!(m.selected_text, "é漢🎉");
        // Reversed endpoints map identically.
        let r = select(text, TextPos::new(0, 4), TextPos::new(0, 1));
        assert_eq!((r.start, r.end), (1, 10));
    }

    #[test]
    fn combining_marks_are_separate_scalar_values() {
        // "e\u{301}" is one grapheme but two scalar values (GTK offsets count scalar values).
        let text = "xe\u{301}y\n";
        let m = select(text, TextPos::new(0, 1), TextPos::new(0, 3));
        assert_eq!((m.start, m.end), (1, 4));
        assert_eq!(m.selected_text, "e\u{301}");
    }

    #[test]
    fn crlf_terminator_is_hidden_but_counted() {
        let text = "ab\r\ncd\r\n";
        let lines = file(text);
        assert_eq!(lines[0].display, "ab");
        assert_eq!(lines[1].new_start, Some(4));
        // Select to end of the first line: excludes the CR.
        let m = select(text, TextPos::new(0, 0), TextPos::new(0, 2));
        assert_eq!((m.start, m.end), (0, 2));
        // Select into the next line: includes CRLF.
        let m = select(text, TextPos::new(0, 1), TextPos::new(1, 1));
        assert_eq!((m.start, m.end), (1, 5));
        assert_eq!(m.selected_text, "b\r\nc");
    }

    #[test]
    fn lone_carriage_return_and_paragraph_separator_stay_on_one_line() {
        let text = "a\rb\u{2029}c\n";
        let lines = file(text);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].display,
            format!("a{CR_SYMBOL}b{PARAGRAPH_SYMBOL}c")
        );
        let m = select(text, TextPos::new(0, 1), TextPos::new(0, 4));
        assert_eq!((m.start, m.end), (1, 6));
        assert_eq!(m.selected_text, "\rb\u{2029}");
    }

    #[test]
    fn tabs_are_single_characters() {
        let text = "\t\tlet x = 1;\n";
        let m = select(text, TextPos::new(0, 2), TextPos::new(0, 5));
        assert_eq!((m.start, m.end), (2, 5));
        assert_eq!(m.selected_text, "let");
    }

    #[test]
    fn selection_ending_at_column_zero_includes_newline_only() {
        let text = "one\ntwo\n";
        let m = select(text, TextPos::new(0, 0), TextPos::new(1, 0));
        assert_eq!((m.start, m.end), (0, 4));
        assert_eq!(m.selected_text, "one\n");
    }

    #[test]
    fn empty_selection_is_rejected() {
        let lines = file("abc\n");
        assert_eq!(
            map_selection(&lines, TextPos::new(0, 1), TextPos::new(0, 1)),
            Err(SelectionError::Empty)
        );
    }

    fn line(
        kind: DiffLineKind,
        text: &str,
        old: Option<(u64, u64)>,
        new: Option<(u64, u64)>,
    ) -> DiffLine {
        DiffLine {
            kind,
            old_no: old.map(|o| o.0),
            new_no: new.map(|n| n.0),
            text: text.to_owned(),
            old_byte_start: old.map(|o| o.1),
            new_byte_start: new.map(|n| n.1),
        }
    }

    fn diff() -> FileDiff {
        // old: "a\nold\nc\n"  new: "a\nnew é\nc\n" ; second hunk far below.
        FileDiff {
            objective_id: ObjectiveId::new("o"),
            basis: Basis::Working,
            path: RepoPath::utf8("a.txt"),
            old_digest: None,
            new_digest: None,
            binary: false,
            truncated: false,
            hunks: vec![
                DiffHunk {
                    old_start: 1,
                    old_lines: 3,
                    new_start: 1,
                    new_lines: 3,
                    lines: vec![
                        line(DiffLineKind::Context, "a", Some((1, 0)), Some((1, 0))),
                        line(DiffLineKind::Del, "old", Some((2, 2)), None),
                        line(DiffLineKind::Add, "new é", None, Some((2, 2))),
                        line(DiffLineKind::Context, "c", Some((3, 6)), Some((3, 9))),
                    ],
                },
                DiffHunk {
                    old_start: 40,
                    old_lines: 1,
                    new_start: 40,
                    new_lines: 1,
                    lines: vec![line(
                        DiffLineKind::Context,
                        "z",
                        Some((40, 500)),
                        Some((40, 503)),
                    )],
                },
            ],
        }
    }

    #[test]
    fn diff_selection_prefers_new_side_and_skips_deleted_lines() {
        let lines = diff_lines(&diff());
        // lines: 0 sep, 1 ctx a, 2 del old, 3 add new é, 4 ctx c, 5 sep, 6 ctx z
        let m = map_selection(&lines, TextPos::new(1, 0), TextPos::new(3, 5)).unwrap();
        assert_eq!(m.side, Side::New);
        assert_eq!((m.start, m.end), (0, 8));
        assert_eq!(m.selected_text, "a\nnew é");
    }

    #[test]
    fn diff_selection_of_deleted_lines_uses_old_side() {
        let lines = diff_lines(&diff());
        let m = map_selection(&lines, TextPos::new(2, 1), TextPos::new(2, 3)).unwrap();
        assert_eq!(m.side, Side::Old);
        assert_eq!((m.start, m.end), (3, 5));
        assert_eq!(m.selected_text, "ld");
        assert_eq!(m.line_hint, Some(2));
    }

    #[test]
    fn diff_selection_starting_on_deleted_line_starts_at_next_new_line() {
        let lines = diff_lines(&diff());
        let m = map_selection(&lines, TextPos::new(2, 1), TextPos::new(4, 1)).unwrap();
        assert_eq!(m.side, Side::New);
        assert_eq!((m.start, m.end), (2, 10));
        assert_eq!(m.selected_text, "new é\nc");
    }

    #[test]
    fn diff_selection_across_hunks_is_rejected() {
        let lines = diff_lines(&diff());
        assert_eq!(
            map_selection(&lines, TextPos::new(4, 0), TextPos::new(6, 1)),
            Err(SelectionError::SpansHunks)
        );
    }

    #[test]
    fn separator_only_selection_has_no_source() {
        let lines = diff_lines(&diff());
        assert_eq!(
            map_selection(&lines, TextPos::new(0, 0), TextPos::new(0, 3)),
            Err(SelectionError::NoSourceLines)
        );
    }
}
