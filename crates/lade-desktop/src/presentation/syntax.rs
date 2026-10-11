//! Whether the viewer highlights syntax. GtkSourceView detects the language from the file name
//! and highlights; this decides when it may, so large files stay responsive.
//!
//! Highlighting is visual only: the buffer text, and so the byte-exact selection mapping in
//! [`super::text_map`], is identical with and without it.

/// Shown text above this size is not highlighted (the reference file of the performance budget is
/// 1,280,000 bytes; see `docs/planning/performance-budgets.md`).
pub const HIGHLIGHT_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Shown text with more lines than this is not highlighted.
pub const HIGHLIGHT_MAX_LINES: usize = 40_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight {
    /// Highlight with the detected language.
    On,
    /// No language is known for this file name: plain text.
    NoLanguage,
    /// Too large to highlight responsively: plain text, stated in the viewer footer.
    TooLarge,
}

impl Highlight {
    pub fn decide(language_known: bool, bytes: usize, lines: usize) -> Self {
        if bytes > HIGHLIGHT_MAX_BYTES || lines > HIGHLIGHT_MAX_LINES {
            Self::TooLarge
        } else if language_known {
            Self::On
        } else {
            Self::NoLanguage
        }
    }

    /// The viewer footer's statement, when highlighting was turned off for size.
    pub fn note(self) -> Option<String> {
        (self == Self::TooLarge).then(|| {
            format!(
                "Syntax highlighting is off for text over {} MiB or {} lines",
                HIGHLIGHT_MAX_BYTES / (1024 * 1024),
                HIGHLIGHT_MAX_LINES
            )
        })
    }
}

/// The file name GtkSourceView matches language globs against (the last path component).
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_text_is_never_highlighted() {
        assert_eq!(Highlight::decide(true, 1_280_000, 20_000), Highlight::On);
        assert_eq!(Highlight::decide(false, 10, 1), Highlight::NoLanguage);
        assert_eq!(
            Highlight::decide(true, HIGHLIGHT_MAX_BYTES + 1, 1),
            Highlight::TooLarge
        );
        assert_eq!(
            Highlight::decide(false, 10, HIGHLIGHT_MAX_LINES + 1),
            Highlight::TooLarge
        );
        assert!(Highlight::TooLarge.note().unwrap().contains("2 MiB"));
        assert_eq!(Highlight::On.note(), None);
    }

    #[test]
    fn language_is_guessed_from_the_last_component() {
        assert_eq!(file_name("src/net/retry.rs"), "retry.rs");
        assert_eq!(file_name("Makefile"), "Makefile");
        assert_eq!(file_name("a/b/"), "");
    }
}
