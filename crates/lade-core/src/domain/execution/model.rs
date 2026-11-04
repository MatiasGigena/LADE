//! The model an Agent Session runs on.

use crate::error::DomainError;

pub const MAX_LABEL_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    provider: String,
    id: String,
}

impl ModelRef {
    /// A model named by its provider and id.
    ///
    /// # Errors
    ///
    /// `DomainError::InvalidRequest` when either label is empty, longer than `MAX_LABEL_BYTES`, or
    /// contains whitespace or control characters.
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

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_up_to_the_byte_limit_are_kept_verbatim() {
        let at_limit = "é".repeat(MAX_LABEL_BYTES / 2);
        let model = ModelRef::new("lade-fixture", &at_limit).expect("at the limit");
        assert_eq!(model.provider(), "lade-fixture");
        assert_eq!(model.id(), at_limit);
        assert!(ModelRef::new("p", &"x".repeat(MAX_LABEL_BYTES)).is_ok());
    }

    #[test]
    fn empty_oversized_spaced_or_control_labels_are_refused_on_either_side() {
        let over = "x".repeat(MAX_LABEL_BYTES + 1);
        for bad in [
            "",
            "a b",
            "a\tb",
            "a\u{a0}b",
            "a\u{7}b",
            "a\nb",
            over.as_str(),
        ] {
            for (provider, id) in [(bad, "ok"), ("ok", bad)] {
                assert!(
                    matches!(
                        ModelRef::new(provider, id),
                        Err(DomainError::InvalidRequest(_))
                    ),
                    "{provider:?}/{id:?}"
                );
            }
        }
    }

    #[test]
    fn models_compare_by_provider_and_id() {
        let model = |provider, id| ModelRef::new(provider, id).expect("model");
        assert_eq!(model("a", "m"), model("a", "m"));
        assert_ne!(model("a", "m"), model("b", "m"));
        assert_ne!(model("a", "m"), model("a", "n"));
    }
}
