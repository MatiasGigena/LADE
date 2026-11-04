//! Reviewed facts about the built-in model providers and the address ranges no provider
//! endpoint may resolve to. Profile import validates custom providers against these tables, and
//! supervision derives worker egress policies and credential lookups from them.

use std::net::{IpAddr, Ipv4Addr};

pub const FIXTURE_PROVIDER: &str = "lade-fixture";

#[derive(Clone, Copy, Debug)]
pub struct BuiltInProvider {
    pub id: &'static str,
    /// API hosts the worker may reach over HTTPS for this provider.
    pub hosts: &'static [&'static str],
    /// Environment variables that carry the provider's credential, in lookup order.
    pub env_vars: &'static [&'static str],
}

pub const BUILT_IN_PROVIDERS: &[BuiltInProvider] = &[
    BuiltInProvider {
        id: FIXTURE_PROVIDER,
        hosts: &[],
        env_vars: &[],
    },
    BuiltInProvider {
        id: "anthropic",
        hosts: &["api.anthropic.com"],
        env_vars: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_AUTH_TOKEN",
        ],
    },
    BuiltInProvider {
        id: "openai",
        hosts: &["api.openai.com"],
        env_vars: &["OPENAI_API_KEY"],
    },
    BuiltInProvider {
        id: "google",
        hosts: &["generativelanguage.googleapis.com"],
        env_vars: &["GEMINI_API_KEY"],
    },
    BuiltInProvider {
        id: "openrouter",
        hosts: &["openrouter.ai"],
        env_vars: &["OPENROUTER_API_KEY"],
    },
    BuiltInProvider {
        id: "mistral",
        hosts: &["api.mistral.ai"],
        env_vars: &["MISTRAL_API_KEY"],
    },
    BuiltInProvider {
        id: "groq",
        hosts: &["api.groq.com"],
        env_vars: &["GROQ_API_KEY"],
    },
    BuiltInProvider {
        id: "xai",
        hosts: &["api.x.ai"],
        env_vars: &["XAI_API_KEY"],
    },
    BuiltInProvider {
        id: "deepseek",
        hosts: &["api.deepseek.com"],
        env_vars: &["DEEPSEEK_API_KEY"],
    },
    BuiltInProvider {
        id: "cerebras",
        hosts: &["api.cerebras.ai"],
        env_vars: &["CEREBRAS_API_KEY"],
    },
    BuiltInProvider {
        id: "together",
        hosts: &["api.together.ai"],
        env_vars: &["TOGETHER_API_KEY"],
    },
    BuiltInProvider {
        id: "fireworks",
        hosts: &["api.fireworks.ai"],
        env_vars: &["FIREWORKS_API_KEY"],
    },
    BuiltInProvider {
        id: "moonshotai",
        hosts: &["api.moonshot.ai"],
        env_vars: &["MOONSHOT_API_KEY"],
    },
    BuiltInProvider {
        id: "huggingface",
        hosts: &["router.huggingface.co"],
        env_vars: &["HF_TOKEN"],
    },
    BuiltInProvider {
        id: "nvidia",
        hosts: &["integrate.api.nvidia.com"],
        env_vars: &["NVIDIA_API_KEY"],
    },
    BuiltInProvider {
        id: "zai",
        hosts: &["api.z.ai"],
        env_vars: &["ZAI_API_KEY"],
    },
    BuiltInProvider {
        id: "vercel-ai-gateway",
        hosts: &["ai-gateway.vercel.sh"],
        env_vars: &["AI_GATEWAY_API_KEY"],
    },
    BuiltInProvider {
        id: "kimi-coding",
        hosts: &["api.kimi.com"],
        env_vars: &["KIMI_API_KEY"],
    },
];

#[must_use]
pub fn built_in_provider(id: &str) -> Option<&'static BuiltInProvider> {
    BUILT_IN_PROVIDERS.iter().find(|p| p.id == id)
}

/// True for loopback, private, link-local, documentation and other addresses that are not
/// reachable public Internet hosts, including IPv4-mapped and NAT64 forms of them.
#[must_use]
pub fn is_non_public(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_non_public_v4(v4);
            }
            let seg = v6.segments();
            if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = seg[6].to_be_bytes();
                let [c, d] = seg[7].to_be_bytes();
                return is_non_public_v4(Ipv4Addr::new(a, b, c, d));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] & 0xffc0) == 0xfec0
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)
                || seg[..6] == [0, 0, 0, 0, 0, 0]
        }
    }
}

fn is_non_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_documentation()
        || a == 0
        || (a == 100 && (b & 0xc0) == 64)
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b & 0xfe) == 18)
        || a >= 240
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        for addr in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(is_non_public(addr.parse().unwrap()), "{addr}");
        }
        for addr in [
            "1.1.1.1",
            "160.79.104.10",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_non_public(addr.parse().unwrap()), "{addr}");
        }
    }
}
