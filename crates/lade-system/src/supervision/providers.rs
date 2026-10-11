//! Provider access as the developer sees it: for every provider of the pinned SDK catalogue (and
//! the LADE profile's custom providers) which authentication LADE can use, whether it is usable now,
//! and — when it is not — the trust rule or missing credential that explains why. Nothing here is a
//! silent omission: unsupported authentication is an explicit `Restricted` with a reason.

use std::collections::BTreeMap;

use super::credentials::{CredentialSource, FIXTURE_PROVIDER, provider_env_vars};
use super::gateway::EgressPolicy;
use crate::adapters::profile::CustomProvider;

/// Facts the SDK catalogue reports about one provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderFacts {
    pub id: String,
    pub name: String,
    /// pi offers key-style authentication (an environment variable or stored key).
    pub api_key: bool,
    /// The provider's interactive OAuth login, if any (for example "Anthropic (Claude Pro/Max)").
    pub oauth: Option<String>,
    /// The OAuth login is backed by a subscription.
    pub subscription: bool,
    pub base_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AuthKind {
    None,
    ApiKey,
    SubscriptionToken,
    OauthLogin,
    CloudCredentials,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessStatus {
    Available,
    MissingCredentials,
    Restricted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAccess {
    pub auth: Vec<AuthKind>,
    pub status: AccessStatus,
    pub reason: Option<String>,
    /// Credential variable names (never values) that make the provider available.
    pub variables: Vec<String>,
}

/// Subscription access that pi accepts as a plain variable (works like an API key here).
const SUBSCRIPTION_VARS: &[&str] = &["ANTHROPIC_OAUTH_TOKEN", "KIMI_API_KEY"];

pub const OAUTH_RESTRICTION: &str = "interactive OAuth login and refresh-token storage are restricted: refresh-token \
     credential custody (who runs the login, who refreshes, where the rotated token is kept) is an open product \
     decision (#43), and tokens in the sandbox's auth store would be readable by every tool";

fn restricted(auth: Vec<AuthKind>, reason: String) -> ProviderAccess {
    ProviderAccess {
        auth,
        status: AccessStatus::Restricted,
        reason: Some(reason),
        variables: Vec::new(),
    }
}

fn cloud(id: &str) -> Option<&'static str> {
    Some(match id {
        "amazon-bedrock" => {
            "AWS profiles and keys are not released, and the Bedrock SDK's node:http transport bypasses the LADE \
             gateway"
        }
        "google-vertex" => {
            "Application Default Credentials files are not released into the sandbox"
        }
        "azure-openai-responses" => {
            "the endpoint host is specific to each Azure resource; there is no reviewed per-resource egress policy"
        }
        "cloudflare-ai-gateway" | "cloudflare-workers-ai" => {
            "the endpoint host is specific to each Cloudflare account; there is no reviewed egress policy"
        }
        _ => return None,
    })
}

/// Classifies one provider against the reviewed egress policies, the credential source and the
/// LADE profile's custom providers.
pub fn classify(
    facts: &ProviderFacts,
    policies: &BTreeMap<String, EgressPolicy>,
    credentials: &CredentialSource,
    custom: &[CustomProvider],
) -> ProviderAccess {
    if facts.id == FIXTURE_PROVIDER {
        return ProviderAccess {
            auth: vec![AuthKind::None],
            status: AccessStatus::Available,
            reason: Some("offline scripted fixture model, not a real model".into()),
            variables: Vec::new(),
        };
    }
    if let Some(provider) = custom.iter().find(|p| p.id == facts.id) {
        let available = credentials.is_available_custom(&provider.vars);
        let endpoint = format!(
            "custom provider from the LADE profile at {}:{}{}",
            provider.host,
            provider.port,
            if provider.local { " (local)" } else { "" }
        );
        return ProviderAccess {
            auth: vec![AuthKind::Custom],
            status: if available {
                AccessStatus::Available
            } else {
                AccessStatus::MissingCredentials
            },
            reason: Some(if available {
                endpoint
            } else {
                format!(
                    "{endpoint}; set {} in the laded environment, the Secret Service (laded \
                     credentials set {} --variable NAME; read only when execution starts) or \
                     ~/.config/lade/credentials.env",
                    provider.vars.join(" and "),
                    provider.id
                )
            }),
            variables: provider.vars.clone(),
        };
    }
    let mut auth = Vec::new();
    if facts.api_key {
        auth.push(AuthKind::ApiKey);
    }
    let vars = provider_env_vars(&facts.id);
    if vars.is_some_and(|vars| vars.iter().any(|v| SUBSCRIPTION_VARS.contains(v))) {
        auth.push(AuthKind::SubscriptionToken);
    }
    if facts.oauth.is_some() {
        auth.push(AuthKind::OauthLogin);
    }
    if let Some(why) = cloud(&facts.id) {
        let mut auth = vec![AuthKind::CloudCredentials];
        if facts.oauth.is_some() {
            auth.push(AuthKind::OauthLogin);
        }
        return restricted(auth, why.to_string());
    }
    let oauth_note = facts.oauth.as_ref().map(|name| {
        format!(
            "{name}: {OAUTH_RESTRICTION}{}",
            if facts.id == "github-copilot" {
                "; Copilot also resolves per-account endpoint hosts"
            } else {
                ""
            }
        )
    });
    let (Some(vars), true) = (vars, policies.contains_key(&facts.id)) else {
        let endpoint = facts
            .base_url
            .as_deref()
            .unwrap_or("an account-specific endpoint");
        let base = format!("no reviewed egress policy in this LADE build for {endpoint}");
        return restricted(
            auth,
            match oauth_note {
                Some(note) => format!("{base}; {note}"),
                None => base,
            },
        );
    };
    // The Secret Service is not asked here (that would read every provider's key): a key kept
    // only there shows as missing until execution starts and looks it up.
    let available = credentials.is_available(&facts.id);
    let mut reason = if available {
        None
    } else {
        Some(format!(
            "set {} in the laded environment, the Secret Service (laded credentials set {}; read \
             only when execution starts) or ~/.config/lade/credentials.env",
            vars.join(" or "),
            facts.id
        ))
    };
    if let Some(note) = oauth_note {
        reason = Some(match reason {
            Some(missing) => format!("{missing}; {note}"),
            None => note,
        });
    }
    ProviderAccess {
        auth,
        status: if available {
            AccessStatus::Available
        } else {
            AccessStatus::MissingCredentials
        },
        reason,
        variables: vars.iter().map(|v| (*v).to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervision::gateway::default_policies;
    use std::ffi::OsString;

    fn facts(id: &str, api_key: bool, oauth: Option<&str>) -> ProviderFacts {
        ProviderFacts {
            id: id.into(),
            name: id.into(),
            api_key,
            oauth: oauth.map(str::to_string),
            subscription: oauth.is_some(),
            base_url: Some(format!("https://{id}.example")),
        }
    }

    #[test]
    fn every_provider_class_is_supported_or_explicitly_restricted() {
        let policies = default_policies();
        let empty = CredentialSource::empty();
        let fixture = classify(&facts("lade-fixture", true, None), &policies, &empty, &[]);
        assert_eq!(fixture.status, AccessStatus::Available);

        let anthropic = classify(
            &facts("anthropic", true, Some("Anthropic (Claude Pro/Max)")),
            &policies,
            &empty,
            &[],
        );
        assert_eq!(anthropic.status, AccessStatus::MissingCredentials);
        assert_eq!(
            anthropic.auth,
            [
                AuthKind::ApiKey,
                AuthKind::SubscriptionToken,
                AuthKind::OauthLogin
            ]
        );
        assert!(anthropic.reason.as_deref().unwrap().contains("OAuth login"));
        assert!(
            anthropic
                .variables
                .contains(&"ANTHROPIC_OAUTH_TOKEN".to_string())
        );

        let token = CredentialSource::new(
            [(OsString::from("ANTHROPIC_OAUTH_TOKEN"), OsString::from("t"))],
            None,
        );
        let subscribed = classify(
            &facts("anthropic", true, Some("Anthropic (Claude Pro/Max)")),
            &policies,
            &token,
            &[],
        );
        assert_eq!(subscribed.status, AccessStatus::Available);

        let codex = classify(
            &facts("openai-codex", false, Some("OpenAI (ChatGPT Plus/Pro)")),
            &policies,
            &empty,
            &[],
        );
        assert_eq!(codex.status, AccessStatus::Restricted);
        assert_eq!(codex.auth, [AuthKind::OauthLogin]);
        assert!(codex.reason.unwrap().contains("credential custody"));

        let bedrock = classify(&facts("amazon-bedrock", true, None), &policies, &empty, &[]);
        assert_eq!(bedrock.status, AccessStatus::Restricted);
        assert_eq!(bedrock.auth, [AuthKind::CloudCredentials]);

        let unknown = classify(&facts("baseten", true, None), &policies, &empty, &[]);
        assert_eq!(unknown.status, AccessStatus::Restricted);
        assert!(
            unknown
                .reason
                .unwrap()
                .contains("no reviewed egress policy")
        );

        let custom = CustomProvider {
            id: "local".into(),
            host: "127.0.0.1".into(),
            port: 11434,
            local: true,
            vars: Vec::new(),
        };
        let local = classify(&facts("local", true, None), &policies, &empty, &[custom]);
        assert_eq!(local.status, AccessStatus::Available);
        assert_eq!(local.auth, [AuthKind::Custom]);
    }

    /// The pinned compatibility manifest states exactly what this build's tables decide.
    #[test]
    fn compatibility_manifest_matches_the_reviewed_tables() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/implementation/pi-compatibility.json"
        ))
        .unwrap();
        let policies = default_policies();
        let providers = manifest["providers"].as_array().unwrap();
        assert!(providers.len() > 30);
        for provider in providers {
            let id = provider["id"].as_str().unwrap();
            let status = provider["status"].as_str().unwrap();
            let usable =
                provider_env_vars(id).is_some() && policies.contains_key(id) && cloud(id).is_none();
            assert_eq!(status == "supported", usable, "{id}");
            if status == "restricted" {
                let reason = provider["reason"].as_str().unwrap();
                assert!(!reason.is_empty(), "{id}");
                if let Some(why) = cloud(id) {
                    assert_eq!(reason, why, "{id}");
                }
            } else if id != FIXTURE_PROVIDER {
                let vars: Vec<&str> = provider["variables"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                assert_eq!(Some(vars.as_slice()), provider_env_vars(id), "{id}");
            }
        }
    }
}
