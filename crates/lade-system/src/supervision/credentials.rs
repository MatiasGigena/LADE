//! Provider credential custody for worker launches.
//!
//! Credentials come from three places, in this order of precedence for each variable:
//!
//! 1. `laded`'s own process environment (an explicit operator override, for example in the unit);
//! 2. the user's Secret Service (GNOME Keyring), through `secret-tool`, when it is installed and a
//!    session bus exists ([`super::secret_service`]): items `service=lade provider=<id>
//!    variable=<NAME>`, stored with `laded credentials set <provider>`;
//! 3. the optional file `$XDG_CONFIG_HOME/lade/credentials.env` (default
//!    `~/.config/lade/credentials.env`), the plaintext fallback the user writes by hand.
//!
//! The Secret Service ranks above the file so that a key moved into the keyring is not shadowed by
//! a stale plaintext copy, and below the environment so that a deliberate override keeps working.
//! It is consulted only when a worker for the selected provider launches (and by the start
//! preflight when neither the environment nor the file satisfies the provider), with a deadline;
//! an unavailable Secret Service is reported, never fatal, and never causes a key to be written
//! anywhere. The file must be a regular file owned by the service user with no group/other
//! permission bits; otherwise it is refused (there is no fallback location). Only the variables
//! of the selected provider are released to a launch, values are never logged, and every `Debug`
//! output redacts them. The released key is readable by the worker process and its tools (the
//! in-process credential exposure accepted by issue #8).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::secret_service::{Lookup, SecretService};

/// Largest accepted credentials file.
const MAX_FILE_BYTES: u64 = 64 << 10;

/// Providers that need no credentials (the offline fixture model).
pub const FIXTURE_PROVIDER: &str = "lade-fixture";

/// Credential variables per provider, mirroring pi-ai 0.85.1 `env-api-keys.js`
/// (`getApiKeyEnvVars`). Any one present variable satisfies the provider; all present ones are
/// released. `ANTHROPIC_OAUTH_TOKEN` is a Claude subscription token (for example from
/// `claude setup-token`) and `KIMI_API_KEY` a Kimi Code subscription key: subscription access that
/// is a plain variable works like an API key. Providers whose pi authentication is not a plain
/// variable (Bedrock profiles, Vertex ADC, interactive OAuth logins) are not listed and therefore
/// unavailable; see `supervision::providers` for the reasons shown to the developer.
const PROVIDER_VARS: &[(&str, &[&str])] = &[
    (FIXTURE_PROVIDER, &[]),
    (
        "anthropic",
        &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_AUTH_TOKEN",
        ],
    ),
    ("openai", &["OPENAI_API_KEY"]),
    ("google", &["GEMINI_API_KEY"]),
    ("openrouter", &["OPENROUTER_API_KEY"]),
    ("mistral", &["MISTRAL_API_KEY"]),
    ("groq", &["GROQ_API_KEY"]),
    ("xai", &["XAI_API_KEY"]),
    ("deepseek", &["DEEPSEEK_API_KEY"]),
    ("cerebras", &["CEREBRAS_API_KEY"]),
    ("together", &["TOGETHER_API_KEY"]),
    ("fireworks", &["FIREWORKS_API_KEY"]),
    ("moonshotai", &["MOONSHOT_API_KEY"]),
    ("huggingface", &["HF_TOKEN"]),
    ("nvidia", &["NVIDIA_API_KEY"]),
    ("zai", &["ZAI_API_KEY"]),
    ("vercel-ai-gateway", &["AI_GATEWAY_API_KEY"]),
    ("kimi-coding", &["KIMI_API_KEY"]),
];

/// The credential variable names pi reads for `provider`, or `None` for an unknown provider.
pub fn provider_env_vars(provider: &str) -> Option<&'static [&'static str]> {
    PROVIDER_VARS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, vars)| *vars)
}

fn known_var(name: &str) -> bool {
    PROVIDER_VARS.iter().any(|(_, vars)| vars.contains(&name)) || custom_var(name)
}

