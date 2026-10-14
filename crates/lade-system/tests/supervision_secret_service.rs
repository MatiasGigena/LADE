mod adapters_support;

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::supervision::credentials::{CredentialError, CredentialSource};
use lade_system::supervision::secret_service::SecretService;

fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect()
}

fn private_file(tmp: &TempDir, text: &str) -> PathBuf {
    let path = tmp.path().join("credentials.env");
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

struct FakeSecretTool {
    tmp: TempDir,
}

impl FakeSecretTool {
    fn new(tag: &str) -> Self {
        let tmp = TempDir::new(tag);
        let program = tmp.path().join("secret-tool");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-secret-tool"),
            &program,
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(tmp.path().join("store")).unwrap();
        Self { tmp }
    }

    fn dir(&self) -> &Path {
        self.tmp.path()
    }

    fn service(&self) -> SecretService {
        SecretService::new(
            self.dir().join("secret-tool"),
            env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")]),
        )
    }

    fn item(&self, provider: &str, variable: &str, value: &str) {
        fs::write(
            self.dir().join(format!("store/{provider}.{variable}")),
            value,
        )
        .unwrap();
    }

    fn mode(&self, mode: &str) {
        fs::write(self.dir().join("mode"), mode).unwrap();
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn secret_service_ranks_between_the_environment_and_the_file() {
    let fake = FakeSecretTool::new("ss-order");
    fake.item("openai", "OPENAI_API_KEY", "sk-keyring");
    fake.item("groq", "GROQ_API_KEY", "gsk-keyring");
    fake.item("anthropic", "ANTHROPIC_OAUTH_TOKEN", "oat-keyring");
    let tmp = TempDir::new("ss-order-file");
    let file = private_file(&tmp, "OPENAI_API_KEY=sk-file\nMISTRAL_API_KEY=m-file\n");
    let source = CredentialSource::new(env(&[("GROQ_API_KEY", "gsk-env")]), Some(file))
        .with_secret_service(fake.service());

    let openai = source.resolve("openai").unwrap();
    assert_eq!(openai.vars[0].1.expose(), "sk-keyring");
    assert_eq!(
        source.resolve("groq").unwrap().vars[0].1.expose(),
        "gsk-env"
    );
    assert!(!fake.calls().iter().any(|c| c.contains("groq")));
    assert_eq!(
        source.resolve("mistral").unwrap().vars[0].1.expose(),
        "m-file"
    );
    assert_eq!(
        source.resolve("anthropic").unwrap().names(),
        ["ANTHROPIC_OAUTH_TOKEN"]
    );
    let calls = fake.calls();
    assert!(
        calls.contains(&"lookup service lade provider openai variable OPENAI_API_KEY".to_string()),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|c| !c.contains("keyring") && !c.contains("-file")),
        "{calls:?}"
    );
    let seen = fs::read_to_string(fake.dir().join("env")).unwrap();
    assert!(
        seen.contains("DBUS_SESSION_BUS_ADDRESS") && seen.contains("LC_ALL"),
        "{seen}"
    );
    assert!(!seen.contains("_API_KEY"), "{seen}");
    assert!(!format!("{source:?}{openai:?}").contains("sk-keyring"));

    let before = fake.calls().len();
    source.check("openai").unwrap();
    source.check("groq").unwrap();
    assert!(!source.is_available("anthropic"));
    assert_eq!(fake.calls().len(), before);
    source.check("anthropic").unwrap();
    assert!(fake.calls().len() > before);
    assert!(matches!(
        source.check("xai"),
        Err(CredentialError::Missing {
            secret_service: None,
            ..
        })
    ));
}

#[test]
fn custom_providers_read_their_variables_from_the_secret_service() {
    let fake = FakeSecretTool::new("ss-custom");
    fake.item("local-llm", "LADE_PROVIDER_LOCAL_KEY", "local-keyring");
    let source = CredentialSource::new(Vec::new(), None).with_secret_service(fake.service());
    let vars = vec!["LADE_PROVIDER_LOCAL_KEY".to_string()];
    let released = source.resolve_custom("local-llm", &vars).unwrap();
    assert_eq!(released.vars[0].1.expose(), "local-keyring");
    source.check_custom("local-llm", &vars).unwrap();
    assert!(!source.is_available_custom(&vars));
    let two = vec![
        "LADE_PROVIDER_LOCAL_KEY".to_string(),
        "LADE_PROVIDER_OTHER".to_string(),
    ];
    match source.resolve_custom("local-llm", &two) {
        Err(CredentialError::MissingCustom { vars, .. }) => {
            assert_eq!(vars, ["LADE_PROVIDER_OTHER"]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_secret_service_that_never_answers_is_bounded_and_not_fatal() {
    let fake = FakeSecretTool::new("ss-timeout");
    fake.mode("timeout");
    let tmp = TempDir::new("ss-timeout-file");
    let file = private_file(&tmp, "OPENAI_API_KEY=sk-file\n");
    let service = fake.service().with_timeout(Duration::from_millis(300));
    let started = Instant::now();
    let with_file =
        CredentialSource::new(Vec::new(), Some(file)).with_secret_service(service.clone());
    assert_eq!(
        with_file.resolve("openai").unwrap().vars[0].1.expose(),
        "sk-file"
    );
    let without = CredentialSource::new(Vec::new(), None).with_secret_service(service);
    let error = without.resolve("anthropic").unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| c.contains("anthropic"))
            .count(),
        1
    );
    let message = error.to_string();
    assert!(message.contains("did not answer"), "{message}");
    assert!(
        message.contains("laded credentials set anthropic"),
        "{message}"
    );
}

#[test]
fn an_unavailable_or_locked_secret_service_is_explained_and_the_file_still_applies() {
    let fake = FakeSecretTool::new("ss-unavailable");
    fake.mode("unavailable");
    let tmp = TempDir::new("ss-unavailable-file");
    let file = private_file(&tmp, "GROQ_API_KEY=gsk-file\n");
    let source = CredentialSource::new(Vec::new(), Some(file)).with_secret_service(fake.service());
    assert_eq!(
        source.resolve("groq").unwrap().vars[0].1.expose(),
        "gsk-file"
    );
    let message = source.resolve("openai").unwrap_err().to_string();
    assert!(
        message.contains("Secret Service unavailable: The name org.freedesktop.secrets"),
        "{message}"
    );

    fake.mode("locked");
    let message = source.resolve("openai").unwrap_err().to_string();
    assert!(message.contains("locked"), "{message}");
    assert!(fake.calls().iter().any(|c| c.starts_with("search ")));

    let missing = CredentialSource::new(Vec::new(), None)
        .with_secret_service(SecretService::detect(None, |_| None));
    let message = missing.resolve("openai").unwrap_err().to_string();
    assert!(message.contains("libsecret-tools"), "{message}");
    let no_bus = CredentialSource::new(Vec::new(), None).with_secret_service(
        SecretService::detect(Some(fake.dir().join("secret-tool")), |_| None),
    );
    let calls = fake.calls().len();
    let message = no_bus.resolve("openai").unwrap_err().to_string();
    assert!(message.contains("D-Bus"), "{message}");
    assert_eq!(fake.calls().len(), calls);
}

fn laded_credentials(fake: &FakeSecretTool, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_laded"))
        .arg("credentials")
        .args(args)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", fake.dir().display()))
        .env("HOME", fake.dir())
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

#[test]
fn laded_credentials_stores_and_clears_through_secret_tool_stdin() {
    let fake = FakeSecretTool::new("ss-cli");
    let stored = laded_credentials(&fake, &["set", "openai"], b"sk-cli-value\n");
    assert!(stored.status.success(), "{stored:?}");
    assert_eq!(
        fs::read_to_string(fake.dir().join("store/openai.OPENAI_API_KEY")).unwrap(),
        "sk-cli-value"
    );
    assert_eq!(
        fake.calls(),
        [
            "store --label LADE openai OPENAI_API_KEY service lade provider openai variable \
             OPENAI_API_KEY"
        ]
    );
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&stored.stdout),
        String::from_utf8_lossy(&stored.stderr)
    );
    assert!(!printed.contains("sk-cli-value"), "{printed}");
    assert!(!fake.dir().join(".local/state").exists());

    let token = laded_credentials(
        &fake,
        &["set", "anthropic", "--variable", "ANTHROPIC_OAUTH_TOKEN"],
        b"oat-value",
    );
    assert!(token.status.success(), "{token:?}");
    assert!(
        fake.dir()
            .join("store/anthropic.ANTHROPIC_OAUTH_TOKEN")
            .is_file()
    );
    let custom = laded_credentials(
        &fake,
        &["set", "local-llm", "--variable", "LADE_PROVIDER_LOCAL"],
        b"k",
    );
    assert!(custom.status.success(), "{custom:?}");

    for (args, stdin, code) in [
        (
            &["set", "openai", "--variable", "GROQ_API_KEY"][..],
            &b"x"[..],
            64,
        ),
        (&["set", "local-llm"][..], &b"x"[..], 64),
        (&["set", "lade-fixture"][..], &b"x"[..], 64),
        (&["set", "bad id"][..], &b"x"[..], 64),
        (&["set", "groq"][..], &b"\n"[..], 65),
        (&["set", "groq"][..], &b"two\nlines\n"[..], 65),
        (&["rotate", "groq"][..], &b""[..], 64),
    ] {
        let out = laded_credentials(&fake, args, stdin);
        assert_eq!(out.status.code(), Some(code), "{args:?}: {out:?}");
    }
    assert!(!fake.dir().join("store/groq.GROQ_API_KEY").exists());

    let cleared = laded_credentials(&fake, &["clear", "anthropic"], b"");
    assert!(cleared.status.success(), "{cleared:?}");
    assert!(
        !fake
            .dir()
            .join("store/anthropic.ANTHROPIC_OAUTH_TOKEN")
            .exists()
    );
    assert!(fake.dir().join("store/openai.OPENAI_API_KEY").exists());
    let again = laded_credentials(&fake, &["clear", "anthropic"], b"");
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout).contains("no LADE Secret Service item"));
}

#[test]
fn laded_credentials_without_a_secret_service_points_to_the_credentials_file() {
    let fake = FakeSecretTool::new("ss-cli-unavailable");
    for mode in ["unavailable", "locked"] {
        fake.mode(mode);
        let out = laded_credentials(&fake, &["set", "openai"], b"sk-never-stored");
        assert_eq!(out.status.code(), Some(69), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("Secret Service unavailable"), "{stderr}");
        assert!(stderr.contains("credentials.env"), "{stderr}");
        assert!(!stderr.contains("sk-never-stored"));
    }
    let calls = fake.calls().len();
    let out = Command::new(env!("CARGO_BIN_EXE_laded"))
        .args(["credentials", "set", "openai"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", fake.dir().display()))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(69), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("D-Bus"));
    assert_eq!(fake.calls().len(), calls);
    assert!(
        fs::read_dir(fake.dir().join("store"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!fake.dir().join(".config").exists());
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

struct Keyring {
    tmp: TempDir,
    bus: Child,
    address: String,
}

const KEYRING_PASSWORD: &[u8] = b"lade-test-password";

impl Keyring {
    fn start() -> Result<Self, String> {
        for tool in [
            "secret-tool",
            "gnome-keyring-daemon",
            "dbus-daemon",
            "dbus-send",
        ] {
            if on_path(tool).is_none() {
                return Err(format!("{tool} not installed"));
            }
        }
        let tmp = TempDir::new("ss-real");
        let runtime = tmp.path().join("run");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let mut bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .env_clear()
            .envs(base_env(tmp.path()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("dbus-daemon: {e}"))?;
        let mut address = String::new();
        let read = BufReader::new(bus.stdout.take().unwrap()).read_line(&mut address);
        let keyring = Self {
            tmp,
            bus,
            address: address.trim().to_owned(),
        };
        if read.is_err() || keyring.address.is_empty() {
            return Err("dbus-daemon printed no address".into());
        }
        if !keyring.unlock() {
            return Err("gnome-keyring-daemon --unlock failed".into());
        }
        Ok(keyring)
    }

    fn unlock(&self) -> bool {
        let Ok(mut unlock) = Command::new("gnome-keyring-daemon")
            .args(["--replace", "--unlock", "--components=secrets"])
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let _ = unlock.stdin.take().unwrap().write_all(KEYRING_PASSWORD);
        unlock.wait().is_ok_and(|s| s.success())
    }

    fn env(&self) -> Vec<(OsString, OsString)> {
        let mut env = base_env(self.tmp.path());
        env.push(("DBUS_SESSION_BUS_ADDRESS".into(), (&self.address).into()));
        env
    }

    fn service(&self) -> SecretService {
        SecretService::new(on_path("secret-tool").unwrap(), self.env())
    }

    fn laded(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_laded"))
            .arg("credentials")
            .args(args)
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().unwrap()
    }
}

fn base_env(home: &Path) -> Vec<(OsString, OsString)> {
    vec![
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("HOME".into(), home.into()),
        ("XDG_DATA_HOME".into(), home.join("data").into()),
        ("XDG_CONFIG_HOME".into(), home.join("config").into()),
        ("XDG_RUNTIME_DIR".into(), home.join("run").into()),
    ]
}

impl Drop for Keyring {
    fn drop(&mut self) {
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

#[test]
fn real_secret_service_round_trip_lock_and_clear() {
    let keyring = match Keyring::start() {
        Ok(keyring) => keyring,
        Err(reason) => {
            eprintln!("skipping real Secret Service test: {reason}");
            return;
        }
    };
    let stored = keyring.laded(&["set", "openai"], b"sk-real-keyring\n");
    assert!(stored.status.success(), "{stored:?}");

    let source = CredentialSource::new(Vec::new(), None).with_secret_service(keyring.service());
    let released = source.resolve("openai").unwrap();
    assert_eq!(released.vars[0].1.expose(), "sk-real-keyring");
    assert!(matches!(
        source.resolve("groq"),
        Err(CredentialError::Missing {
            secret_service: None,
            ..
        })
    ));

    let locked = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.secrets",
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service.Lock",
            "array:objpath:/org/freedesktop/secrets/collection/login",
        ])
        .env_clear()
        .envs(keyring.env())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(locked.success());
    let message = source.resolve("openai").unwrap_err().to_string();
    assert!(message.contains("locked"), "{message}");
    assert!(!message.contains("sk-real-keyring"));
    let refused = keyring.laded(&["set", "groq"], b"gsk-never");
    assert_eq!(refused.status.code(), Some(69), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("locked"));

    assert!(keyring.unlock(), "unlock again");
    assert_eq!(
        source.resolve("openai").unwrap().vars[0].1.expose(),
        "sk-real-keyring"
    );
    let cleared = keyring.laded(&["clear", "openai"], b"");
    assert!(cleared.status.success(), "{cleared:?}");
    assert!(source.resolve("openai").is_err());
}
