//! The model catalogue: listing pi's providers and models once per profile through a short-lived
//! worker, and replying with each provider's access status under LADE's egress and credential
//! policy.

use std::fs;
use std::sync::Arc;

use lade_protocol::client::{
    ModelCatalog, ModelInfo, ModelsListQuery, ProviderAuth, ProviderInfo, ProviderStatus, ReplyData,
};
use lade_protocol::types::{Counter, ThinkingLevel};
use lade_protocol::worker::Catalog;

use crate::adapters::profile;
use crate::adapters::providers::FIXTURE_PROVIDER;
use crate::supervision::{AccessStatus, AuthKind, LaunchError, ProviderFacts};
use crate::support::fs::ensure_private_dir;

use super::convert::preview;
use super::{Completed, Flow, ReqCtx, Service};
use crate::support::new_uuid;

const MAX_PROVIDERS: usize = 128;
const MAX_REPLY_MODELS: usize = 512;
const MAX_CATALOG_MODELS: usize = 4096;

pub(crate) const CATALOG_SANDBOX_FAILED: &str =
    "LADE can't run agents on this computer because the sandbox check failed.";
pub(crate) const CATALOG_FAILED: &str =
    "LADE couldn't load the list of models. Details are in the service diagnostics.";

#[derive(Clone, Debug, Default)]
pub(crate) struct CatalogData {
    pub sdk_version: String,
    pub providers: Vec<(ProviderFacts, Vec<ModelInfo>)>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogFailure {
    pub message: &'static str,
    pub detail: String,
}

impl CatalogFailure {
    fn other(detail: String) -> Self {
        Self {
            message: CATALOG_FAILED,
            detail,
        }
    }

