//! Custom providers declared in the profile's `models.json`, and the trust rules they must
//! follow.

use std::collections::BTreeSet;
use std::fs;
use std::net::IpAddr;
use std::path::Path;

use serde_json::Value;

use crate::adapters::providers::{built_in_provider, is_non_public};

use super::{CUSTOM_VAR_PREFIX, MAX_MODELS_JSON};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomProvider {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub local: bool,
    pub vars: Vec<String>,
}

pub(super) fn variable(value: &str) -> Option<&str> {
    let name = value
        .strip_prefix("${")
        .and_then(|v| v.strip_suffix('}'))
        .or_else(|| value.strip_prefix('$'))?;
    let valid = name.starts_with(CUSTOM_VAR_PREFIX)
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    valid.then_some(name)
}

pub(super) fn endpoint(url: &str) -> Result<(bool, String, u16), String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err("baseUrl must be an http(s) URL".into());
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("baseUrl needs a host and no user information".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, tail) = v6.split_once(']').ok_or("invalid IPv6 host")?;
        (host.to_string(), tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        Some(port) => port.parse::<u16>().map_err(|_| "invalid baseUrl port")?,
        None if secure => 443,
        None => 80,
    };
    let host = host.to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    if !valid || port == 0 {
        return Err("invalid baseUrl host".into());
    }
    Ok((secure, host, port))
}

pub(super) fn is_local(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(is_non_public)
}

pub(super) fn read_models(path: &Path) -> Result<(Vec<u8>, Vec<CustomProvider>), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    if !meta.is_file() {
        return Err("models.json is not a regular file".into());
    }
    if meta.len() > MAX_MODELS_JSON {
        return Err("models.json is larger than 1 MiB".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    let providers = custom_providers(&bytes)?;
    Ok((bytes, providers))
}

/// The custom providers declared in a `models.json` document.
///
/// # Errors
///
/// Returns a reason the developer can read when the document is not JSON, has no `providers`
/// object, or declares a provider that is malformed or not allowed.
pub fn custom_providers(bytes: &[u8]) -> Result<Vec<CustomProvider>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "models.json is not valid JSON".to_string())?;
    let providers = value
        .get("providers")
        .and_then(Value::as_object)
        .ok_or("models.json needs a providers object")?;
    let mut found = Vec::new();
    for (id, config) in providers {
        let valid_id = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
            });
        if !valid_id {
            return Err(format!("provider id {id:?} is not a lowercase label"));
        }
        if built_in_provider(id).is_some() {
            return Err(format!(
                "provider {id} redefines a built-in provider; its reviewed endpoint policy applies"
            ));
        }
        let config = config
            .as_object()
            .ok_or(format!("provider {id} must be an object"))?;
        if config.contains_key("oauth") {
            return Err(format!(
                "provider {id}: OAuth providers are not supported (no credential custody)"
            ));
        }
        let url = config
            .get("baseUrl")
            .and_then(Value::as_str)
            .ok_or(format!("provider {id} needs a baseUrl"))?;
        let (secure, host, port) = endpoint(url).map_err(|e| format!("provider {id}: {e}"))?;
        let local = is_local(&host);
        if !secure && !local {
            return Err(format!(
                "provider {id}: plain HTTP is allowed only for local servers"
            ));
        }
        let mut vars = BTreeSet::new();
        let mut check = |what: &str, value: &str| -> Result<(), String> {
            if value.starts_with('!') {
                return Err(format!(
                    "provider {id}: {what} runs a shell command, which LADE refuses"
                ));
            }
            if let Some(name) = variable(value) {
                vars.insert(name.to_string());
                return Ok(());
            }
            if value.contains('$') {
                return Err(format!(
                    "provider {id}: {what} may reference one whole {CUSTOM_VAR_PREFIX}* variable only"
                ));
            }
            if what == "apiKey" && !local {
                return Err(format!(
                    "provider {id}: put the key in credentials.env and reference it as ${CUSTOM_VAR_PREFIX}…"
                ));
            }
            Ok(())
        };
        if let Some(key) = config.get("apiKey") {
            check(
                "apiKey",
                key.as_str()
                    .ok_or(format!("provider {id}: apiKey must be text"))?,
            )?;
        }
        if let Some(headers) = config.get("headers") {
            let headers = headers
                .as_object()
                .ok_or(format!("provider {id}: headers must be an object"))?;
            for value in headers.values() {
                check(
                    "a header",
                    value
                        .as_str()
                        .ok_or(format!("provider {id}: header values must be text"))?,
                )?;
            }
        }
        found.push(CustomProvider {
            id: id.clone(),
            host,
            port,
            local,
            vars: vars.into_iter().collect(),
        });
    }
    Ok(found)
}

/// The custom providers of the LADE profile at `profile`; none when it has no
/// `models.json`.
///
/// # Errors
///
/// Returns a reason the developer can read when `models.json` is not a regular file, is larger
/// than 1 MiB, cannot be read, or fails [`custom_providers`].
pub fn profile_providers(profile: &Path) -> Result<Vec<CustomProvider>, String> {
    let path = profile.join("models.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_models(&path).map(|(_, providers)| providers)
}
