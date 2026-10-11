//! Modal dialogs. Each closes on Escape (keybinding table) and returns focus to its opener.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{self, Dialog, LimitedInput, label, set_accessible_label, vbox};
use crate::presentation::controller::{Controller, FeedbackBatch, UnsavedDraft};
use crate::presentation::format;
use crate::presentation::intents::StartPoint;
use crate::presentation::keys::{self, AppAction, Keymap};
use crate::presentation::model::Intent as ObjectiveIntent;
use crate::presentation::text_bound::{OVERRIDE_REASON_LIMIT, TEXT_LIMIT};

/// Generic confirmation; `destructive` uses the destructive treatment for the confirm button.
pub fn confirm(
    opener: &impl IsA<gtk::Widget>,
    title: &str,
    body: &str,
    confirm_label: &str,
    destructive: bool,
    on_confirm: impl Fn() -> Result<(), String> + 'static,
) {
    let dialog = Dialog::new(opener, title, 440);
    dialog.body.append(&label(body, &[]));
    dialog.add_cancel().grab_focus();
    let ok = dialog.add_button(
        confirm_label,
        &[if destructive {
            "destructive"
        } else {
            "primary"
        }],
    );
    let window = dialog.window.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    ok.connect_clicked(move |_| match on_confirm() {
        Ok(()) => window.close(),
        Err(message) => d.show_error(&message),
    });
    dialog.present();
}

/// Closing or Exit would lose draft text the service has not acknowledged: the developer keeps the
/// window open (default) or explicitly discards that text. Returns the dialog window so the
/// caller can close it when the drafts get saved meanwhile.
pub fn unsaved_drafts(
    opener: &impl IsA<gtk::Widget>,
    exit: bool,
    drafts: &[UnsavedDraft],
    connected: bool,
    on_discard: impl Fn() + 'static,
) -> gtk::Window {
    let count = drafts.len();
    let plural = if count == 1 { "" } else { "s" };
    let title = if exit {
        "Exit with unsaved feedback?"
    } else {
        "Close with unsaved feedback?"
    };
    let dialog = Dialog::new(opener, title, 520);
    let why = if connected {
        "The service has not acknowledged saving it"
    } else {
        "The service is not reachable, so it cannot be saved now"
    };
    dialog.body.append(&widgets::warning_line(&format!(
        "{count} feedback draft{plural} not saved. {why}."
    )));
    let items = vbox(8);
    for draft in drafts {
        let entry = vbox(2);
        entry.add_css_class("confirm-item");
        entry.append(&label(
            &format!("{} · {}", draft.objective, draft.reference_label),
            &["meta"],
        ));
        entry.append(&label(&draft.status, &[]));
        items.append(&entry);
    }
    dialog.body.append(&items);
    dialog.body.append(&label(
        if exit {
            "If you exit now, the unsaved text is lost; a draft keeps only the revision the service saved before. Keep the window open to retry saving."
        } else {
            "If you close now, the unsaved text is lost; a draft keeps only the revision the service saved before. Keep the window open to retry saving."
        },
        &["secondary"],
    ));
    let keep = dialog.add_button("Keep window open", &[]);
    let window = dialog.window.clone();
    keep.connect_clicked(move |_| window.close());
    keep.grab_focus();
    let discard = dialog.add_button(
        if exit {
            "Exit and discard unsaved text"
        } else {
            "Close and discard unsaved text"
        },
        &["destructive"],
    );
    discard.connect_clicked(move |_| on_discard());
    dialog.present();
    dialog.window
}