    fn launch(error: &LaunchError) -> Self {
        Self {
            message: match error {
                LaunchError::SandboxUnavailable(_) => CATALOG_SANDBOX_FAILED,
                _ => CATALOG_FAILED,
            },
            detail: error.detail(),
        }
    }
}

/// The model catalogue, listed once by a short-lived worker and cached until the profile
/// changes. Requests that arrive while it is being listed wait for the same result.
#[derive(Default)]
pub(crate) struct CatalogCache {
    pub data: Option<Arc<CatalogData>>,
    pub listing: bool,
    pub waiting: Vec<(ReqCtx, ModelsListQuery)>,
}

fn wire_auth(kind: AuthKind) -> ProviderAuth {
    match kind {
        AuthKind::None => ProviderAuth::None,
        AuthKind::ApiKey => ProviderAuth::ApiKey,
        AuthKind::SubscriptionToken => ProviderAuth::SubscriptionToken,
        AuthKind::OauthLogin => ProviderAuth::OauthLogin,
        AuthKind::CloudCredentials => ProviderAuth::CloudCredentials,
        AuthKind::Custom => ProviderAuth::Custom,
    }
}

fn wire_status(status: AccessStatus) -> ProviderStatus {
    match status {
        AccessStatus::Available => ProviderStatus::Available,
        AccessStatus::MissingCredentials => ProviderStatus::MissingCredentials,
        AccessStatus::Restricted => ProviderStatus::Restricted,
    }
}

fn text(value: &str, max: usize) -> Option<String> {
    Some(preview(value, max)).filter(|s| !s.is_empty())
}

fn fallback_catalog(test_fixture_provider: bool, error: String) -> CatalogData {
    let fixture = (
        ProviderFacts {
            id: FIXTURE_PROVIDER.into(),
            name: "LADE fixture provider (offline, scripted; not a real model)".into(),
            api_key: true,
            oauth: None,
            subscription: false,
            base_url: None,
        },
        vec![ModelInfo {
            provider: FIXTURE_PROVIDER.into(),
            id: "scripted".into(),
            name: "LADE fixture: scripted (offline test model)".into(),
            reasoning: false,
            thinking_levels: vec![ThinkingLevel::Off],
        }],
    );
    CatalogData {
        sdk_version: "1.5.0".into(),
        providers: if test_fixture_provider {
            vec![fixture]
        } else {
            Vec::new()
        },
        error: Some(error),
    }
}

pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<CatalogData, String> {
    let catalog: Catalog = serde_json::from_slice(bytes)
        .map_err(|e| format!("catalogue output is not a catalogue: {e}"))?;
    let sdk_version = text(&catalog.sdk_version, 64).ok_or("catalogue has no SDK version")?;
    let mut providers = Vec::new();
    let mut total = 0usize;
    for entry in catalog.providers.into_iter().take(MAX_PROVIDERS) {
        let Some(id) = text(&entry.id, 128) else {
            continue;
        };
        let mut models = Vec::new();
        for model in entry.models {
            if total >= MAX_CATALOG_MODELS {
                break;
            }
            let Some(model_id) = text(&model.id, 256) else {
                continue;
            };
            models.push(ModelInfo {
                provider: id.clone(),
                name: text(&model.name, 256).unwrap_or_else(|| model_id.clone()),
                id: model_id,
                reasoning: model.reasoning,
                thinking_levels: model
                    .thinking
                    .iter()
                    .filter_map(|l| ThinkingLevel::parse(l))
                    .take(8)
                    .collect(),
            });
            total += 1;
        }
        let facts = ProviderFacts {
            name: text(&entry.name, 256).unwrap_or_else(|| id.clone()),
            api_key: entry.api_key,
            oauth: entry.oauth.as_deref().and_then(|o| text(o, 200)),
            subscription: entry.subscription,
            base_url: entry.base_url.as_deref().and_then(|u| text(u, 400)),
            id,
        };
        providers.push((facts, models));
    }
    Ok(CatalogData {
        sdk_version,
        providers,
        error: catalog.error.as_deref().and_then(|e| text(e, 512)),
    })
}

impl Service {
    pub(crate) fn models_list(&mut self, ctx: ReqCtx, query: ModelsListQuery) -> Flow {
        if let Some(data) = self.catalog.data.clone() {
            let reply = self.catalog_reply(&data, &query);
            self.finish(ctx, Ok(reply));
            return Flow::Done;
        }
        self.catalog.waiting.push((ctx, query));
        if !self.catalog.listing {
            self.catalog.listing = true;
            let launcher = self.launcher.clone();
            let scratch = self.paths.tmp.join(format!("catalog-{}", new_uuid()));
            let profile = self.profile_dir();
            self.spawn(move || {
                let models = profile.join("models.json");
                let models = profile::profile_providers(&profile)
                    .is_ok_and(|p| !p.is_empty())
                    .then_some(models);
                let result = ensure_private_dir(&scratch)
                    .map_err(|e| CatalogFailure::other(format!("catalogue scratch: {}", e.kind())))
                    .and_then(|()| {
                        launcher
                            .catalog(&scratch, models.as_deref())
                            .map_err(|e| CatalogFailure::launch(&e))
                    })
                    .and_then(|bytes| parse_catalog(&bytes).map_err(CatalogFailure::other));
                let _ = fs::remove_dir_all(&scratch);
                Completed::Catalog {
                    result: result.map_err(|failure| {
                        eprintln!("laded: model catalogue unavailable: {}", failure.detail);
                        failure.message.to_owned()
                    }),
                }
            });
        }
        Flow::Deferred
    }

    pub(crate) fn catalog_done(&mut self, result: Result<CatalogData, String>) {
        self.catalog.listing = false;
        let data = match result {
            Ok(data) => {
                let data = Arc::new(data);
                self.catalog.data = Some(data.clone());
                data
            }
            Err(message) => Arc::new(fallback_catalog(
                self.test_fixture_provider,
                preview(&message, 512),
            )),
        };
        for (ctx, query) in std::mem::take(&mut self.catalog.waiting) {
            let reply = self.catalog_reply(&data, &query);
            self.finish(ctx, Ok(reply));
        }
    }

