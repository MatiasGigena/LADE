use std::path::PathBuf;

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
    let sparse = Keymap::from_config(r#"{"version":1,"bindings":{"add-feedback":"F9"}}"#).unwrap();
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
