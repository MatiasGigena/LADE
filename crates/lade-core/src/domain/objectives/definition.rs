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
