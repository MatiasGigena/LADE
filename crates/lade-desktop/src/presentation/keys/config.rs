//! The keyboard shortcuts file: where it lives, its JSON form, and loading and saving it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::{AppAction, DEFAULT_APP_KEYBINDINGS, Keymap};

impl Keymap {
    pub fn to_config(&self) -> String {
        let mut out = String::from("{\n  \"version\": 1,\n  \"bindings\": {\n");
        let lines: Vec<String> = DEFAULT_APP_KEYBINDINGS
            .iter()
            .filter(|b| b.fixed.is_none())
            .map(|b| format!("    \"{}\": \"{}\"", b.id, self.gtk(b.action)))
            .collect();
        out.push_str(&lines.join(",\n"));
        out.push_str("\n  }\n}\n");
        out
    }

    pub fn from_config(text: &str) -> Result<Self, String> {
        let value = lade_protocol::bounded_json(text.as_bytes())
            .map_err(|e| format!("not valid JSON ({e})"))?;
        let object = value.as_object().ok_or("the top level is not an object")?;
        if let Some(key) = object
            .keys()
            .find(|k| !matches!(k.as_str(), "version" | "bindings"))
        {
            return Err(format!("unknown field “{key}”"));
        }
        if object.get("version").and_then(|v| v.as_u64()) != Some(1) {
            return Err("“version” must be 1".to_owned());
        }
        let bindings = object
            .get("bindings")
            .and_then(|b| b.as_object())
            .ok_or("“bindings” must be an object")?;
        let mut entries: Vec<(AppAction, String)> = DEFAULT_APP_KEYBINDINGS
            .iter()
            .map(|b| (b.action, b.default.to_owned()))
            .collect();
        for (id, accel) in bindings {
            let binding = DEFAULT_APP_KEYBINDINGS
                .iter()
                .find(|b| b.id == id)
                .ok_or_else(|| format!("unknown action “{id}”"))?;
            let accel = accel
                .as_str()
                .ok_or_else(|| format!("the shortcut for “{id}” is not a string"))?;
            if let Some(entry) = entries.iter_mut().find(|(a, _)| *a == binding.action) {
                accel.clone_into(&mut entry.1);
            }
        }
        Self::propose(&entries).map_err(|errors| errors.join(" "))
    }
}

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

pub fn config_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let absolute = |name: &str| env(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let base =
        absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|h| h.join(".config")))?;
    Some(base.join("lade").join("keybindings.json"))
}

pub fn load(path: &Path) -> (Keymap, Option<String>) {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Keymap::default(), None),
        Err(e) => return (Keymap::default(), Some(ignored(path, &e.to_string()))),
    };
    let mut text = String::new();
    let read = file
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())
        .and_then(|n| {
            if n as u64 > MAX_CONFIG_BYTES {
                Err("the file is larger than 64 KiB".to_owned())
            } else {
                Ok(())
            }
        });
    match read.and_then(|()| Keymap::from_config(&text)) {
        Ok(keymap) => (keymap, None),
        Err(reason) => (Keymap::default(), Some(ignored(path, &reason))),
    }
}

fn ignored(path: &Path, reason: &str) -> String {
    let mut message = String::new();
    let _ = write!(
        message,
        "Keyboard shortcuts file {} was ignored: {reason}. The default shortcuts apply.",
        path.display()
    );
    message
}

pub fn save(path: &Path, keymap: &Keymap) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or("the shortcuts file has no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let temp = dir.join(".keybindings.json.tmp");
    std::fs::write(&temp, keymap.to_config())
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("{}: {e}", path.display())
        })
}
