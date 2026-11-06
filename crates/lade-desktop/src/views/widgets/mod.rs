//! Shared GTK building blocks: labels, buttons, messages, disclosures and spacing, plus
//! accessibility announcements.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::state::model::ActionAvailability;

mod dialog;
mod input;

pub use dialog::Dialog;
pub use input::{LimitedInput, bounded_text, placeholder_text};

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

pub const FOCUS_KEY_PREFIX: &str = "k:";

pub mod space {
    pub const TIGHT: i32 = 4;
    pub const ITEM: i32 = 8;
    pub const GROUP: i32 = 12;
    pub const SECTION: i32 = 16;
    pub const PAGE: i32 = 24;
    pub const REGION: i32 = 32;
}

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

pub fn no_hyphens(label: &gtk::Label) {
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
    label.set_attributes(Some(&attrs));
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Warning,
    Error,
}

impl MessageKind {
    fn sign(self) -> &'static str {
        match self {
            Self::Info => "i",
            Self::Warning => "⚠",
            Self::Error => "✕",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Info => "message-info",
            Self::Warning => "message-warning",
            Self::Error => "message-error",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Info => "Note",
            Self::Warning => "Warning",
            Self::Error => "Error",
        }
    }
}

pub fn inline_message(kind: MessageKind, text: &str, details: Option<&str>) -> gtk::Box {
    let root = vbox(space::TIGHT);
    root.add_css_class("inline-message");
    root.add_css_class(kind.class());
    let row = hbox(space::ITEM);
    let sign = gtk::Label::new(Some(kind.sign()));
    sign.add_css_class("message-sign");
    sign.set_valign(gtk::Align::Start);
    row.append(&sign);
    let message = label(text, &[]);
    message.set_hexpand(true);
    row.append(&message);
    root.append(&row);
    set_accessible_label(&root, &format!("{}: {text}", kind.prefix()));
    if let Some(details) = details.filter(|d| !d.is_empty()) {
        let code = label(details, &["mono", "message-details"]);
        code.set_selectable(true);
        root.append(&disclosure("Details", false, &code, |_| {}));
        set_accessible_description(&root, details);
    }
    root
}

pub fn empty_state(
    title: &str,
    body: &str,
    action: Option<&gtk::Button>,
    note: Option<&str>,
) -> gtk::Widget {
    let block = vbox(space::GROUP);
    block.add_css_class("empty-block");
    block.set_halign(gtk::Align::Center);
    block.set_valign(gtk::Align::Center);
    block.set_hexpand(true);
    block.set_vexpand(true);
    let centred = |text: &gtk::Label| {
        text.set_xalign(0.5);
        text.set_justify(gtk::Justification::Center);
        text.set_max_width_chars(56);
    };
    let heading = heading(title, "empty-heading");
    centred(&heading);
    block.append(&heading);
    if !body.is_empty() {
        let text = label(body, &["secondary"]);
        centred(&text);
        block.append(&text);
    }
    if let Some(action) = action {
        action.set_halign(gtk::Align::Center);
        block.append(action);
    }
    if let Some(note) = note.filter(|n| !n.is_empty()) {
        let line = label(note, &["meta"]);
        centred(&line);
        block.append(&line);
    }
    block.upcast()
}

pub fn region_scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(true)
        .child(child)
        .build()
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

pub fn dropdown(choices: &[&str]) -> gtk::DropDown {
    dropdown_or(choices, "None available")
}

