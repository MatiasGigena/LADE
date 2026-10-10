//! Small widget helpers shared by the views. Styling comes only from `assets/style.css` classes.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::model::{ActionAvailability, Attention, Lifecycle};
use crate::presentation::{format, keys};

/// Polite screen-reader announcement. GTK 4.14 calls a missing `announce` implementation (a
/// NULL vfunc: segfault) when the window's accessibility context is not the AT-SPI backend, e.g.
/// without a session bus, so other contexts are skipped; the visible text remains. With
/// `GTK_A11Y=none` GTK creates no context at all (the binding would panic on the NULL), so that
/// setting skips announcements before asking for one.
pub fn announce(
    widget: &impl IsA<gtk::Accessible>,
    message: &str,
    priority: gtk::AccessibleAnnouncementPriority,
) {
    if std::env::var_os("GTK_A11Y").is_some_and(|v| v == "none") {
        return;
    }
    if widget.at_context().type_().name() == "GtkAtSpiContext" {
        widget.announce(message, priority);
    }
}

/// Widget names starting with this prefix are stable keys used to restore focus after rebuilds.
pub const FOCUS_KEY_PREFIX: &str = "k:";

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    no_hyphens(&label);
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// Paths, identifiers and code must never gain inserted hyphens when wrapped.
pub fn no_hyphens(label: &gtk::Label) {
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
    label.set_attributes(Some(&attrs));
}

/// Single-line label that ellipsizes; the full text is available as tooltip.
pub fn line_label(
    text: &str,
    classes: &[&str],
    ellipsize: gtk::pango::EllipsizeMode,
) -> gtk::Label {
    let label = label(text, classes);
    label.set_wrap(false);
    label.set_ellipsize(ellipsize);
    label.set_tooltip_text(Some(text));
    label
}

/// Multi-line input with a real boundary whose height stays between `min` and `max` px (the text
/// scrolls beyond that), independent of earlier layout passes.
pub fn bounded_text(view: &gtk::TextView, min: i32, max: i32) -> gtk::ScrolledWindow {
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    // Tab moves focus to the next control instead of inserting a tab character.
    view.set_accepts_tab(false);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_left_margin(8);
    view.set_right_margin(8);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(min)
        .max_content_height(max)
        .propagate_natural_height(true)
        .child(view)
        .build();
    scroll.add_css_class("text-input");
    scroll
}

fn string_factory(ellipsize: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        if ellipsize {
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(text)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) else {
            return;
        };
        label.set_text(&text.string());
        label.set_tooltip_text(Some(&text.string()));
    });
    factory
}

/// A drop-down whose closed button ellipsizes a long choice (full text as tooltip and in the
/// open list), so it never widens a narrow column.
pub fn dropdown(choices: &[&str]) -> gtk::DropDown {
    let dropdown = gtk::DropDown::new(Some(gtk::StringList::new(choices)), None::<gtk::Expression>);
    dropdown.set_factory(Some(&string_factory(true)));
    dropdown.set_list_factory(Some(&string_factory(false)));
    dropdown.set_hexpand(true);
    let update = |d: &gtk::DropDown| {
        let text = d
            .selected_item()
            .and_downcast::<gtk::StringObject>()
            .map(|s| s.string().to_string());
        d.set_tooltip_text(text.as_deref());
    };
    update(&dropdown);
    dropdown.connect_selected_notify(update);
    dropdown
}

/// A label for a button that wraps instead of widening its column.
pub fn wrapping_button(text: &str, classes: &[&str]) -> gtk::Button {
    let button = gtk::Button::new();
    let child = gtk::Label::new(Some(text));
    child.set_wrap(true);
    child.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    child.set_justify(gtk::Justification::Center);
    no_hyphens(&child);
    button.set_child(Some(&child));
    for class in classes {
        button.add_css_class(class);
    }
    button
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub fn set_accessible_label(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

pub fn set_accessible_description(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Description(text)]);
}

pub fn set_focus_key(widget: &impl IsA<gtk::Widget>, key: &str) {
    widget.set_widget_name(&format!("{FOCUS_KEY_PREFIX}{key}"));
}

