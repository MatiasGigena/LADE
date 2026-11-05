use super::models::{endpoint, variable};
use super::*;
use crate::support::random_fill;

fn temp(tag: &str) -> PathBuf {
    let mut bytes = [0u8; 8];
    random_fill(&mut bytes).unwrap();
    let dir = std::env::temp_dir().join(format!("lade-profile-{tag}-{}", hex(&bytes)));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn extension_hashes_match_the_worker_algorithm() {
    let dir = temp("hash");
    let ext = dir.join("ext");
    fs::create_dir_all(ext.join("lib")).unwrap();
    fs::write(ext.join("index.js"), "export default function () {}\n").unwrap();
    fs::write(ext.join("lib/util.js"), "export const x = 1;\n").unwrap();
    assert_eq!(
        extension_hash(&ext.join("index.js")).unwrap(),
        "4554c00dbb28cad89d67afb301d893073eefca19bee50292764c7b8c3dc8bf03"
    );
    assert_eq!(
        extension_hash(&ext).unwrap(),
        "3649d03b8d53b790fab2b926092c3e3ff4dfac57fbf968ab8c3e8e27b767d73a"
    );
    std::os::unix::fs::symlink("index.js", ext.join("link.js")).unwrap();
    assert!(extension_hash(&ext).unwrap_err().contains("symbolic link"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn snapshots_contain_only_approved_unchanged_extensions() {
    let dir = temp("snapshot");
    let profile = dir.join("profile");
    fs::create_dir_all(profile.join("extensions")).unwrap();
    fs::create_dir_all(profile.join("skills/demo")).unwrap();
    fs::write(profile.join("AGENTS.md"), "context").unwrap();
    fs::write(profile.join("skills/demo/SKILL.md"), "skill").unwrap();
    fs::write(profile.join("extensions/a.js"), "a").unwrap();
    fs::write(profile.join("extensions/b.js"), "b").unwrap();
    fs::write(profile.join("extensions/notes.txt"), "ignored").unwrap();
    let a = sha256(b"a");
    let entries = scan(&profile);
    assert_eq!(
        entries.iter().filter(|e| e.kind == Kind::Extension).count(),
        2
    );
    let dest = dir.join("session/profile");
    let loaded = materialize(&profile, &dest, |name, sha| name == "a.js" && sha == a).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "a.js");
    assert!(dest.join("AGENTS.md").is_file());
    assert!(dest.join("skills/demo/SKILL.md").is_file());
    assert!(
        !dest.join("extensions/b.js").exists(),
        "unapproved code is never copied"
    );
    fs::write(profile.join("extensions/a.js"), "changed").unwrap();
    let loaded = materialize(&profile, &dest, |name, sha| name == "a.js" && sha == a).unwrap();
    assert!(loaded.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn variables_need_the_custom_prefix() {
    assert_eq!(variable("$LADE_PROVIDER_KEY"), Some("LADE_PROVIDER_KEY"));
    assert_eq!(variable("${LADE_PROVIDER_KEY}"), Some("LADE_PROVIDER_KEY"));
    assert_eq!(variable("$OPENAI_API_KEY"), None);
    assert_eq!(variable("$LADE_PROVIDER_key"), None);
}

#[test]
fn endpoints_parse_scheme_host_and_port() {
    assert_eq!(
        endpoint("https://api.example.com/v1").unwrap(),
        (true, "api.example.com".into(), 443)
    );
    assert_eq!(
        endpoint("http://localhost:11434/v1").unwrap(),
        (false, "localhost".into(), 11434)
    );
    assert_eq!(
        endpoint("http://[::1]:8080").unwrap(),
        (false, "::1".into(), 8080)
    );
    assert!(endpoint("ftp://x").is_err());
    assert!(endpoint("https://user@host").is_err());
    assert!(endpoint("https://host:0").is_err());
}

#[test]
fn custom_providers_follow_the_trust_rules() {
    let ok = br#"{"providers":{"local":{"baseUrl":"http://127.0.0.1:11434/v1","apiKey":"ollama","api":"openai-completions","models":[{"id":"m"}]},
        "proxy":{"baseUrl":"https://llm.example.com/v1","apiKey":"$LADE_PROVIDER_PROXY","headers":{"x-team":"lade"}}}}"#;
    let providers = custom_providers(ok).unwrap();
    assert_eq!(providers.len(), 2);
    assert!(providers[0].local);
    assert_eq!(providers[1].vars, ["LADE_PROVIDER_PROXY"]);
    assert_eq!(providers[1].port, 443);
    for (bad, why) in [
        (
            r#"{"providers":{"p":{"baseUrl":"https://h","apiKey":"!op read x"}}}"#,
            "shell",
        ),
        (
            r#"{"providers":{"p":{"baseUrl":"https://h","apiKey":"sk-literal"}}}"#,
            "credentials.env",
        ),
        (
            r#"{"providers":{"p":{"baseUrl":"https://h","apiKey":"$OPENAI_API_KEY"}}}"#,
            "variable",
        ),
        (
            r#"{"providers":{"p":{"baseUrl":"http://public.example"}}}"#,
            "plain HTTP",
        ),
        (
            r#"{"providers":{"anthropic":{"baseUrl":"https://proxy"}}}"#,
            "built-in",
        ),
        (
            r#"{"providers":{"p":{"baseUrl":"https://h","oauth":"radius"}}}"#,
            "OAuth",
        ),
        (
            r#"{"providers":{"p":{"baseUrl":"https://h","headers":{"k":"!cmd"}}}}"#,
            "shell",
        ),
    ] {
        let error = custom_providers(bad.as_bytes()).unwrap_err();
        assert!(error.contains(why), "{bad}: {error}");
    }
}
