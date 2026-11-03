use std::collections::BTreeMap;

use super::credentials::{CredentialSource, FIXTURE_PROVIDER, provider_env_vars};
use super::gateway::EgressPolicy;
use crate::adapters::profile::CustomProvider;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderFacts {
    pub id: String,
    pub name: String,
    pub api_key: bool,
    pub oauth: Option<String>,
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
    pub variables: Vec<String>,
}

const SUBSCRIPTION_VARS: &[&str] = &["ANTHROPIC_OAUTH_TOKEN", "KIMI_API_KEY"];

pub const OAUTH_RESTRICTION: &str = "interactive OAuth login and refresh-token storage are restricted: refresh-token \
     credential custody (who runs the login, who refreshes, where the rotated token is kept) is an open product \
     decision, and tokens in the sandbox's auth store would be readable by every tool";

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

fn missing_key(facts: &ProviderFacts, auth: &[AuthKind]) -> String {
    let name = if facts.name.trim().is_empty() {
        &facts.id
    } else {
        &facts.name
    };
    let key = if auth.contains(&AuthKind::SubscriptionToken) {
        "an API key or subscription token"
    } else {
        "an API key"
    };
    format!("Add {key} for {name} to use it.")
}

const KEYRING_AT_START: &str = "LADE looks for its key in your keyring when the agent starts.";

fn without_plain_key(credentials: &CredentialSource, missing: String) -> (AccessStatus, String) {
    let keyring = credentials.secret_service();
    if keyring.is_enabled() && keyring.unavailable_reason().is_none() {
        (AccessStatus::Available, KEYRING_AT_START.to_owned())
    } else {
        (AccessStatus::MissingCredentials, missing)
    }
}

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
        let (status, reason) = if available {
            (AccessStatus::Available, endpoint)
        } else {
            without_plain_key(
                credentials,
                format!(
                    "Add the key for the custom provider {} to use it.",
                    provider.id
                ),
            )
        };
        return ProviderAccess {
            auth: vec![AuthKind::Custom],
            status,
            reason: Some(reason),
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
    let (status, reason) = if credentials.is_available(&facts.id) {
        (AccessStatus::Available, None)
    } else {
        let (status, reason) = without_plain_key(credentials, missing_key(facts, &auth));
        (status, Some(reason))
    };
    ProviderAccess {
        auth,
        status,
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
        assert_eq!(
            anthropic.reason.as_deref(),
            Some("Add an API key or subscription token for anthropic to use it.")
        );
        let groq = classify(&facts("groq", true, None), &policies, &empty, &[]);
        assert_eq!(
            groq.reason.as_deref(),
            Some("Add an API key for groq to use it.")
        );
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
        assert_eq!(subscribed.reason, None);

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

    #[test]
    fn a_usable_keyring_keeps_a_keyless_provider_startable() {
        use crate::supervision::secret_service::SecretService;
        let policies = default_policies();
        let groq = facts("groq", true, None);
        let unusable =
            CredentialSource::empty().with_secret_service(SecretService::detect(None, |_| None));
        let access = classify(&groq, &policies, &unusable, &[]);
        assert_eq!(access.status, AccessStatus::MissingCredentials);
        assert_eq!(
            access.reason.as_deref(),
            Some("Add an API key for groq to use it.")
        );
        let keyring = CredentialSource::empty().with_secret_service(SecretService::new(
            "/usr/bin/secret-tool".into(),
            Vec::new(),
        ));
        let access = classify(&groq, &policies, &keyring, &[]);
        assert_eq!(access.status, AccessStatus::Available);
        assert_eq!(access.reason.as_deref(), Some(KEYRING_AT_START));
        assert_eq!(access.variables, ["GROQ_API_KEY"]);
        let custom = CustomProvider {
            id: "remote".into(),
            host: "llm.example".into(),
            port: 443,
            local: false,
            vars: vec!["LADE_PROVIDER_REMOTE".into()],
        };
        let remote = facts("remote", true, None);
        let access = classify(&remote, &policies, &unusable, std::slice::from_ref(&custom));
        assert_eq!(access.status, AccessStatus::MissingCredentials);
        let access = classify(&remote, &policies, &keyring, &[custom]);
        assert_eq!(access.status, AccessStatus::Available);
    }

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