    /// Forgets the cached catalogue after the profile changed, so the next listing asks pi again.
    pub(crate) fn invalidate_catalog(&mut self) {
        self.catalog.data = None;
    }

    fn catalog_reply(&self, data: &CatalogData, query: &ModelsListQuery) -> ReplyData {
        let offered =
            |facts: &ProviderFacts| self.test_fixture_provider || facts.id != FIXTURE_PROVIDER;
        let mut providers: Vec<ProviderInfo> = data
            .providers
            .iter()
            .filter(|(facts, _)| offered(facts))
            .map(|(facts, models)| {
                let access = self.launcher.provider_access(facts);
                ProviderInfo {
                    provider: facts.id.clone(),
                    name: preview(&facts.name, 256),
                    auth: access.auth.iter().map(|a| wire_auth(*a)).take(8).collect(),
                    status: wire_status(access.status),
                    models: Counter::new(models.len() as u64),
                    reason: access.reason.map(|r| preview(&r, 512)),
                    variables: (!access.variables.is_empty()).then(|| {
                        access
                            .variables
                            .iter()
                            .take(8)
                            .map(|v| preview(v, 128))
                            .collect()
                    }),
                }
            })
            .collect();
        providers.sort_by_key(|p| match p.status {
            ProviderStatus::Available => 0,
            ProviderStatus::MissingCredentials => 1,
            ProviderStatus::Restricted => 2,
        });
        let matching: Vec<&ModelInfo> = data
            .providers
            .iter()
            .filter(|(facts, _)| {
                offered(facts) && query.provider.as_deref() == Some(facts.id.as_str())
            })
            .flat_map(|(_, models)| models)
            .collect();
        ReplyData::ModelsList(ModelCatalog {
            sdk_version: data.sdk_version.clone(),
            providers,
            truncated: matching.len() > MAX_REPLY_MODELS,
            models: matching
                .into_iter()
                .take(MAX_REPLY_MODELS)
                .cloned()
                .collect(),
            error: data.error.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_failures_keep_the_sentence_and_the_detail_apart() {
        let sandbox = CatalogFailure::launch(&LaunchError::SandboxUnavailable(
            "sandbox_unavailable: the probe sandbox exited with status 1: bwrap: Creating new namespace failed".into(),
        ));
        assert_eq!(sandbox.message, CATALOG_SANDBOX_FAILED);
        assert!(sandbox.detail.contains("Creating new namespace failed"));
        assert!(!sandbox.message.contains("sandbox_unavailable"));
        let other = CatalogFailure::other("catalogue output is not JSON".into());
        assert_eq!(other.message, CATALOG_FAILED);
    }

    #[test]
    fn catalogue_parses_the_worker_shape_and_skips_incomplete_entries() {
        let bytes = br#"{"sdk_version":"1.5.0","error":null,"providers":[
            {"id":"anthropic","name":"Anthropic","api_key":true,"oauth":null,"subscription":false,
             "base_url":null,"models":[
                {"id":"opus","name":"","reasoning":true,"thinking":["off","high","sideways"]},
                {"id":"","name":"nameless","reasoning":false,"thinking":[]}]},
            {"id":"","name":"no id","models":[]}]}"#;
        let data = parse_catalog(bytes).unwrap();
        assert_eq!(data.sdk_version, "1.5.0");
        assert_eq!(data.error, None);
        let [(facts, models)] = data.providers.as_slice() else {
            panic!("{:?}", data.providers);
        };
        assert_eq!(
            (facts.id.as_str(), facts.api_key, facts.oauth.as_deref()),
            ("anthropic", true, None)
        );
        let [model] = models.as_slice() else {
            panic!("{models:?}");
        };
        assert_eq!((model.id.as_str(), model.name.as_str()), ("opus", "opus"));
        assert_eq!(
            model.thinking_levels,
            [ThinkingLevel::Off, ThinkingLevel::High]
        );
        assert!(parse_catalog(br#"{"providers":[]}"#).is_err());
        assert!(parse_catalog(b"not json").is_err());
    }
}
