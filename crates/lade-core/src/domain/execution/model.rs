//! The model an Agent Session runs on.

use crate::error::DomainError;

pub const MAX_LABEL_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    provider: String,
    id: String,
}

impl ModelRef {
    pub fn new(provider: &str, id: &str) -> Result<Self, DomainError> {
        let valid = |text: &str| {
            !text.is_empty()
                && text.len() <= MAX_LABEL_BYTES
                && !text.chars().any(|c| c.is_control() || c.is_whitespace())
        };
        if !valid(provider) || !valid(id) {
            return Err(DomainError::InvalidRequest(
                "Choose a model: the provider and model names can't be empty or contain spaces.",
            ));
        }
        Ok(Self {
            provider: provider.to_string(),
            id: id.to_string(),
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}
