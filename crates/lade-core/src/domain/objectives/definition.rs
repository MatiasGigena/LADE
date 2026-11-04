//! The values that define an Objective: its title, its intent and its branch names.

use crate::domain::ids::ObjectiveId;
use crate::domain::inspection::MAX_TEXT_BYTES;
use crate::error::DomainError;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_BRANCH_BYTES: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title(String);

impl Title {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BranchName(String);

impl BranchName {
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
            && !name.ends_with(".lock")
            && !name.contains("..")
            && !name.contains("@{")
            && !name.chars().any(forbidden_char)
            && name
                .split('/')
                .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"));
        if !valid {
            return Err(DomainError::InvalidRequest(
                "That isn't a valid branch name.",
            ));
        }
        Ok(Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

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

    pub fn outcome(&self) -> &str {
        &self.outcome
    }

    pub fn constraints(&self) -> &str {
        &self.constraints
    }

    pub fn acceptance(&self) -> &str {
        &self.acceptance
    }
}