pub fn button(text: &str, classes: &[&str]) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    for class in classes {
        button.add_css_class(class);
    }
    button
}

pub fn section_title(text: &str) -> gtk::Label {
    heading(text, "section-title")
}

/// A label exposed with the heading role.
pub fn heading(text: &str, class: &str) -> gtk::Label {
    let title = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .accessible_role(gtk::AccessibleRole::Heading)
        .build();
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.add_css_class(class);
    title
}

/// Compact status pill: sign plus text, never color alone.
pub fn status_pill(lifecycle: Lifecycle, attention: Attention) -> gtk::Box {
    let (sign, text) = format::status_pill(lifecycle, attention);
    pill(sign, text)
}

pub fn pill(sign: &str, text: &str) -> gtk::Box {
    let pill = hbox(6);
    pill.add_css_class("pill");
    pill.set_halign(gtk::Align::Start);
    pill.set_valign(gtk::Align::Center);
    let sign_label = gtk::Label::new(Some(sign));
    sign_label.add_css_class("pill-sign");
    pill.append(&sign_label);
    pill.append(&gtk::Label::new(Some(text)));
    set_accessible_label(&pill, text);
    pill
}

/// Signed change counts "+N −M" with semantic ink (and the signs as text equivalents).
pub fn counts(added: u64, removed: u64) -> gtk::Box {
    let row = hbox(8);
    let add = gtk::Label::new(Some(&format!("+{added}")));
    add.add_css_class("added");
    add.add_css_class("numeric");
    let del = gtk::Label::new(Some(&format!("−{removed}")));
    del.add_css_class("removed");
    del.add_css_class("numeric");
    row.append(&add);
    row.append(&del);
    set_accessible_label(
        &row,
        &format!("{added} lines added, {removed} lines removed"),
    );
    row
}

/// An action whose availability comes only from the service projection. A disabled action keeps
/// its place and shows the reason as visible text and tooltip; a not-offered one is hidden.
pub fn action_row(
    text: &str,
    availability: &ActionAvailability,
    classes: &[&str],
    on_click: impl Fn(&gtk::Button) + 'static,
) -> gtk::Box {
    let row = vbox(4);
    let button = wrapping_button(text, classes);
    set_accessible_label(&button, text);
    set_focus_key(&button, &format!("action:{text}"));
    button.set_hexpand(true);
    button.connect_clicked(on_click);
    row.append(&button);
    match availability {
        ActionAvailability::Enabled => {}
        ActionAvailability::Disabled { reason } => {
            button.set_sensitive(false);
            button.set_tooltip_text(Some(reason));
            for class in classes {
                if *class == "primary" {
                    button.remove_css_class("primary");
                }
            }
            let why = label(reason, &["meta"]);
            set_accessible_description(&button, reason);
            row.append(&why);
        }
        ActionAvailability::NotOffered => row.set_visible(false),
    }
    row
}

/// Local error line near the control that failed.
pub fn error_line(text: &str) -> gtk::Box {
    let row = hbox(8);
    row.add_css_class("error-box");
    let sign = gtk::Label::new(Some("✕"));
    row.append(&sign);
    let message = label(text, &[]);
    message.set_hexpand(true);
    row.append(&message);
    set_accessible_label(&row, &format!("Error: {text}"));
    row
}

pub fn warning_line(text: &str) -> gtk::Box {
    let row = hbox(8);
    row.add_css_class("warning-box");
    let sign = gtk::Label::new(Some("⚠"));
    sign.set_valign(gtk::Align::Start);
    row.append(&sign);
    let message = label(text, &[]);
    message.set_hexpand(true);
    row.append(&message);
    set_accessible_label(&row, &format!("Warning: {text}"));
    row
}

/// Key/value row for the inspector.
pub fn field(key: &str, value: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = hbox(12);
    let key_label = label(key, &["meta", "field-key"]);
    key_label.set_valign(gtk::Align::Start);
    key_label.set_width_chars(11);
    key_label.set_max_width_chars(11);
    row.append(&key_label);
    value.set_hexpand(true);
    row.append(value);
    row
}

