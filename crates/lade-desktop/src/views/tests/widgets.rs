//! The shared widget builders, the length-bounded input and the modal dialog.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::presentation::state::model::ActionAvailability;
use crate::presentation::text::bound::TextLimit;
use crate::views::tests::{descendants, laid_out, settle_until};
use crate::views::widgets::{
    Dialog, LimitedInput, MessageKind, action_row, compact_action, counts, disclosure, dropdown_or,
    find_named, inline_message, list_action_row, rebuild_preserving_focus, set_focus_key,
};
use crate::views::*;

/// Whether `widget` and its ancestors below any window are marked visible, shown or not.
fn marked_visible(widget: &gtk::Widget) -> bool {
    let mut current = Some(widget.clone());
    while let Some(w) = current {
        if w.is::<gtk::Window>() {
            return true;
        }
        if !w.get_visible() {
            return false;
        }
        current = w.parent();
    }
    true
}

fn labels(widget: &impl IsA<gtk::Widget>) -> Vec<String> {
    descendants(widget.as_ref())
        .into_iter()
        .filter(marked_visible)
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .collect()
}

fn first_button(widget: &impl IsA<gtk::Widget>) -> gtk::Button {
    descendants(widget.as_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Button>().ok())
        .expect("a button")
}

fn disabled(reason: &str) -> ActionAvailability {
    ActionAvailability::Disabled {
        reason: reason.into(),
    }
}

