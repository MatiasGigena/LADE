use std::path::{Path, PathBuf};
use std::time::Duration;

pub const FAULTS_ENV: &str = "LADE_TEST_FAULTS_DIR";

const MAX_DELAY: Duration = Duration::from_secs(10);

#[cfg(debug_assertions)]
pub fn point(name: &str) {
    use std::sync::OnceLock;
    static ARMED: OnceLock<Option<String>> = OnceLock::new();
    let armed = ARMED.get_or_init(|| std::env::var("LADE_FAULT_POINT").ok());
    if armed.as_deref() == Some(name) {
        eprintln!("laded: fault injection: aborting at {name}");
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn point(_name: &str) {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Refuse,
    Delay(Duration),
}

#[cfg(debug_assertions)]
pub fn directory() -> Option<&'static Path> {
    use std::sync::OnceLock;
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os(FAULTS_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    })
    .as_deref()
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn directory() -> Option<&'static Path> {
    None
}

#[must_use]
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

#[must_use]
pub fn operation(operation: &str) -> Option<Fault> {
    fault_in(directory()?, operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_faults_follow_the_files_in_the_directory() {
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