/// Custom providers of the LADE profile reference credentials as `LADE_PROVIDER_*` variables.
pub fn custom_var(name: &str) -> bool {
    name.strip_prefix(crate::adapters::profile::CUSTOM_VAR_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty()
                && name.len() <= 128
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// A credential value. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(OsString);

impl Secret {
    pub(crate) fn new(value: OsString) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &OsStr {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The variables released for one launch (only the selected provider's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleasedCredentials {
    pub vars: Vec<(String, Secret)>,
}

impl ReleasedCredentials {
    pub fn names(&self) -> Vec<&str> {
        self.vars.iter().map(|(k, _)| k.as_str()).collect()
    }
}

#[derive(Debug)]
pub enum CredentialError {
    UnknownProvider(String),
    /// None of the provider's variables is set (names only). `secret_service` says why the
    /// Secret Service could not be asked, when it could not.
    Missing {
        provider: String,
        vars: &'static [&'static str],
        secret_service: Option<String>,
    },
    /// A custom provider's referenced variables are not set (names only).
    MissingCustom {
        provider: String,
        vars: Vec<String>,
        secret_service: Option<String>,
    },
    /// The credentials file exists but is not private to the service user.
    InsecureFile {
        path: PathBuf,
        reason: &'static str,
    },
    Io {
        path: PathBuf,
        kind: io::ErrorKind,
    },
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider(p) => write!(f, "no credential mapping for provider {p}"),
            Self::Missing {
                provider,
                vars,
                secret_service,
            } => {
                write!(
                    f,
                    "no credentials for provider {provider}: set {} in the laded environment, the \
                     Secret Service (laded credentials set {provider}) or the credentials file",
                    vars.join(" or ")
                )?;
                unavailable_note(f, secret_service)
            }
            Self::MissingCustom {
                provider,
                vars,
                secret_service,
            } => {
                write!(
                    f,
                    "no credentials for custom provider {provider}: set {} in the laded \
                     environment, the Secret Service (laded credentials set {provider} --variable \
                     NAME) or the credentials file",
                    vars.join(" and ")
                )?;
                unavailable_note(f, secret_service)
            }
            Self::InsecureFile { path, reason } => {
                write!(f, "credentials file {} refused: {reason}", path.display())
            }
            Self::Io { path, kind } => {
                write!(f, "credentials file {} unreadable: {kind}", path.display())
            }
        }
    }
}

impl std::error::Error for CredentialError {}

fn unavailable_note(f: &mut fmt::Formatter<'_>, reason: &Option<String>) -> fmt::Result {
    match reason {
        Some(reason) => write!(f, " (Secret Service unavailable: {reason})"),
        None => Ok(()),
    }
}

/// Variables found for one resolution and what is still missing.
struct Gathered {
    found: Vec<(String, Secret)>,
    missing: Vec<String>,
    /// Why the Secret Service could not answer, if it could not.
    secret_service: Option<String>,
}

/// Where credentials are read from. Holds only known credential variables of the environment
/// snapshot; the Secret Service is asked and the file re-read (and re-checked) on every
/// resolution.
#[derive(Clone)]
pub struct CredentialSource {
    env: Vec<(String, OsString)>,
    file: Option<PathBuf>,
    secret_service: SecretService,
}

impl fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("CredentialSource")
            .field("env", &names)
            .field("file", &self.file)
            .field("secret_service", &self.secret_service)
            .finish()
    }
}

impl CredentialSource {
    /// Known credential variables from `env` plus the optional credentials file; the Secret
    /// Service is not consulted (see [`Self::with_secret_service`]).
    pub fn new(env: impl IntoIterator<Item = (OsString, OsString)>, file: Option<PathBuf>) -> Self {
        let env = env
            .into_iter()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
            .filter(|(k, _)| known_var(k))
            .collect();
        Self {
            env,
            file,
            secret_service: SecretService::disabled(),
        }
    }

    /// The same source, consulting `secret_service` between the environment and the file.
    pub fn with_secret_service(mut self, secret_service: SecretService) -> Self {
        self.secret_service = secret_service;
        self
    }

    /// `laded`'s environment, the user's Secret Service and `$XDG_CONFIG_HOME/lade/credentials.env`
    /// (`$HOME/.config/lade/credentials.env` when `XDG_CONFIG_HOME` is unset or relative).
    pub fn from_process() -> Self {
        Self::new(std::env::vars_os(), default_file())
            .with_secret_service(SecretService::from_env())
    }