/// Restores focus to the same keyed widget after `rebuild` replaced the container's children, or to
/// the container's first focusable child; never moves focus that was elsewhere.
pub fn rebuild_preserving_focus(container: &impl IsA<gtk::Widget>, rebuild: impl FnOnce()) {
    let container = container.as_ref();
    let focused = container.root().and_then(|r| r.focus());
    let inside = focused
        .as_ref()
        .is_some_and(|f| f.is_ancestor(container) || f == container);
    let key = focused
        .filter(|_| inside)
        .map(|f| f.widget_name().to_string());
    rebuild();
    if !inside {
        return;
    }
    let restored = key
        .filter(|k| k.starts_with(FOCUS_KEY_PREFIX))
        .and_then(|k| find_named(container, &k))
        .is_some_and(|w| w.grab_focus());
    if !restored {
        container.child_focus(gtk::DirectionType::TabForward);
    }
}

pub fn find_named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(found) = find_named(&c, name) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Installs Escape (from the keybinding table) to close a dialog window.
fn install_close_shortcut(window: &gtk::Window) {
    let controller = gtk::ShortcutController::new();
    // Fixed binding: Escape always dismisses the local dialog (see `keys`).
    let accel = keys::binding(keys::AppAction::CloseDialog).default;
    if let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) {
        let action = gtk::CallbackAction::new(|widget, _| {
            if let Some(window) = widget.downcast_ref::<gtk::Window>() {
                window.close();
            }
            glib::Propagation::Stop
        });
        controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
    }
    window.add_controller(controller);
}

/// A modal dialog whose closing returns focus to `opener`.
pub struct Dialog {
    pub window: gtk::Window,
    pub body: gtk::Box,
    pub buttons: gtk::Box,
    pub error: gtk::Box,
}

impl Dialog {
    pub fn new(opener: &impl IsA<gtk::Widget>, title: &str, width: i32) -> Self {
        let opener = opener.as_ref().clone();
        let parent = opener.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let window = gtk::Window::builder()
            .modal(true)
            .title(title)
            .default_width(width)
            .resizable(true)
            .build();
        window.set_transient_for(parent.as_ref());
        window.add_css_class("lade-dialog");
        let outer = vbox(16);
        outer.add_css_class("dialog-content");
        outer.append(&section_title(title));
        let body = vbox(12);
        outer.append(&body);
        let error = vbox(0);
        outer.append(&error);
        let buttons = hbox(8);
        buttons.set_halign(gtk::Align::End);
        outer.append(&buttons);
        window.set_child(Some(&outer));
        install_close_shortcut(&window);
        window.connect_close_request(move |_| {
            opener.grab_focus();
            glib::Propagation::Proceed
        });
        Self {
            window,
            body,
            buttons,
            error,
        }
    }

    pub fn show_error(&self, message: &str) {
        self.show_errors(&[message.to_owned()]);
    }

    /// Replaces the dialog's error area with one error line per message.
    pub fn show_errors(&self, messages: &[String]) {
        clear(&self.error);
        self.error.set_spacing(8);
        for message in messages {
            self.error.append(&error_line(message));
        }
    }

    pub fn add_cancel(&self) -> gtk::Button {
        let cancel = button("Cancel", &[]);
        let window = self.window.clone();
        cancel.connect_clicked(move |_| window.close());
        self.buttons.append(&cancel);
        cancel
    }

    pub fn add_button(&self, text: &str, classes: &[&str]) -> gtk::Button {
        let b = button(text, classes);
        self.buttons.append(&b);
        b
    }

    pub fn present(&self) {
        self.window.present();
    }
}

/// Monotonic milliseconds for draft scheduling.
pub fn monotonic_ms() -> u64 {
    u64::try_from(glib::monotonic_time() / 1000).unwrap_or(0)
}

/// Wall-clock milliseconds for relative timestamps.
pub fn now_ms() -> i64 {
    glib::real_time() / 1000
}
