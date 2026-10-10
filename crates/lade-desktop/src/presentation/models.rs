//! Client-side table of well-known pi model ids for the execution model picker.
//!
//! The protocol's `Settings` carries no model catalog, so this is a presentation convenience only:
//! the service decides whether a model is usable (credentials, network policy) and rejects
//! `execution.start` with its reason otherwise. Ids follow the pinned pi-ai model registry.

use super::model::ModelRef;

/// The offline, scripted fixture provider implemented by the worker (`lade-fixture/scripted`).
pub const FIXTURE_PROVIDER: &str = "lade-fixture";
pub const FIXTURE_MODEL: &str = "scripted";

/// `(provider, id, label)`; the fixture first so an offline setup always has a working choice.
const KNOWN_MODELS: &[(&str, &str, &str)] = &[
    (
        FIXTURE_PROVIDER,
        FIXTURE_MODEL,
        "Fixture (offline, scripted)",
    ),
    ("anthropic", "claude-opus-5", "Claude Opus 5"),
    ("anthropic", "claude-sonnet-5", "Claude Sonnet 5"),
    ("anthropic", "claude-haiku-4-5", "Claude Haiku 4.5"),
    ("openai", "gpt-5.5", "GPT-5.5"),
    ("openai", "gpt-5.4-mini", "GPT-5.4 mini"),
    ("google", "gemini-3.1-pro-preview", "Gemini 3.1 Pro"),
    ("google", "gemini-3.5-flash", "Gemini 3.5 Flash"),
];

/// Picker entries: the known table, with a configured default that is not in it listed first.
pub fn available_models(default_model: Option<&ModelRef>) -> Vec<ModelRef> {
    let mut models: Vec<ModelRef> = KNOWN_MODELS
        .iter()
        .map(|(provider, id, _)| ModelRef::new(*provider, *id))
        .collect();
    if let Some(default) = default_model
        && !models.contains(default)
    {
        models.insert(0, default.clone());
    }
    models
}

/// Short picker label (the inspector is narrow); unknown models show their `provider/id`.
pub fn display_label(model: &ModelRef) -> String {
    KNOWN_MODELS
        .iter()
        .find(|(provider, id, _)| *provider == model.provider && *id == model.id)
        .map_or_else(|| model.label(), |(_, _, label)| (*label).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_listed_and_labelled() {
        let models = available_models(None);
        assert_eq!(models[0], ModelRef::new("lade-fixture", "scripted"));
        assert_eq!(display_label(&models[0]), "Fixture (offline, scripted)");
        assert!(models.iter().any(|m| m.provider == "anthropic"));
    }

    #[test]
    fn unknown_default_is_offered_first() {
        let custom = ModelRef::new("openrouter", "some/model");
        let models = available_models(Some(&custom));
        assert_eq!(models[0], custom);
        assert_eq!(display_label(&custom), "openrouter/some/model");
        let known = ModelRef::new("openai", "gpt-5.5");
        assert_eq!(available_models(Some(&known)).len(), KNOWN_MODELS.len());
    }
}