#[test]
#[ignore = "requires a graphical display"]
fn action_rows_show_why_an_action_is_disabled_and_hide_what_is_not_offered() {
    gtk::test_synced(|| {
        let clicked = Rc::new(Cell::new(0));
        let count = clicked.clone();
        let enabled = action_row(
            "Approve",
            &ActionAvailability::Enabled,
            &["primary"],
            move |_| count.set(count.get() + 1),
        );
        let button = first_button(&enabled);
        assert!(button.is_sensitive() && button.has_css_class("primary"));
        assert_eq!(button.widget_name(), "k:action:Approve");
        button.emit_clicked();
        assert_eq!(clicked.get(), 1);
        assert_eq!(labels(&enabled), ["Approve"]);

        let row = action_row(
            "Approve",
            &disabled("Run the checks first"),
            &["primary"],
            |_| {},
        );
        let button = first_button(&row);
        assert!(!button.is_sensitive());
        assert!(
            !button.has_css_class("primary"),
            "a disabled action is not primary"
        );
        assert_eq!(
            button.tooltip_text().as_deref(),
            Some("Run the checks first")
        );
        assert_eq!(labels(&row), ["Approve", "Run the checks first"]);

        assert!(!action_row("Approve", &ActionAvailability::NotOffered, &[], |_| {}).is_visible());
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn list_and_compact_actions_follow_the_same_availability_rules() {
    gtk::test_synced(|| {
        let row = list_action_row("Discard", &disabled("Integrated"), true, |_| {});
        let button = first_button(&row);
        assert!(button.has_css_class("destructive-quiet") && !button.is_sensitive());
        assert_eq!(labels(&row), ["Discard", "Integrated"]);
        assert!(
            !list_action_row("Discard", &ActionAvailability::NotOffered, false, |_| {})
                .is_visible()
        );

        let compact = compact_action("Freeze", &disabled("Already frozen"), |_| {});
        assert!(!compact.is_sensitive());
        assert_eq!(compact.tooltip_text().as_deref(), Some("Already frozen"));
        assert!(!compact_action("Freeze", &ActionAvailability::NotOffered, |_| {}).is_visible());
        assert!(compact_action("Freeze", &ActionAvailability::Enabled, |_| {}).is_sensitive());
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn an_empty_dropdown_shows_its_placeholder_and_cannot_be_used() {
    gtk::test_synced(|| {
        let empty = dropdown_or(&[], "No models");
        assert!(!empty.is_sensitive());
        assert_eq!(empty.tooltip_text().as_deref(), Some("No models"));
        let choices = dropdown_or(&["One", "Two"], "No models");
        assert!(choices.is_sensitive());
        assert_eq!(choices.tooltip_text().as_deref(), Some("One"));
        choices.set_selected(1);
        assert_eq!(
            choices.tooltip_text().as_deref(),
            Some("Two"),
            "the tooltip follows the selection"
        );
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn a_disclosure_shows_its_child_only_while_open_and_reports_toggles() {
    gtk::test_synced(|| {
        let child = gtk::Label::new(Some("hidden detail"));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let record = seen.clone();
        let root = disclosure("Details", false, &child, move |open| {
            record.borrow_mut().push(open);
        });
        assert!(!child.is_visible());
        let toggle = descendants(root.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<gtk::ToggleButton>().ok())
            .expect("toggle");
        toggle.set_active(true);
        assert!(child.is_visible());
        toggle.set_active(false);
        assert!(!child.is_visible());
        assert_eq!(*seen.borrow(), [true, false]);
        let open = gtk::Label::new(Some("shown"));
        disclosure("Open", true, &open, |_| {});
        assert!(open.is_visible());
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn inline_messages_put_details_behind_a_disclosure() {
    gtk::test_synced(|| {
        let plain = inline_message(MessageKind::Warning, "Couldn't save.", Some(""));
        assert!(plain.has_css_class("message-warning"));
        assert_eq!(labels(&plain), ["⚠", "Couldn't save."]);
        let detailed = inline_message(MessageKind::Error, "Couldn't save.", Some("io: disk full"));
        assert!(detailed.has_css_class("message-error"));
        let shown = labels(&detailed);
        assert!(shown.contains(&"Details".to_owned()));
        assert!(
            !shown.contains(&"io: disk full".to_owned()),
            "collapsed by default"
        );
        assert_eq!(
            labels(&counts(3, 1)),
            ["+3", "−1"],
            "counts use a true minus sign"
        );
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn a_rebuild_keeps_focus_on_the_widget_with_the_same_key() {
    gtk::test_synced(|| {
        let window = gtk::Window::new();
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.set_child(Some(&container));
        let fill = |container: &gtk::Box| {
            widgets::clear(container);
            for key in ["first", "second"] {
                let button = gtk::Button::with_label(key);
                set_focus_key(&button, key);
                container.append(&button);
            }
        };
        fill(&container);
        window.present();
        settle_until("the keyed buttons are shown", || {
            find_named(container.upcast_ref(), "k:second").is_some_and(|w| laid_out(&w))
        });
        let second = find_named(container.upcast_ref(), "k:second").expect("keyed widget");
        assert!(second.grab_focus());
        rebuild_preserving_focus(&container, || fill(&container));
        let focused = GtkWindowExt::focus(&window).expect("focus kept inside the container");
        assert_eq!(focused.widget_name(), "k:second");
        assert_ne!(focused, second, "the rebuilt widget took the focus");
        window.close();
    });
}

/// Inserts `text` at the end of the buffer as one user action, as typing or pasting does.
fn type_text(buffer: &gtk::TextBuffer, text: &str) {
    buffer.begin_user_action();
    buffer.insert(&mut buffer.end_iter(), text);
    buffer.end_user_action();
}

#[test]
#[ignore = "requires a graphical display"]
fn a_limited_input_refuses_edits_past_its_limit_and_says_so() {
    gtk::test_synced(|| {
        let view = gtk::TextView::new();
        let input = LimitedInput::new(&view, TextLimit::Chars(5), "The title");
        let commits = Rc::new(RefCell::new(Vec::new()));
        let record = commits.clone();
        input.connect_commit(move |text| record.borrow_mut().push(text.to_owned()));
        input.set_text("abc");
        assert!(
            commits.borrow().is_empty(),
            "programmatic text is not a commit"
        );
        type_text(&view.buffer(), "de");
        assert_eq!(input.text(), "abcde");
        assert_eq!(commits.borrow().last().map(String::as_str), Some("abcde"));
        assert!(input.error.first_child().is_none());
        type_text(&view.buffer(), "fgh");
        assert_eq!(input.text(), "abcde", "the over-long edit is undone");
        assert!(
            labels(&input.error)
                .iter()
                .any(|l| l.contains("The title would be 8 characters")),
            "{:?}",
            labels(&input.error)
        );
        view.buffer().set_text("ab");
        assert!(
            input.error.first_child().is_none(),
            "an accepted edit clears the error"
        );
        assert_eq!(commits.borrow().last().map(String::as_str), Some("ab"));
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn a_dialog_lists_its_errors_and_cancel_closes_it() {
    gtk::test_synced(|| {
        let opener = gtk::Window::new();
        let anchor = gtk::Button::with_label("open");
        opener.set_child(Some(&anchor));
        opener.present();
        let dialog = Dialog::new(&anchor, "Rename", 480);
        assert!(dialog.window.is_modal());
        assert_eq!(dialog.window.transient_for().as_ref(), Some(&opener));
        assert!(!dialog.error.get_visible());
        dialog.show_errors(&["Too long".into(), "Not unique".into()]);
        assert!(dialog.error.get_visible());
        assert_eq!(labels(&dialog.error), ["✕", "Too long", "✕", "Not unique"]);
        dialog.show_errors(&[]);
        assert!(!dialog.error.get_visible());
        let cancel = dialog.add_cancel();
        dialog.add_button("Rename", &["primary"]);
        dialog.present();
        settle_until("the dialog opens", || dialog.window.is_visible());
        assert!(dialog.window.is_visible());
        cancel.emit_clicked();
        settle_until("cancel closes the dialog", || !dialog.window.is_visible());
        assert!(!dialog.window.is_visible());
        opener.close();
    });
}
