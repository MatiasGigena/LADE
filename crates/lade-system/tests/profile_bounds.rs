#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod adapters_support;

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Command;

use adapters_support::TempDir;
use lade_system::adapters::profile::{MAX_EXTENSION_BYTES, extension_hash};

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .unwrap()
        .split_ascii_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn oversized_directory_extensions_are_rejected_before_loading_their_contents() {
    const CHILD_PATH: &str = "LADE_PROFILE_BOUNDS_CHILD_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let before = peak_rss_kib();
        assert_eq!(
            extension_hash(&PathBuf::from(path)).unwrap_err(),
            "extension is too large"
        );
        let growth = peak_rss_kib().saturating_sub(before) * 1024;
        assert!(
            growth < MAX_EXTENSION_BYTES * 2,
            "extension scan allocated {growth} bytes"
        );
        return;
    }
    let tmp = TempDir::new("profile-memory-bound");
    let extension = tmp.path().join("extension");
    fs::create_dir(&extension).unwrap();
    File::create(extension.join("large.bin"))
        .unwrap()
        .set_len(MAX_EXTENSION_BYTES * 8)
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "oversized_directory_extensions_are_rejected_before_loading_their_contents",
            "--nocapture",
        ])
        .env(CHILD_PATH, &extension)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
