use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppAction {
    NextRegion,
    PreviousRegion,
    ShowWorkbench,
    ShowActivity,
    FocusSearch,
    AddFeedback,
    NewObjective,
    OpenWorkspace,
    ToggleViewerMode,
    SendMessage,
    ShowShortcuts,
    ShowTerminal,
    LeaveTerminal,
    TerminalCopy,
    TerminalPaste,
    Exit,
    CloseDialog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    pub action: AppAction,
    pub id: &'static str,
    pub default: &'static str,
    pub description: &'static str,
    pub fixed: Option<&'static str>,
}

const REGION_ESCAPE: &str = "Fixed: the region escape route always works.";

pub const DEFAULT_APP_KEYBINDINGS: &[KeyBinding] = &[
    KeyBinding {
        action: AppAction::NextRegion,
        id: "next-region",
        default: "F6",
        description: "Move to the next region",
        fixed: Some(REGION_ESCAPE),
    },
    KeyBinding {
        action: AppAction::PreviousRegion,
        id: "previous-region",
        default: "<Shift>F6",
        description: "Move to the previous region",
        fixed: Some(REGION_ESCAPE),
    },
    KeyBinding {
        action: AppAction::ShowWorkbench,
        id: "show-workbench",
        default: "<Control>1",
        description: "Show the Workbench",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::ShowActivity,
        id: "show-activity",
        default: "<Control>2",
        description: "Show Activity",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::FocusSearch,
        id: "focus-search",
        default: "<Control>k",
        description: "Search",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::AddFeedback,
        id: "add-feedback",
        default: "<Control><Shift>m",
        description: "Add feedback on the selection",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::NewObjective,
        id: "new-objective",
        default: "<Control>n",
        description: "New Objective",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::OpenWorkspace,
        id: "open-workspace",
        default: "<Control>o",
        description: "Open a Git repository",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::ToggleViewerMode,
        id: "toggle-viewer-mode",
        default: "<Control><Shift>f",
        description: "Switch between diff and full file",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::SendMessage,
        id: "send-message",
        default: "<Control>Return",
        description: "Send the composed message (in the message field)",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::ShowShortcuts,
        id: "show-shortcuts",
        default: "<Control>question",
        description: "Show and change keyboard shortcuts",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::ShowTerminal,
        id: "show-terminal",
        default: "<Control>3",
        description: "Show the Objective terminal and focus it",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::LeaveTerminal,
        id: "leave-terminal",
        default: "<Control><Shift>e",
        description: "Leave the terminal (focus returns to where it was)",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::TerminalCopy,
        id: "terminal-copy",
        default: "<Control><Shift>c",
        description: "Copy the terminal selection (in the terminal)",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::TerminalPaste,
        id: "terminal-paste",
        default: "<Control><Shift>v",
        description: "Paste into the terminal (in the terminal)",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::Exit,
        id: "exit",
        default: "<Control>q",
        description: "Exit: stop the service, its agents and terminals",
        fixed: None,
    },
    KeyBinding {
        action: AppAction::CloseDialog,
        id: "close-dialog",
        default: "Escape",
        description: "Close the dialog",
        fixed: Some("Fixed: Escape always closes the dialog."),
    },
];

pub fn reaches_window_from_terminal(action: AppAction) -> bool {
    matches!(
        action,
        AppAction::NextRegion
            | AppAction::PreviousRegion
            | AppAction::LeaveTerminal
            | AppAction::TerminalCopy
            | AppAction::TerminalPaste
            | AppAction::Exit
    )
}

pub fn binding(action: AppAction) -> &'static KeyBinding {
    DEFAULT_APP_KEYBINDINGS
        .iter()
        .find(|b| b.action == action)
        .unwrap_or(&DEFAULT_APP_KEYBINDINGS[0])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accel {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub other_modifier: bool,
    pub key: String,
}