    /// No credentials at all (only the fixture provider resolves).
    pub fn empty() -> Self {
        Self::new(Vec::new(), None)
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Release the selected provider's variables: every one that is set, each from the first
    /// source that has it (environment, Secret Service, credentials file).
    pub fn resolve(&self, provider: &str) -> Result<ReleasedCredentials, CredentialError> {
        let vars = provider_env_vars(provider)
            .ok_or_else(|| CredentialError::UnknownProvider(provider.to_owned()))?;
        if vars.is_empty() {
            return Ok(ReleasedCredentials::default());
        }
        let gathered = self.gather(provider, vars, |g| g.found.is_empty())?;
        if gathered.found.is_empty() {
            return Err(CredentialError::Missing {
                provider: provider.to_owned(),
                vars,
                secret_service: gathered.secret_service,
            });
        }
        Ok(ReleasedCredentials {
            vars: gathered.found,
        })
    }

    /// Release exactly `vars` for a custom provider (`LADE_PROVIDER_*` names only). Every listed
    /// variable must be set; a provider without variables (a keyless local server) releases none.
    pub fn resolve_custom(
        &self,
        provider: &str,
        vars: &[String],
    ) -> Result<ReleasedCredentials, CredentialError> {
        if vars.iter().any(|v| !custom_var(v)) {
            return Err(CredentialError::UnknownProvider(provider.to_owned()));
        }
        if vars.is_empty() {
            return Ok(ReleasedCredentials::default());
        }
        let names: Vec<&str> = vars.iter().map(String::as_str).collect();
        let gathered = self.gather(provider, &names, |g| !g.missing.is_empty())?;
        if !gathered.missing.is_empty() {
            return Err(CredentialError::MissingCustom {
                provider: provider.to_owned(),
                vars: gathered.missing,
                secret_service: gathered.secret_service,
            });
        }
        Ok(ReleasedCredentials {
            vars: gathered.found,
        })
    }

    /// Whether a worker for `provider` could get its credentials now, without returning values.
    /// The Secret Service is asked only when the environment and the file do not already satisfy
    /// the provider.
    pub fn check(&self, provider: &str) -> Result<(), CredentialError> {
        let vars = provider_env_vars(provider)
            .ok_or_else(|| CredentialError::UnknownProvider(provider.to_owned()))?;
        if vars.is_empty() || self.plain_sources_have(vars, false)? {
            return Ok(());
        }
        self.resolve(provider).map(drop)
    }

    /// [`Self::check`] for a custom provider (every variable is required).
    pub fn check_custom(&self, provider: &str, vars: &[String]) -> Result<(), CredentialError> {
        let names: Vec<&str> = vars.iter().map(String::as_str).collect();
        if vars.iter().all(|v| custom_var(v)) && self.plain_sources_have(&names, true)? {
            return Ok(());
        }
        self.resolve_custom(provider, vars).map(drop)
    }

    /// Whether `provider` is satisfied by the environment or the credentials file alone. The
    /// Secret Service is never asked here (describing every provider must not read keys).
    pub fn is_available(&self, provider: &str) -> bool {
        provider_env_vars(provider).is_some_and(|vars| {
            vars.is_empty() || self.plain_sources_have(vars, false).unwrap_or(false)
        })
    }

    /// [`Self::is_available`] for a custom provider (every variable is required).
    pub fn is_available_custom(&self, vars: &[String]) -> bool {
        let names: Vec<&str> = vars.iter().map(String::as_str).collect();
        vars.iter().all(|v| custom_var(v)) && self.plain_sources_have(&names, true).unwrap_or(false)
    }

    /// The Secret Service this source consults.
    pub fn secret_service(&self) -> &SecretService {
        &self.secret_service
    }

    /// Any (with `all`: every) of `names` set in the environment or the file.
    fn plain_sources_have(&self, names: &[&str], all: bool) -> Result<bool, CredentialError> {
        let file_vars = self.file_vars()?;
        let has = |name: &&str| {
            self.env
                .iter()
                .chain(&file_vars)
                .any(|(k, v)| k == name && valid_value(v))
        };
        Ok(if all {
            names.iter().all(has)
        } else {
            names.iter().any(has)
        })
    }

    fn file_vars(&self) -> Result<Vec<(String, OsString)>, CredentialError> {
        match &self.file {
            Some(path) => read_file(path),
            None => Ok(Vec::new()),
        }
    }

    /// Each of `names` from the first source that has it. When `failed` says the result fails the
    /// resolution after the Secret Service found nothing, a locked keyring is looked for to
    /// explain it.
    fn gather(
        &self,
        provider: &str,
        names: &[&str],
        failed: impl Fn(&Gathered) -> bool,
    ) -> Result<Gathered, CredentialError> {
        let file_vars = self.file_vars()?;
        let service = &self.secret_service;
        let deadline = Instant::now() + service.timeout();
        let mut gathered = Gathered {
            found: Vec::new(),
            missing: Vec::new(),
            secret_service: service.unavailable_reason().map(str::to_owned),
        };
        let mut not_found = false;
        let from = |source: &[(String, OsString)], name: &str| {
            source
                .iter()
                .find(|(k, v)| k == name && valid_value(v))
                .map(|(_, v)| Secret(v.clone()))
        };
        for name in names {
            let mut value = from(&self.env, name);
            if value.is_none() && service.is_enabled() && gathered.secret_service.is_none() {
                match service.lookup(provider, name, deadline) {
                    Lookup::Found(secret) if valid_value(secret.expose()) => value = Some(secret),
                    Lookup::Found(_) | Lookup::NotFound => not_found = true,
                    Lookup::Unavailable(reason) => {
                        eprintln!(
                            "laded: Secret Service unavailable for provider {provider} ({reason}); \
                             using the environment and the credentials file only"
                        );
                        gathered.secret_service = Some(reason);
                    }
                }
            }
            match value.or_else(|| from(&file_vars, name)) {
                Some(value) => gathered.found.push(((*name).to_owned(), value)),
                None => gathered.missing.push((*name).to_owned()),
            }
        }
        if not_found && gathered.secret_service.is_none() && failed(&gathered) {
            gathered.secret_service = service.locked(provider, deadline);
        }
        Ok(gathered)
    }
}

fn valid_value(value: &OsStr) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty() && !bytes.iter().any(|b| matches!(b, 0 | b'\n' | b'\r'))
}

