//! Whether a file is shown syntax-highlighted: only when its language is known and it is within
//! the size limits.

pub const HIGHLIGHT_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const HIGHLIGHT_MAX_LINES: usize = 40_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight {
    On,
    NoLanguage,
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