impl Accel {
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || {
            format!(
                "‘{text}’ is not a valid shortcut. Use GTK syntax, for example F8 or <Control>F8."
            )
        };
        let mut rest = text.trim();
        let mut accel = Self {
            shift: false,
            control: false,
            alt: false,
            other_modifier: false,
            key: String::new(),
        };
        while let Some(inner) = rest.strip_prefix('<') {
            let (name, after) = inner.split_once('>').ok_or_else(invalid)?;
            match name.to_ascii_lowercase().as_str() {
                "shift" => accel.shift = true,
                "control" | "ctrl" | "ctl" | "primary" => accel.control = true,
                "alt" | "mod1" => accel.alt = true,
                "super" | "hyper" | "meta" | "release" => accel.other_modifier = true,
                _ => return Err(invalid()),
            }
            rest = after;
        }
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(invalid());
        }
        accel.key = canonical_key(rest);
        Ok(accel)
    }

    pub fn function_key(&self) -> Option<u8> {
        let digits = self.key.strip_prefix('F')?;
        if digits.starts_with('0') {
            return None;
        }
        digits.parse::<u8>().ok().filter(|n| (1..=35).contains(n))
    }

    pub fn to_gtk(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.shift, "<Shift>"),
            (self.control, "<Control>"),
            (self.alt, "<Alt>"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&self.key);
        out
    }

    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (on, name) in [
            (self.control, "Ctrl"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
        ] {
            if on {
                parts.push(name.to_owned());
            }
        }
        parts.push(match self.key.as_str() {
            "Return" => "Enter".to_owned(),
            "question" => "?".to_owned(),
            "Escape" => "Esc".to_owned(),
            key if key.chars().count() == 1 => key.to_ascii_uppercase(),
            key => key.to_owned(),
        });
        parts.join("+")
    }

    fn same_combination(&self, other: &Self) -> bool {
        self.shift == other.shift
            && self.control == other.control
            && self.alt == other.alt
            && self.other_modifier == other.other_modifier
            && self.key.eq_ignore_ascii_case(&other.key)
    }
}

fn canonical_key(key: &str) -> String {
    if key.chars().count() == 1 {
        return key.to_ascii_lowercase();
    }
    match key.strip_prefix(['f', 'F']) {
        Some(digits) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            format!("F{digits}")
        }
        _ => key.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    bindings: Vec<(AppAction, Accel)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: DEFAULT_APP_KEYBINDINGS
                .iter()
                .map(|b| (b.action, default_accel(b)))
                .collect(),
        }
    }
}

fn default_accel(binding: &KeyBinding) -> Accel {
    Accel::parse(binding.default).unwrap_or(Accel {
        shift: false,
        control: false,
        alt: false,
        other_modifier: false,
        key: binding.default.to_owned(),
    })
}

impl Keymap {
    pub fn accel(&self, action: AppAction) -> Accel {
        self.bindings
            .iter()
            .find(|(a, _)| *a == action)
            .map_or_else(
                || default_accel(binding(action)),
                |(_, accel)| accel.clone(),
            )
    }

    pub fn gtk(&self, action: AppAction) -> String {
        self.accel(action).to_gtk()
    }

    pub fn label(&self, action: AppAction) -> String {
        self.accel(action).label()
    }

