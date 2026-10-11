//! Test-only fault injection for native end-to-end evidence (`scripts/e2e-resilience`).
//!
//! **Not a product setting.** It is inert unless `laded` was started with the environment
//! variable [`FAULTS_ENV`] naming an absolute directory; the service then logs a warning at start.
//! While that directory holds a file named after an operation, that operation misbehaves:
//!
//! | File | Content | Effect |
//! | --- | --- | --- |
//! | `draft.save` | empty (or anything else) | `draft.save` is refused with `durability_failure` before anything is written |
//! | `draft.save` | `delay <ms>` | `draft.save` waits `<ms>` (≤ 10 s) on the service thread, then proceeds normally |
//!
//! The file is read at every such request, so a test can create and remove it while the service
//! runs. Nothing else reads the variable.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// Environment variable naming the fault directory (test-only).
pub const FAULTS_ENV: &str = "LADE_TEST_FAULTS_DIR";

const MAX_DELAY: Duration = Duration::from_secs(10);

/// What an injected fault asks the operation to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Refuse,
    Delay(Duration),
}

/// The fault directory from the environment, read once.
pub fn directory() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os(FAULTS_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    })
    .as_deref()
}

/// The fault currently injected for `operation` under `dir`, if any.
pub fn fault_in(dir: &Path, operation: &str) -> Option<Fault> {
    let text = std::fs::read_to_string(dir.join(operation)).ok()?;
    let delay = text
        .trim()
        .strip_prefix("delay ")
        .and_then(|ms| ms.trim().parse::<u64>().ok());
    Some(match delay {
        Some(ms) => Fault::Delay(Duration::from_millis(ms).min(MAX_DELAY)),
        None => Fault::Refuse,
    })
}

/// The fault injected for `operation`, when fault injection is enabled.
pub fn fault(operation: &str) -> Option<Fault> {
    fault_in(directory()?, operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_follow_the_files_in_the_directory() {
        let dir = std::env::temp_dir().join(format!("lade-faults-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), None);
        std::fs::write(dir.join("draft.save"), "").unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), Some(Fault::Refuse));
        std::fs::write(dir.join("draft.save"), "delay 250\n").unwrap();
        assert_eq!(
            fault_in(&dir, "draft.save"),
            Some(Fault::Delay(Duration::from_millis(250)))
        );
        std::fs::write(dir.join("draft.save"), "delay 999999").unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), Some(Fault::Delay(MAX_DELAY)));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), None);
    }
}