pub fn approve_with_override(opener: &impl IsA<gtk::Widget>, controller: &Rc<Controller>) {
    let dialog = Dialog::new(opener, "Approve with an override reason", 480);
    dialog.body.append(&label(
        "Checks for this candidate are missing or did not all pass. Approval can still be recorded with a deliberate reason; it never bypasses candidate identity, target safety or the separate integration step.",
        &["secondary"],
    ));
    let reason = gtk::TextView::new();
    set_accessible_label(&reason, "Override reason");
    dialog.body.append(&label(
        "Override reason (recorded with the approval)",
        &["meta"],
    ));
    let reason_input = LimitedInput::new(&reason, OVERRIDE_REASON_LIMIT, "the override reason");
    dialog
        .body
        .append(&reason_input.field(&widgets::bounded_text(&reason, 96, 200)));
    dialog.add_cancel();
    let ok = dialog.add_button("Approve candidate", &["primary"]);
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    ok.connect_clicked(
        move |_| match controller.approve(Some(reason_input.text())) {
            Ok(()) => window.close(),
            Err(message) => d.show_error(&message),
        },
    );
    dialog.present();
    reason.grab_focus();
}

pub fn cleanup(
    opener: &impl IsA<gtk::Widget>,
    controller: &Rc<Controller>,
    branch: &str,
    unsent: u64,
) {
    let dialog = Dialog::new(opener, "Clean up branch and worktree", 480);
    dialog.body.append(&label(
        &format!(
            "This removes the branch {branch} and its worktree. The Objective record, accepted inputs and review evidence are kept."
        ),
        &[],
    ));
    if unsent > 0 {
        dialog.body.append(&widgets::warning_line(&format!(
            "{unsent} unsent draft{} still reference this work.",
            if unsent == 1 { "" } else { "s" }
        )));
    }
    let check = gtk::CheckButton::with_label(
        "Also remove uncommitted or unretained data in the worktree (cannot be undone)",
    );
    dialog.body.append(&check);
    dialog.add_cancel().grab_focus();
    let ok = dialog.add_button("Clean up", &["destructive"]);
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    ok.connect_clicked(move |_| match controller.cleanup(check.is_active()) {
        Ok(()) => window.close(),
        Err(message) => d.show_error(&message),
    });
    dialog.present();
}

/// The deliberate deletion of a cleaned-up Objective's Git history. The button stays disabled
/// until the user ticks the acknowledgement; nothing else ever sends this request.
pub fn delete_history(opener: &impl IsA<gtk::Widget>, controller: &Rc<Controller>, title: &str) {
    let dialog = Dialog::new(opener, "Delete Git history", 500);
    dialog.body.append(&label(
        &format!(
            "This deletes the retention references (refs/lade/retain/…) that keep every inspected capture and candidate of “{title}”, and its lade/ branch if it still exists. Git may then garbage-collect that content: candidates and captures of this Objective can no longer be inspected, and draft or feedback references into them cannot be re-checked. Integrated commits on the target, the Objective record, accepted inputs and review evidence are kept."
        ),
        &[],
    ));
    let check =
        gtk::CheckButton::with_label("I understand this history cannot be restored by LADE");
    dialog.body.append(&check);
    dialog.add_cancel().grab_focus();
    let ok = dialog.add_button("Delete history", &["destructive"]);
    ok.set_sensitive(false);
    let button = ok.clone();
    check.connect_toggled(move |c| button.set_sensitive(c.is_active()));
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        if !check.is_active() {
            return;
        }
        match controller.delete_history() {
            Ok(()) => window.close(),
            Err(message) => d.show_error(&message),
        }
    });
    dialog.present();
}