    #[cfg(test)]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn propose(entries: &[(AppAction, String)]) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let mut parsed: Vec<(AppAction, Accel)> = Vec::new();
        for binding in DEFAULT_APP_KEYBINDINGS {
            let mut matching = entries.iter().filter(|(a, _)| *a == binding.action);
            let (Some((_, text)), None) = (matching.next(), matching.next()) else {
                errors.push(format!(
                    "{}: exactly one shortcut is required.",
                    binding.description
                ));
                continue;
            };
            let accel = match Accel::parse(text) {
                Ok(accel) => accel,
                Err(message) => {
                    errors.push(format!("{}: {message}", binding.description));
                    continue;
                }
            };
            let default = default_accel(binding);
            let own_default = accel.same_combination(&default);
            if let Some(reason) = binding.fixed {
                if !own_default {
                    errors.push(format!(
                        "{}: {reason} It stays {}.",
                        binding.description,
                        default.label()
                    ));
                }
            } else if !own_default && !(accel.function_key().is_some() && !accel.other_modifier) {
                errors.push(format!(
                    "{}: {} is reserved for native typing, editing, selection, clipboard or undo. Keep {} or use a function key F1–F35 with optional Shift, Control or Alt.",
                    binding.description,
                    accel.label(),
                    default.label()
                ));
            }
            if let Some((other, _)) = parsed.iter().find(|(_, a)| a.same_combination(&accel)) {
                errors.push(format!(
                    "{} is assigned to both “{}” and “{}”. Each action needs its own combination.",
                    accel.label(),
                    self::binding(*other).description,
                    binding.description
                ));
            }
            parsed.push((binding.action, accel));
        }
        if errors.is_empty() {
            Ok(Self { bindings: parsed })
        } else {
            Err(errors)
        }
    }

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

pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(keymap: &Keymap) -> Vec<(AppAction, String)> {
        DEFAULT_APP_KEYBINDINGS
            .iter()
            .map(|b| (b.action, keymap.gtk(b.action)))
            .collect()
    }

    fn with(changes: &[(AppAction, &str)]) -> Vec<(AppAction, String)> {
        let mut list = entries(&Keymap::default());
        for (action, text) in changes {
            if let Some(entry) = list.iter_mut().find(|(a, _)| a == action) {
                entry.1 = (*text).to_owned();
            }
        }
        list
    }

    #[test]
    fn defaults_are_valid_and_keep_region_cycling() {
        for b in DEFAULT_APP_KEYBINDINGS {
            let accel = Accel::parse(b.default).expect("default parses");
            assert_eq!(Accel::parse(&accel.to_gtk()), Ok(accel), "{b:?}");
        }
        let keymap = Keymap::propose(&entries(&Keymap::default())).expect("defaults valid");
        assert!(keymap.is_default());
        assert_eq!(keymap.gtk(AppAction::NextRegion), "F6");
        assert_eq!(keymap.gtk(AppAction::PreviousRegion), "<Shift>F6");
        assert_eq!(keymap.label(AppAction::AddFeedback), "Ctrl+Shift+M");
        assert_eq!(keymap.label(AppAction::ShowShortcuts), "Ctrl+?");
        assert_eq!(keymap.label(AppAction::SendMessage), "Ctrl+Enter");
        assert_eq!(keymap.label(AppAction::ShowTerminal), "Ctrl+3");
        assert_eq!(keymap.label(AppAction::LeaveTerminal), "Ctrl+Shift+E");
        assert_eq!(keymap.label(AppAction::Exit), "Ctrl+Q");
    }

    #[test]
    fn default_table_is_stable_and_remappable() {
        let table: Vec<(&str, &str)> = DEFAULT_APP_KEYBINDINGS
            .iter()
            .map(|b| (b.id, b.default))
            .collect();
        assert_eq!(
            table,
            [
                ("next-region", "F6"),
                ("previous-region", "<Shift>F6"),
                ("show-workbench", "<Control>1"),
                ("show-activity", "<Control>2"),
                ("focus-search", "<Control>k"),
                ("add-feedback", "<Control><Shift>m"),
                ("new-objective", "<Control>n"),
                ("open-workspace", "<Control>o"),
                ("toggle-viewer-mode", "<Control><Shift>f"),
                ("send-message", "<Control>Return"),
                ("show-shortcuts", "<Control>question"),
                ("show-terminal", "<Control>3"),
                ("leave-terminal", "<Control><Shift>e"),
                ("terminal-copy", "<Control><Shift>c"),
                ("terminal-paste", "<Control><Shift>v"),
                ("exit", "<Control>q"),
                ("close-dialog", "Escape"),
            ]
        );
        let remappable: Vec<&KeyBinding> = DEFAULT_APP_KEYBINDINGS
            .iter()
            .filter(|b| b.fixed.is_none())
            .collect();
        let changes: Vec<(AppAction, String)> = remappable
            .iter()
            .zip(1..)
            .map(|(b, n)| (b.action, format!("<Alt>F{n}")))
            .collect();
        let refs: Vec<(AppAction, &str)> = changes.iter().map(|(a, s)| (*a, s.as_str())).collect();
        let keymap = Keymap::propose(&with(&refs)).expect("every action remaps to a function key");
        for (action, accel) in &changes {
            assert_eq!(&keymap.gtk(*action), accel);
        }
    }

    #[test]
    fn terminal_escape_routes_are_configurable_and_reach_the_window() {
        let keymap = Keymap::propose(&with(&[
            (AppAction::ShowTerminal, "F7"),
            (AppAction::LeaveTerminal, "<Shift>F7"),
        ]))
        .expect("valid");
        assert_eq!(keymap.gtk(AppAction::ShowTerminal), "F7");
        assert_eq!(keymap.gtk(AppAction::LeaveTerminal), "<Shift>F7");
        let text = keymap.to_config();
        assert!(text.contains("\"leave-terminal\": \"<Shift>F7\""), "{text}");
        assert_eq!(Keymap::from_config(&text), Ok(keymap));
        assert!(Keymap::propose(&with(&[(AppAction::LeaveTerminal, "<Control>c")])).is_err());
        for action in [
            AppAction::NextRegion,
            AppAction::PreviousRegion,
            AppAction::LeaveTerminal,
        ] {
            assert!(reaches_window_from_terminal(action), "{action:?}");
        }
        for action in [
            AppAction::NewObjective,
            AppAction::FocusSearch,
            AppAction::AddFeedback,
            AppAction::SendMessage,
            AppAction::ShowTerminal,
        ] {
            assert!(!reaches_window_from_terminal(action), "{action:?}");
        }
    }

    #[test]
    fn syntax_is_checked_and_normalised() {
        let accel = Accel::parse("<ctrl><SHIFT>f8").unwrap();
        assert_eq!(accel.to_gtk(), "<Shift><Control>F8");
        assert_eq!(accel.function_key(), Some(8));
        assert_eq!(Accel::parse("<Mod1>F35").unwrap().to_gtk(), "<Alt>F35");
        for bad in [
            "",
            "<Control>",
            "<Control",
            "<Bogus>F8",
            "F8 F9",
            "Ctrl+F8",
            "<Control>-",
        ] {
            assert!(Accel::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(Accel::parse("F36").unwrap().function_key(), None);
        assert_eq!(Accel::parse("F0").unwrap().function_key(), None);
        assert_eq!(Accel::parse("F08").unwrap().function_key(), None);
    }

    #[test]
    fn remaps_accept_own_default_or_function_keys_only() {
        let keymap = Keymap::propose(&with(&[
            (AppAction::FocusSearch, "<Control><Alt>F8"),
            (AppAction::AddFeedback, "F9"),
            (AppAction::NewObjective, "<ctrl>N"),
        ]))
        .expect("valid");
        assert_eq!(keymap.gtk(AppAction::FocusSearch), "<Control><Alt>F8");
        assert_eq!(keymap.gtk(AppAction::AddFeedback), "F9");
        assert_eq!(keymap.gtk(AppAction::NewObjective), "<Control>n");
        for reserved in [
            "a",
            "<Control>a",
            "<Control>c",
            "<Control>z",
            "Return",
            "Tab",
            "<Super>F8",
        ] {
            let errors =
                Keymap::propose(&with(&[(AppAction::FocusSearch, reserved)])).expect_err(reserved);
            assert!(errors[0].contains("reserved"), "{errors:?}");
        }
        assert!(Keymap::propose(&with(&[(AppAction::FocusSearch, "<Control>n")])).is_err());
    }

    #[test]
    fn duplicates_name_the_combination_and_both_actions() {
        let errors = Keymap::propose(&with(&[
            (AppAction::FocusSearch, "<Control>F8"),
            (AppAction::NewObjective, "<ctrl>f8"),
        ]))
        .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("Ctrl+F8"), "{errors:?}");
        assert!(errors[0].contains("Search") && errors[0].contains("New Objective"));
        let errors = Keymap::propose(&with(&[(AppAction::FocusSearch, "F6")])).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("F6") && e.contains("both"))
        );
    }

    #[test]
    fn region_escape_and_dialog_close_cannot_be_remapped() {
        for (action, key) in [
            (AppAction::NextRegion, "F7"),
            (AppAction::PreviousRegion, "<Shift>F7"),
            (AppAction::CloseDialog, "F10"),
        ] {
            let errors = Keymap::propose(&with(&[(action, key)])).unwrap_err();
            assert!(errors[0].contains("Fixed"), "{errors:?}");
        }
    }

    #[test]
    fn proposals_must_be_complete_and_report_every_problem() {
        let mut list = with(&[
            (AppAction::FocusSearch, "x"),
            (AppAction::AddFeedback, "<Nope>"),
        ]);
        list.retain(|(a, _)| *a != AppAction::NewObjective);
        let errors = Keymap::propose(&list).unwrap_err();
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn config_round_trips_and_invalid_files_fall_back() {
        let keymap = Keymap::propose(&with(&[(AppAction::FocusSearch, "<Shift>F3")])).unwrap();
        let text = keymap.to_config();
        assert_eq!(Keymap::from_config(&text), Ok(keymap));
        let sparse =
            Keymap::from_config(r#"{"version":1,"bindings":{"add-feedback":"F9"}}"#).unwrap();
        assert_eq!(sparse.gtk(AppAction::AddFeedback), "F9");
        assert_eq!(sparse.gtk(AppAction::FocusSearch), "<Control>k");
        for bad in [
            "",
            "[]",
            "{\"version\":2,\"bindings\":{}}",
            "{\"version\":1}",
            "{\"version\":1,\"bindings\":{\"bogus\":\"F8\"}}",
            "{\"version\":1,\"bindings\":{\"focus-search\":8}}",
            "{\"version\":1,\"bindings\":{\"focus-search\":\"<Control>a\"}}",
            "{\"version\":1,\"bindings\":{\"next-region\":\"F7\"}}",
            "{\"version\":1,\"bindings\":{\"focus-search\":\"F8\",\"add-feedback\":\"F8\"}}",
            "{\"version\":1,\"bindings\":{},\"extra\":true}",
        ] {
            assert!(Keymap::from_config(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn load_and_save_use_the_file_or_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "lade-keys-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("lade").join("keybindings.json");
        assert_eq!(load(&path), (Keymap::default(), None), "missing file");
        let keymap = Keymap::propose(&with(&[(AppAction::ShowActivity, "F2")])).unwrap();
        save(&path, &keymap).unwrap();
        assert_eq!(load(&path), (keymap, None));
        std::fs::write(
            &path,
            "{\"version\":1,\"bindings\":{\"focus-search\":\"a\"}}",
        )
        .unwrap();
        let (fallback, message) = load(&path);
        assert!(fallback.is_default());
        assert!(message.unwrap().contains("ignored"));
        std::fs::write(&path, vec![b' '; 70 * 1024]).unwrap();
        assert!(load(&path).1.unwrap().contains("64 KiB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_path_follows_xdg() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            config_path(env(&[("XDG_CONFIG_HOME", "/c"), ("HOME", "/h")])),
            Some(PathBuf::from("/c/lade/keybindings.json"))
        );
        assert_eq!(
            config_path(env(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.config/lade/keybindings.json"))
        );
        assert_eq!(config_path(env(&[])), None);
    }
}
