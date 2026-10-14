mod adapters_support;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use adapters_support::TempDir;
use lade_system::supervision::credentials::{CredentialError, CredentialSource, provider_env_vars};

fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect()
}

fn private_file(tmp: &TempDir, text: &str, mode: u32) -> std::path::PathBuf {
    let path = tmp.path().join("credentials.env");
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn only_selected_provider_variables_are_released() {
    let source = CredentialSource::new(
        env(&[
            ("ANTHROPIC_API_KEY", "sk-ant-env"),
            ("OPENAI_API_KEY", "sk-openai"),
            ("PATH", "/usr/bin"),
            ("AWS_SECRET_ACCESS_KEY", "aws"),
        ]),
        None,
    );
    let released = source.resolve("anthropic").unwrap();
    assert_eq!(released.names(), ["ANTHROPIC_API_KEY"]);
    assert_eq!(released.vars[0].1.expose(), "sk-ant-env");
    assert!(source.resolve("lade-fixture").unwrap().vars.is_empty());
    let debug = format!("{source:?} {released:?}");
    assert!(
        !debug.contains("sk-ant-env") && !debug.contains("sk-openai"),
        "{debug}"
    );
    assert!(
        !debug.contains("aws"),
        "unknown variables are not even retained"
    );
    match source.resolve("groq") {
        Err(CredentialError::Missing { vars, .. }) => assert_eq!(vars, ["GROQ_API_KEY"]),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        source.resolve("amazon-bedrock"),
        Err(CredentialError::UnknownProvider(_))
    ));
    assert_eq!(provider_env_vars("google"), Some(&["GEMINI_API_KEY"][..]));
}

#[test]
fn private_file_supplies_missing_variables_and_env_wins() {
    let tmp = TempDir::new("cred-file");
    let path = private_file(
        &tmp,
        "# LADE credentials\nexport OPENAI_API_KEY='sk-file'\nGROQ_API_KEY=gsk-file\nMISTRAL_API_KEY=\n",
        0o600,
    );
    let source = CredentialSource::new(env(&[("GROQ_API_KEY", "gsk-env")]), Some(path));
    assert_eq!(
        source.resolve("openai").unwrap().vars[0].1.expose(),
        "sk-file"
    );
    assert_eq!(
        source.resolve("groq").unwrap().vars[0].1.expose(),
        "gsk-env"
    );
    assert!(matches!(
        source.resolve("mistral"),
        Err(CredentialError::Missing { .. })
    ));
    let absent = CredentialSource::new(Vec::new(), Some(tmp.path().join("absent.env")));
    assert!(matches!(
        absent.resolve("openai"),
        Err(CredentialError::Missing { .. })
    ));
}

#[test]
fn insecure_files_are_refused() {
    let tmp = TempDir::new("cred-insecure");
    let path = private_file(&tmp, "OPENAI_API_KEY=sk-file\n", 0o644);
    let source = CredentialSource::new(Vec::new(), Some(path.clone()));
    let error = source.resolve("openai").unwrap_err();
    assert!(
        matches!(error, CredentialError::InsecureFile { .. }),
        "{error:?}"
    );
    assert!(!error.to_string().contains("sk-file"));
    assert!(source.resolve("lade-fixture").is_ok());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let link = tmp.path().join("link.env");
    symlink(&path, &link).unwrap();
    assert!(matches!(
        CredentialSource::new(Vec::new(), Some(link)).resolve("openai"),
        Err(CredentialError::InsecureFile { .. })
    ));
    assert!(matches!(
        CredentialSource::new(Vec::new(), Some(tmp.path().to_path_buf())).resolve("openai"),
        Err(CredentialError::InsecureFile { .. })
    ));
    assert!(
        CredentialSource::new(Vec::new(), Some(path))
            .resolve("openai")
            .is_ok()
    );
}