fn default_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("lade").join("credentials.env"))
}

/// The effective uid of this process (owner of `/proc/self`), without `unsafe`.
pub(crate) fn current_uid() -> io::Result<u32> {
    fs::metadata("/proc/self").map(|m| m.uid())
}

/// Read `NAME=value` lines (optional `export `, `#` comments, optional matching quotes). A
/// missing file yields nothing; an insecure one is refused.
fn read_file(path: &Path) -> Result<Vec<(String, OsString)>, CredentialError> {
    let io_err = |e: io::Error| CredentialError::Io {
        path: path.to_path_buf(),
        kind: e.kind(),
    };
    let insecure = |reason| CredentialError::InsecureFile {
        path: path.to_path_buf(),
        reason,
    };
    let link = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(e)),
    };
    if !link.file_type().is_file() {
        return Err(insecure("not a regular file"));
    }
    let mut file = fs::File::open(path).map_err(io_err)?;
    let meta = file.metadata().map_err(io_err)?;
    if meta.dev() != link.dev() || meta.ino() != link.ino() {
        return Err(insecure("replaced while opening"));
    }
    if meta.uid() != current_uid().map_err(io_err)? {
        return Err(insecure("not owned by the service user"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(insecure(
            "group or other permission bits set (use mode 0600)",
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(insecure("larger than 64 KiB"));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(insecure("larger than 64 KiB"));
    }
    Ok(parse_env_file(&bytes))
}

fn parse_env_file(bytes: &[u8]) -> Vec<(String, OsString)> {
    let mut vars = Vec::new();
    for line in bytes.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line).trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let line = line.strip_prefix(b"export ").unwrap_or(line).trim_ascii();
        let Some(eq) = line.iter().position(|b| *b == b'=') else {
            continue;
        };
        let Ok(name) = std::str::from_utf8(line[..eq].trim_ascii()) else {
            continue;
        };
        if !known_var(name) {
            continue;
        }
        let mut value = line[eq + 1..].trim_ascii();
        if value.len() >= 2
            && (value[0] == b'"' || value[0] == b'\'')
            && value[value.len() - 1] == value[0]
        {
            value = &value[1..value.len() - 1];
        }
        vars.push((name.to_owned(), OsStr::from_bytes(value).to_os_string()));
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_lines() {
        let vars = parse_env_file(
            b"# c\nexport OPENAI_API_KEY=\"sk-1\"\nUNKNOWN=x\n GROQ_API_KEY = 'g' \r\nbad\n",
        );
        assert_eq!(
            vars,
            vec![
                ("OPENAI_API_KEY".to_owned(), OsString::from("sk-1")),
                ("GROQ_API_KEY".to_owned(), OsString::from("g")),
            ]
        );
    }

    #[test]
    fn custom_variables_need_the_prefix_and_are_all_required() {
        let source = CredentialSource::new(
            [
                (OsString::from("LADE_PROVIDER_ONE"), OsString::from("v1")),
                (OsString::from("RANDOM_VAR"), OsString::from("x")),
            ],
            None,
        );
        assert!(format!("{source:?}").contains("LADE_PROVIDER_ONE"));
        assert!(!format!("{source:?}").contains("RANDOM_VAR"));
        let one = vec!["LADE_PROVIDER_ONE".to_string()];
        assert_eq!(
            source.resolve_custom("p", &one).unwrap().names(),
            ["LADE_PROVIDER_ONE"]
        );
        let two = vec![
            "LADE_PROVIDER_ONE".to_string(),
            "LADE_PROVIDER_TWO".to_string(),
        ];
        assert!(matches!(
            source.resolve_custom("p", &two),
            Err(CredentialError::MissingCustom { .. })
        ));
        assert!(
            source
                .resolve_custom("p", &["OPENAI_API_KEY".to_string()])
                .is_err()
        );
        assert!(source.resolve_custom("p", &[]).unwrap().vars.is_empty());
    }

    #[test]
    fn debug_redacts() {
        let source = CredentialSource::new(
            [(
                OsString::from("OPENAI_API_KEY"),
                OsString::from("sk-secret"),
            )],
            None,
        );
        let released = source.resolve("openai").unwrap();
        assert!(!format!("{source:?}{released:?}").contains("sk-secret"));
        assert_eq!(released.names(), ["OPENAI_API_KEY"]);
    }
}
