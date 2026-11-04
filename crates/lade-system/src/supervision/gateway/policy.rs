//! Egress policy: the HTTPS hosts a worker's gateway may reach, per provider.

use std::collections::BTreeMap;

use crate::adapters::providers::BUILT_IN_PROVIDERS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressRule {
    pub host: String,
    pub port: u16,
    pub allow_private: bool,
}

impl EgressRule {
    #[must_use]
    pub fn https(host: &str) -> Self {
        Self {
            host: normalize_host(host),
            port: 443,
            allow_private: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    rules: Vec<EgressRule>,
}

impl EgressPolicy {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn https_hosts(hosts: &[&str]) -> Self {
        Self {
            rules: hosts.iter().map(|h| EgressRule::https(h)).collect(),
        }
    }

    #[must_use]
    pub fn with_extra(mut self, rules: impl IntoIterator<Item = EgressRule>) -> Self {
        for mut rule in rules {
            rule.host = normalize_host(&rule.host);
            if !self.rules.contains(&rule) {
                self.rules.push(rule);
            }
        }
        self
    }

    #[must_use]
    pub fn rules(&self) -> &[EgressRule] {
        &self.rules
    }

    #[must_use]
    pub fn allows(&self, host: &str, port: u16) -> Option<&EgressRule> {
        let host = normalize_host(host);
        self.rules.iter().find(|r| r.host == host && r.port == port)
    }
}

/// The reviewed endpoint policy of every built-in provider: HTTPS to its documented API hosts.
#[must_use]
pub fn default_policies() -> BTreeMap<String, EgressPolicy> {
    BUILT_IN_PROVIDERS
        .iter()
        .map(|p| (p.id.to_owned(), EgressPolicy::https_hosts(p.hosts)))
        .collect()
}

pub(super) fn normalize_host(host: &str) -> String {
    let host = host.strip_suffix('.').unwrap_or(host);
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_matching() {
        let policy = default_policies()["anthropic"]
            .clone()
            .with_extra([EgressRule::https("Extra.Example.")]);
        assert!(policy.allows("api.anthropic.com", 443).is_some());
        assert!(policy.allows("API.ANTHROPIC.COM.", 443).is_some());
        assert!(policy.allows("api.anthropic.com", 80).is_none());
        assert!(policy.allows("evil.api.anthropic.com", 443).is_none());
        assert!(policy.allows("extra.example", 443).is_some());
        assert!(default_policies()["lade-fixture"].rules().is_empty());
    }
}
