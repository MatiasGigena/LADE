//! Modal dialogs. Each closes on Escape (keybinding table) and returns focus to its opener.

use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{self, Dialog, label, set_accessible_label, vbox};
use crate::presentation::controller::{Controller, FeedbackBatch};
use crate::presentation::format;
use crate::presentation::intents::StartPoint;
use crate::presentation::model::Intent as ObjectiveIntent;

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
    dialog.body.append(&widgets::bounded_text(&reason, 96, 200));
    dialog.add_cancel();
    let ok = dialog.add_button("Approve candidate", &["primary"]);
    let window = dialog.window.clone();
    let controller = controller.clone();
    let dialog = Rc::new(dialog);
    let d = dialog.clone();
    let reason_view = reason.clone();
    ok.connect_clicked(move |_| {
        let buffer = reason_view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        match controller.approve(Some(text.to_string())) {
            Ok(()) => window.close(),
            Err(message) => d.show_error(&message),
        }
    });
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
        dialog.body.append(&widgets::bounded_text(&view, 56, 140));
        view
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
    let text = |v: &gtk::TextView| {
        let b = v.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    };
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
        view.buffer().set_text(text);
        dialog.body.append(&label(name, &["meta"]));
        dialog.body.append(&widgets::bounded_text(&view, 56, 160));
        view
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
    let text = |v: &gtk::TextView| {
        let b = v.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    };
    let first = outcome.clone();
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