pub fn dropdown_or(choices: &[&str], empty: &str) -> gtk::DropDown {
    let shown: &[&str] = if choices.is_empty() {
        &[empty]
    } else {
        choices
    };
    let dropdown = gtk::DropDown::new(Some(gtk::StringList::new(shown)), None::<gtk::Expression>);
    dropdown.set_factory(Some(&string_factory(true)));
    dropdown.set_list_factory(Some(&string_factory(false)));
    dropdown.set_hexpand(true);
    if choices.is_empty() {
        dropdown.set_sensitive(false);
        set_accessible_description(&dropdown, empty);
    }
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

pub fn disclosure(
    title: &str,
    expanded: bool,
    child: &impl IsA<gtk::Widget>,
    on_toggled: impl Fn(bool) + 'static,
) -> gtk::Box {
    let root = vbox(space::TIGHT);
    root.add_css_class("disclosure");
    let toggle = gtk::ToggleButton::new();
    toggle.add_css_class("quiet");
    toggle.add_css_class("disclosure-toggle");
    toggle.set_halign(gtk::Align::Fill);
    let face = hbox(space::TIGHT);
    let chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    chevron.add_css_class("disclosure-chevron");
    chevron.set_valign(gtk::Align::Start);
    face.append(&chevron);
    let text = gtk::Label::new(Some(title));
    text.set_wrap(true);
    text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.set_max_width_chars(60);
    text.set_xalign(0.0);
    face.append(&text);
    toggle.set_child(Some(&face));
    set_accessible_label(&toggle, title);
    set_focus_key(&toggle, &format!("disclosure:{title}"));
    let child = child.as_ref().clone();
    let render = {
        let child = child.clone();
        move |toggle: &gtk::ToggleButton| {
            let open = toggle.is_active();
            chevron.set_icon_name(Some(if open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
            child.set_visible(open);
            toggle.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        }
    };
    toggle.set_active(expanded);
    render(&toggle);
    toggle.connect_toggled(move |t| {
        render(t);
        on_toggled(t.is_active());
    });
    root.append(&toggle);
    root.append(&child);
    root
}

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

/// A strip of fixed tabs on the window chrome above a pane. The current tab takes the pane's
/// fill and joins it, with a concave foot on each side; the others stay on the chrome.
pub struct PaneTabs {
    pub root: gtk::Box,
    pub tabs: Vec<gtk::ToggleButton>,
}

pub fn pane_tabs(name: &str, labels: &[&str]) -> PaneTabs {
    let root = hbox(0);
    root.add_css_class("pane-tabs");
    set_accessible_label(&root, name);
    let mut tabs: Vec<gtk::ToggleButton> = Vec::new();
    for text in labels {
        let tab = gtk::ToggleButton::with_label(text);
        tab.add_css_class("pane-tab");
        if let Some(first) = tabs.first() {
            tab.set_group(Some(first));
        }
        let cell = hbox(0);
        cell.add_css_class("pane-tab-cell");
        for side in ["start", "end"] {
            let foot = hbox(0);
            foot.add_css_class("tab-foot");
            foot.add_css_class(side);
            foot.set_valign(gtk::Align::End);
            foot.set_can_target(false);
            if side == "start" {
                cell.append(&foot);
                cell.append(&tab);
            } else {
                cell.append(&foot);
            }
        }
        let current = cell.clone();
        tab.connect_active_notify(move |tab| {
            if tab.is_active() {
                current.add_css_class("current");
            } else {
                current.remove_css_class("current");
            }
        });
        root.append(&cell);
        tabs.push(tab);
    }
    PaneTabs { root, tabs }
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

pub fn sign_text(sign: &str, text: &str) -> gtk::Box {
    let row = hbox(6);
    row.add_css_class("status-text");
    row.set_valign(gtk::Align::Center);
    let sign_label = gtk::Label::new(Some(sign));
    sign_label.add_css_class("status-sign");
    sign_label.set_valign(gtk::Align::Start);
    row.append(&sign_label);
    let text_label = label(text, &[]);
    text_label.set_hexpand(true);
    row.append(&text_label);
    set_accessible_label(&row, text);
    row
}

pub fn counts(added: u64, removed: u64) -> gtk::Box {
    let row = hbox(space::ITEM);
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

pub fn action_row(
    text: &str,
    availability: &ActionAvailability,
    classes: &[&str],
    on_click: impl Fn(&gtk::Button) + 'static,
) -> gtk::Box {
    let row = vbox(space::TIGHT);
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

pub fn list_action_row(
    text: &str,
    availability: &ActionAvailability,
    destructive: bool,
    on_click: impl Fn(&gtk::Button) + 'static,
) -> gtk::Box {
    let row = vbox(0);
    row.add_css_class("list-action");
    let button = gtk::Button::new();
    let child = label(text, &[]);
    button.set_child(Some(&child));
    button.add_css_class("quiet");
    button.add_css_class("list-action-button");
    if destructive {
        button.add_css_class("destructive-quiet");
    }
    set_accessible_label(&button, text);
    set_focus_key(&button, &format!("action:{text}"));
    button.connect_clicked(on_click);
    row.append(&button);
    match availability {
        ActionAvailability::Enabled => {}
        ActionAvailability::Disabled { reason } => {
            button.set_sensitive(false);
            button.set_tooltip_text(Some(reason));
            set_accessible_description(&button, reason);
            row.append(&label(reason, &["meta", "list-action-reason"]));
        }
        ActionAvailability::NotOffered => row.set_visible(false),
    }
    row
}

pub fn compact_action(
    text: &str,
    availability: &ActionAvailability,
    on_click: impl Fn(&gtk::Button) + 'static,
) -> gtk::Button {
    let button = button(text, &[]);
    set_accessible_label(&button, text);
    set_focus_key(&button, &format!("action:{text}"));
    button.connect_clicked(on_click);
    match availability {
        ActionAvailability::Enabled => {}
        ActionAvailability::Disabled { reason } => {
            button.set_sensitive(false);
            button.set_tooltip_text(Some(reason));
            set_accessible_description(&button, reason);
        }
        ActionAvailability::NotOffered => button.set_visible(false),
    }
    button
}

pub fn error_line(text: &str) -> gtk::Box {
    let row = hbox(space::ITEM);
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
    let row = hbox(space::ITEM);
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

pub fn field(key: &str, value: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = hbox(space::GROUP);
    let key_label = label(key, &["meta", "field-key"]);
    key_label.set_valign(gtk::Align::Start);
    key_label.set_max_width_chars(10);
    row.append(&key_label);
    value.set_hexpand(true);
    row.append(value);
    row
}

pub fn rebuild_preserving_focus(container: &impl IsA<gtk::Widget>, rebuild: impl FnOnce()) {
    let container = container.as_ref();
    let focused = container.root().and_then(|r| r.focus());
    let inside = focused
        .as_ref()
        .is_some_and(|f| f.is_ancestor(container) || f == container);
    let key = focused.filter(|_| inside).and_then(|focused| {
        let mut widget = Some(focused);
        while let Some(w) = widget {
            let name = w.widget_name();
            if name.starts_with(FOCUS_KEY_PREFIX) {
                return Some(name.to_string());
            }
            if &w == container {
                return None;
            }
            widget = w.parent();
        }
        None
    });
    rebuild();
    if !inside {
        return;
    }
    let restored = key
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

pub fn monotonic_ms() -> u64 {
    u64::try_from(glib::monotonic_time() / 1000).unwrap_or(0)
}

pub fn now_ms() -> i64 {
    glib::real_time() / 1000
}