/// Lists the exact items, destination and mode before the explicit submission request.
pub fn submit_feedback(
    opener: &impl IsA<gtk::Widget>,
    controller: &Rc<Controller>,
    batch: FeedbackBatch,
) {
    let dialog = Dialog::new(opener, "Submit feedback to the agent", 560);
    let destination = batch.destination.as_ref().map_or_else(
        || "No destination".to_owned(),
        |s| {
            format!(
                "Agent Session {} · {} · association r{} · generation {}",
                s.session_id.short(),
                s.model.label(),
                s.association_revision,
                s.execution_generation
            )
        },
    );
    dialog
        .body
        .append(&widgets::field("Destination", &label(&destination, &[])));
    dialog.body.append(&widgets::field(
        "Delivery",
        &label(format::mode_label(batch.mode), &[]),
    ));
    let items = vbox(8);
    for (index, item) in batch.items.iter().enumerate() {
        let entry = vbox(2);
        entry.add_css_class("confirm-item");
        entry.append(&label(
            &format!("{}. {}", index + 1, item.reference_label),
            &["meta"],
        ));
        entry.append(&label(&item.body, &[]));
        items.append(&entry);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(320)
        .child(&items)
        .build();
    dialog.body.append(&scroll);
    dialog.body.append(&label(
        "Submitting asks the service to accept these items as one immutable input. It grants no execution permission and is not approval.",
        &["meta"],
    ));
    dialog.add_cancel().grab_focus();
    let ok = dialog.add_button(
        &format!(
            "Submit {} item{}",
            batch.items.len(),
            if batch.items.len() == 1 { "" } else { "s" }
        ),
        &["primary"],
    );
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    ok.connect_clicked(move |_| match controller.submit_feedback(&batch) {
        Ok(()) => window.close(),
        Err(message) => d.show_error(&message),
    });
    dialog.present();
}

pub fn new_objective(opener: &impl IsA<gtk::Widget>, controller: &Rc<Controller>) {
    let (branches, default_branch) = {
        let state = controller.store.state();
        state.active_workspace().map_or_else(
            || (Vec::new(), String::new()),
            |w| {
                (
                    w.branches.clone(),
                    w.default_branch.clone().unwrap_or_default(),
                )
            },
        )
    };
    let dialog = Dialog::new(opener, "New Objective", 560);
    let entry = |name: &str, placeholder: &str| {
        let e = gtk::Entry::builder().placeholder_text(placeholder).build();
        set_accessible_label(&e, name);
        dialog.body.append(&label(name, &["meta"]));
        dialog.body.append(&e);
        e
    };
    let title = entry("Title", "What should be true when this is done?");
    let area = |name: &str| {
        let view = gtk::TextView::new();
        set_accessible_label(&view, name);
        dialog.body.append(&label(name, &["meta"]));
        let input = LimitedInput::new(&view, TEXT_LIMIT, &name.to_lowercase());
        dialog
            .body
            .append(&input.field(&widgets::bounded_text(&view, 56, 140)));
        input
    };
    let outcome = area("Expected outcome");
    let constraints = area("Constraints");
    let acceptance = area("Acceptance criteria");
    let refs: Vec<&str> = branches.iter().map(String::as_str).collect();
    let target = gtk::DropDown::from_strings(&refs);
    if let Some(index) = branches.iter().position(|b| b == &default_branch) {
        target.set_selected(index as u32);
    }
    set_accessible_label(&target, "Integration target branch");
    dialog.body.append(&label(
        "Integration target (receives the accepted Change Set)",
        &["meta"],
    ));
    dialog.body.append(&target);
    let start = entry(
        "Start point",
        "Leave empty to start from the target's current commit, or enter a commit",
    );
    dialog.add_cancel();
    let ok = dialog.add_button("Create Objective", &["primary"]);
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    let text = |input: &LimitedInput| input.text();
    let title_entry = title.clone();
    ok.connect_clicked(move |_| {
        let branch = branches
            .get(target.selected() as usize)
            .cloned()
            .unwrap_or_default();
        let start_text = start.text().trim().to_owned();
        let start_point = if start_text.is_empty() {
            StartPoint::Target
        } else {
            StartPoint::Commit(start_text)
        };
        let intent = ObjectiveIntent {
            outcome: text(&outcome),
            constraints: text(&constraints),
            acceptance: text(&acceptance),
        };
        match controller.create_objective(&title_entry.text(), intent, &branch, start_point) {
            Ok(()) => window.close(),
            Err(message) => d.show_error(&message),
        }
    });
    dialog.present();
    title.grab_focus();
}

/// Edits outcome, constraints and acceptance of the selected Objective. The request carries the
/// intent revision shown here, so an intent changed meanwhile is refused rather than overwritten.
pub fn edit_intent(opener: &impl IsA<gtk::Widget>, controller: &Rc<Controller>) {
    let Some((revision, current)) = controller
        .store
        .state()
        .selected_detail()
        .map(|d| (d.intent_revision, d.intent.clone()))
    else {
        return;
    };
    let dialog = Dialog::new(opener, "Edit intent", 560);
    dialog.body.append(&label(
        &format!(
            "Editing intent revision {revision}. A change of scope may need the agent to be told and the work reviewed again."
        ),
        &["secondary"],
    ));
    let area = |name: &str, text: &str| {
        let view = gtk::TextView::new();
        set_accessible_label(&view, name);
        dialog.body.append(&label(name, &["meta"]));
        let input = LimitedInput::new(&view, TEXT_LIMIT, &name.to_lowercase());
        input.set_text(text);
        dialog
            .body
            .append(&input.field(&widgets::bounded_text(&view, 56, 160)));
        input
    };
    let outcome = area("Expected outcome", &current.outcome);
    let constraints = area("Constraints", &current.constraints);
    let acceptance = area("Acceptance criteria", &current.acceptance);
    dialog.add_cancel();
    let ok = dialog.add_button("Save intent", &["primary"]);
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    let text = |input: &LimitedInput| input.text();
    let first = outcome.view.clone();
    ok.connect_clicked(move |_| {
        let intent = ObjectiveIntent {
            outcome: text(&outcome),
            constraints: text(&constraints),
            acceptance: text(&acceptance),
        };
        match controller.update_intent(revision, intent) {
            Ok(()) => window.close(),
            Err(message) => d.show_error(&message),
        }
    });
    dialog.present();
    first.grab_focus();
}

pub fn open_workspace(window: &gtk::Window, controller: &Rc<Controller>) {
    let dialog = gtk::FileDialog::builder()
        .title("Open a Git repository")
        .accept_label("Open")
        .modal(true)
        .build();
    let controller = controller.clone();
    dialog.select_folder(Some(window), None::<&gio::Cancellable>, move |result| {
        if let Ok(Some(path)) = result.map(|f| f.path()) {
            controller.open_workspace(path);
        }
    });
}

/// Shortcut reference and remapping. Every action is listed with its current binding; remappable
/// actions take an F-key (or their default), fixed ones explain why. Apply validates the whole
/// proposal and installs it at once (then saves it as the client preference); Reset only stages
/// the defaults in the fields.
pub fn shortcuts(
    opener: &impl IsA<gtk::Widget>,
    controller: &Rc<Controller>,
    path: Option<PathBuf>,
) {
    let keymap = controller.store.state().ui.keymap.clone();
    let dialog = Dialog::new(opener, "Keyboard shortcuts", 620);
    dialog.body.append(&label(
        "Each action keeps its default or takes a function key F1–F35, optionally with Shift, Control or Alt (GTK syntax, for example <Control>F8). Other combinations stay with text fields for typing, editing, selection, clipboard and undo. Tab and Shift+Tab move within a region; arrows, Home/End and Enter work in lists.",
        &["secondary"],
    ));
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(16);
    grid.set_margin_end(16); // room for the (non-overlay) scrollbar
    let mut entries: Vec<(AppAction, gtk::Entry)> = Vec::new();
    for (row, binding) in keys::DEFAULT_APP_KEYBINDINGS.iter().enumerate() {
        let row = i32::try_from(row).unwrap_or(i32::MAX);
        let name = label(binding.description, &[]);
        name.set_hexpand(true);
        name.set_valign(gtk::Align::Center);
        grid.attach(&name, 0, row, 1, 1);
        if let Some(reason) = binding.fixed {
            let fixed = vbox(2);
            fixed.append(&label(&keymap.label(binding.action), &["mono", "strong"]));
            fixed.append(&label(reason, &["meta"]));
            fixed.set_size_request(240, -1);
            grid.attach(&fixed, 1, row, 1, 1);
        } else {
            let entry = gtk::Entry::builder()
                .text(keymap.gtk(binding.action))
                .width_chars(18)
                .build();
            entry.add_css_class("mono");
            entry.set_size_request(240, -1);
            entry.set_tooltip_text(Some(&format!("Default: {}", binding.default)));
            set_accessible_label(&entry, &format!("Shortcut for {}", binding.description));
            widgets::set_accessible_description(
                &entry,
                &format!("Currently {}", keymap.label(binding.action)),
            );
            grid.attach(&entry, 1, row, 1, 1);
            entries.push((binding.action, entry));
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .min_content_height(260)
        .max_content_height(440)
        .overlay_scrolling(false)
        .child(&grid)
        .build();
    dialog.body.append(&scroll);
    let status = label("", &["meta", "dialog-status"]);
    status.set_visible(false);
    dialog.body.append(&status);
    let reset = dialog.add_button("Reset to defaults", &[]);
    let close = dialog.add_button("Close", &[]);
    let apply = dialog.add_button("Apply", &["primary"]);
    let window = dialog.window.clone();
    close.connect_clicked(move |_| window.close());
    let dialog = Rc::new(dialog);
    let entries = Rc::new(entries);
    {
        let entries = entries.clone();
        let status = status.clone();
        let dialog = dialog.clone();
        reset.connect_clicked(move |_| {
            for (action, entry) in entries.iter() {
                entry.set_text(keys::binding(*action).default);
            }
            widgets::clear(&dialog.error);
            status.set_text("Defaults staged in the fields. Choose Apply to use them.");
            status.set_visible(true);
        });
    }
    let controller = controller.clone();
    let first = entries.first().map(|(_, e)| e.clone());
    let shown = dialog.clone();
    apply.connect_clicked(move |button| {
        let proposal: Vec<(AppAction, String)> = keys::DEFAULT_APP_KEYBINDINGS
            .iter()
            .map(|b| {
                let text = entries
                    .iter()
                    .find(|(a, _)| *a == b.action)
                    .map_or_else(|| b.default.to_owned(), |(_, e)| e.text().to_string());
                (b.action, text)
            })
            .collect();
        match Keymap::propose(&proposal) {
            Err(errors) => {
                status.set_visible(false);
                let mut shown = errors.clone();
                shown.push("Nothing was changed; the current shortcuts still apply.".to_owned());
                dialog.show_errors(&shown);
                widgets::announce(
                    button,
                    &errors.join(" "),
                    gtk::AccessibleAnnouncementPriority::Medium,
                );
            }
            Ok(keymap) => {
                widgets::clear(&dialog.error);
                for (action, entry) in entries.iter() {
                    entry.set_text(&keymap.gtk(*action));
                }
                let saved = path.as_ref().map(|p| keys::save(p, &keymap).map(|()| p));
                controller.set_keymap(keymap);
                let message = match saved {
                    Some(Ok(p)) => format!("Shortcuts applied and saved to {}.", p.display()),
                    Some(Err(e)) => format!(
                        "Shortcuts applied for this window, but they could not be saved: {e}"
                    ),
                    None => "Shortcuts applied for this window (no configuration directory: set XDG_CONFIG_HOME or HOME to keep them).".to_owned(),
                };
                status.set_text(&message);
                status.set_visible(true);
                widgets::announce(
                    button,
                    &message,
                    gtk::AccessibleAnnouncementPriority::Low,
                );
            }
        }
    });
    shown.present();
    if let Some(first) = first {
        first.grab_focus();
    }
}
