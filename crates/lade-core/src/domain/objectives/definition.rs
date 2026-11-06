//! The values that define an Objective: its title, its intent and its branch names.

use crate::domain::ids::ObjectiveId;
use crate::domain::inspection::MAX_TEXT_BYTES;
use crate::error::DomainError;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_BRANCH_BYTES: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title(String);

impl Title {
    /// A trimmed one-line title.
    ///
    /// # Errors
    ///
    /// `DomainError::InvalidRequest` when the trimmed title is empty, longer than `MAX_TITLE_CHARS`
    /// characters or contains control characters.
    pub fn new(text: &str) -> Result<Self, DomainError> {
        let trimmed = text.trim();
        let count = trimmed.chars().count();
        if count == 0 || count > MAX_TITLE_CHARS || trimmed.chars().any(char::is_control) {
            return Err(DomainError::InvalidRequest(
                "The title must be 1 to 200 characters on one line.",
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BranchName(String);

impl BranchName {
    /// A branch name that git accepts.
    ///
    /// # Errors
    ///
    /// `DomainError::InvalidRequest` when git would refuse the name as a branch: empty, too long,
    /// `@` or `HEAD`, a leading `-`, a trailing `.`, `..` or `@{`, forbidden characters, or an
    /// empty, dot-led or `.lock` component.
    pub fn new(name: &str) -> Result<Self, DomainError> {
        let forbidden_char = |c: char| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        };
        let valid = !name.is_empty()
            && name.len() <= MAX_BRANCH_BYTES
            && name != "@"
            && name != "HEAD"
            && !name.starts_with('-')
            && !name.ends_with('.')
            && !name.contains("..")
            && !name.contains("@{")
            && !name.chars().any(forbidden_char)
            // Git refuses a `.lock` suffix on any component, comparing it case-sensitively.
            && name.split('/').all(|part| {
                !part.is_empty() && !part.starts_with('.') && !part.as_bytes().ends_with(b".lock")
            });
        if !valid {
            return Err(DomainError::InvalidRequest(
                "That isn't a valid branch name.",
            ));
        }
        Ok(Self(name.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn for_objective(id: ObjectiveId) -> Self {
        Self(format!("lade/{}", id.uuid().short()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    outcome: String,
    constraints: String,
    acceptance: String,
}

impl Intent {
    /// # Errors
    ///
    /// - `DomainError::InvalidRequest` when the outcome is blank.
    /// - `DomainError::LimitReached` when any field is over `MAX_TEXT_BYTES`.
    pub fn new(outcome: &str, constraints: &str, acceptance: &str) -> Result<Self, DomainError> {
        if outcome.trim().is_empty() {
            return Err(DomainError::InvalidRequest(
                "Describe the outcome you want.",
            ));
        }
        if [outcome, constraints, acceptance]
            .iter()
            .any(|field| field.len() > MAX_TEXT_BYTES)
        {
            return Err(DomainError::LimitReached("An intent field is too long."));
        }
        Ok(Self {
            outcome: outcome.to_string(),
            constraints: constraints.to_string(),
            acceptance: acceptance.to_string(),
        })
    }

    #[must_use]
    pub fn outcome(&self) -> &str {
        &self.outcome
    }

    #[must_use]
    pub fn constraints(&self) -> &str {
        &self.constraints
    }

    #[must_use]
    pub fn acceptance(&self) -> &str {
        &self.acceptance
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{objective_id, uuid};

    #[test]
    fn titles_intents_and_branches_are_validated() {
        assert_eq!(
            Title::new("  Fix login  ").map(|t| t.as_str().to_string()),
            Ok("Fix login".to_string())
        );
        assert!(Title::new("   ").is_err());
        assert!(Title::new(&"é".repeat(200)).is_ok());
        assert!(Title::new(&"é".repeat(201)).is_err());
        assert!(Title::new("a\nb").is_err());
        assert!(Intent::new(" ", "", "").is_err());
        assert!(Intent::new("x", &"y".repeat(MAX_TEXT_BYTES + 1), "").is_err());
        for good in ["main", "feature/x-1", "release/v1.2"] {
            assert!(BranchName::new(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-x",
            "a..b",
            "a b",
            "a/",
            "/a",
            "a.lock",
            "x/.hidden",
            "a@{1}",
            "@",
            "a:b",
            "a.",
            "a//b",
        ] {
            assert!(BranchName::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn branch_names_reject_the_reserved_head_name() {
        assert!(matches!(
            BranchName::new("HEAD"),
            Err(DomainError::InvalidRequest(_))
        ));
        assert!(BranchName::new("feature/HEAD").is_ok());
        assert!(BranchName::new("head").is_ok());
    }

    #[test]
    fn branch_names_are_bounded_in_bytes_and_lock_suffixes_are_case_sensitive() {
        assert!(BranchName::new(&"a".repeat(MAX_BRANCH_BYTES)).is_ok());
        assert!(BranchName::new(&"a".repeat(MAX_BRANCH_BYTES + 1)).is_err());
        for bad in [
            "a/b.lock", "a.lock/b", "a~1", "a^", "a?", "a*", "a[b", "a\\b", "a\u{7f}",
        ] {
            assert!(BranchName::new(bad).is_err(), "{bad}");
        }
        for good in ["a.LOCK", "a.lockx", "a@b", "x/HEAD", "a.b"] {
            assert_eq!(
                BranchName::new(good).map(|name| name.as_str().to_string()),
                Ok(good.to_string())
            );
        }
    }

    #[test]
    fn objective_branches_are_valid_names_under_lade() {
        for n in [1, 2, u128::MAX] {
            let branch = BranchName::for_objective(objective_id(n));
            assert_eq!(branch.as_str(), format!("lade/{}", uuid(n).short()));
            assert_eq!(BranchName::new(branch.as_str()), Ok(branch));
        }
        assert_ne!(
            BranchName::for_objective(objective_id(1 << 100)),
            BranchName::for_objective(objective_id(2 << 100)),
            "the branch names the leading digits of the id"
        );
    }

    #[test]
    fn titles_count_characters_after_trimming() {
        let padded = format!("  {}  ", "x".repeat(MAX_TITLE_CHARS));
        assert_eq!(
            Title::new(&padded).map(|title| title.as_str().len()),
            Ok(MAX_TITLE_CHARS)
        );
        assert!(Title::new("tab\tinside").is_err());
        assert!(Title::new("\n").is_err());
    }

    #[test]
    fn intents_keep_their_fields_verbatim_and_bound_every_field() {
        let intent = Intent::new(" outcome ", "  ", "").expect("intent");
        assert_eq!(
            (intent.outcome(), intent.constraints(), intent.acceptance()),
            (" outcome ", "  ", "")
        );
        let at_limit = "y".repeat(MAX_TEXT_BYTES);
        assert!(Intent::new(&at_limit, &at_limit, &at_limit).is_ok());
        let over = "y".repeat(MAX_TEXT_BYTES + 1);
        for (outcome, constraints, acceptance) in
            [(over.as_str(), "", ""), ("x", "", over.as_str())]
        {
            assert!(matches!(
                Intent::new(outcome, constraints, acceptance),
                Err(DomainError::LimitReached(_))
            ));
        }
        assert!(matches!(
            Intent::new("\n\t", "", ""),
            Err(DomainError::InvalidRequest(_))
        ));
    }
}
