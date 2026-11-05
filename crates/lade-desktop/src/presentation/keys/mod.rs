//! Application keyboard shortcuts: the rebindable actions and their defaults
//! ([`DEFAULT_APP_KEYBINDINGS`]), and the keymap a person configures over them.

mod accel;
mod config;
#[cfg(test)]
mod tests;

pub use accel::Accel;
pub use config::{config_path, load, save};

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
            } else if !own_default && (accel.function_key().is_none() || accel.other_modifier) {
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
}
