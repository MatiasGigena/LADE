//! The one keybinding table. Views install shortcuts from it; no view checks keys itself.
//! Accelerators use GTK's `gtk_accelerator_parse` syntax.

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
    CloseDialog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    pub action: AppAction,
    pub accelerators: &'static [&'static str],
    pub description: &'static str,
}

pub const DEFAULT_APP_KEYBINDINGS: &[KeyBinding] = &[
    KeyBinding {
        action: AppAction::NextRegion,
        accelerators: &["F6"],
        description: "Move to the next region",
    },
    KeyBinding {
        action: AppAction::PreviousRegion,
        accelerators: &["<Shift>F6"],
        description: "Move to the previous region",
    },
    KeyBinding {
        action: AppAction::ShowWorkbench,
        accelerators: &["<Control>1"],
        description: "Show the Workbench",
    },
    KeyBinding {
        action: AppAction::ShowActivity,
        accelerators: &["<Control>2"],
        description: "Show Activity",
    },
    KeyBinding {
        action: AppAction::FocusSearch,
        accelerators: &["<Control>k"],
        description: "Search",
    },
    KeyBinding {
        action: AppAction::AddFeedback,
        accelerators: &["<Control><Shift>m"],
        description: "Add feedback on the selection",
    },
    KeyBinding {
        action: AppAction::NewObjective,
        accelerators: &["<Control>n"],
        description: "New Objective",
    },
    KeyBinding {
        action: AppAction::OpenWorkspace,
        accelerators: &["<Control>o"],
        description: "Open a Git repository",
    },
    KeyBinding {
        action: AppAction::ToggleViewerMode,
        accelerators: &["<Control><Shift>f"],
        description: "Switch between diff and full file",
    },
    KeyBinding {
        action: AppAction::SendMessage,
        accelerators: &["<Control>Return"],
        description: "Send the composed message",
    },
    KeyBinding {
        action: AppAction::CloseDialog,
        accelerators: &["Escape"],
        description: "Close the dialog",
    },
];

pub fn accelerators(bindings: &[KeyBinding], action: AppAction) -> &'static [&'static str] {
    bindings
        .iter()
        .find(|b| b.action == action)
        .map_or(&[], |b| b.accelerators)
}

/// Pairs of actions bound to the same accelerator (case-insensitive comparison).
pub fn conflicts(bindings: &[KeyBinding]) -> Vec<(String, AppAction, AppAction)> {
    let mut seen: Vec<(String, AppAction)> = Vec::new();
    let mut out = Vec::new();
    for binding in bindings {
        for accel in binding.accelerators {
            let key = accel.to_ascii_lowercase();
            if let Some((_, other)) = seen.iter().find(|(k, _)| *k == key) {
                out.push((key.clone(), *other, binding.action));
            }
            seen.push((key, binding.action));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_no_conflicts_and_keep_region_cycling() {
        assert!(conflicts(DEFAULT_APP_KEYBINDINGS).is_empty());
        assert_eq!(
            accelerators(DEFAULT_APP_KEYBINDINGS, AppAction::NextRegion),
            ["F6"]
        );
        assert_eq!(
            accelerators(DEFAULT_APP_KEYBINDINGS, AppAction::PreviousRegion),
            ["<Shift>F6"]
        );
    }

    #[test]
    fn conflicts_are_reported() {
        let custom = [
            KeyBinding {
                action: AppAction::FocusSearch,
                accelerators: &["<Control>k"],
                description: "",
            },
            KeyBinding {
                action: AppAction::NewObjective,
                accelerators: &["<Control>K"],
                description: "",
            },
        ];
        assert_eq!(conflicts(&custom).len(), 1);
    }
}
