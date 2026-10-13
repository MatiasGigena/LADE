//! Small widget helpers shared by the views. Styling comes only from `assets/style.css` classes.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::presentation::keys;
use crate::presentation::model::ActionAvailability;
use crate::presentation::text_bound::{
    EditGuard, EndDecision, InsertDecision, Snapshot, TextLimit,
};

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

/// Spacing tokens (logical px), the only gaps the views use between widgets. The stylesheet's
/// paddings use the same scale (`assets/style.css`, "Spacing").
pub mod space {
    /// Lines that belong to one item (a label and its reason, a title and its secondary line).
    pub const TIGHT: i32 = 4;
    /// Items of a group; controls in a row.
    pub const ITEM: i32 = 8;
    /// Groups within a section.
    pub const GROUP: i32 = 12;
    /// Sections of a panel.
    pub const SECTION: i32 = 16;
    /// Blocks of a page.
    pub const PAGE: i32 = 24;
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

/// A multi-line input whose user edits are atomic and bounded (`presentation::text_bound`): an
/// edit that would exceed the limit is stopped before insertion and the previous text, cursor and
/// selection are restored, with a local error under the field. Focus stays in the field. The model
/// receives one commit per complete user edit, never GTK's intermediate deletion.
pub struct LimitedInput {
    pub view: gtk::TextView,
    /// Local error area; place it right below the field.
    pub error: gtk::Box,
    what: String,
    guard: RefCell<EditGuard>,
    programmatic: Cell<bool>,
    on_commit: RefCell<Option<CommitHandler>>,
}

type CommitHandler = Box<dyn Fn(&str)>;

impl LimitedInput {
    pub fn new(view: &gtk::TextView, limit: TextLimit, what: &str) -> Rc<Self> {
        let input = Rc::new(Self {
            view: view.clone(),
            error: vbox(0),
            what: what.to_owned(),
            guard: RefCell::new(EditGuard::new(limit)),
            programmatic: Cell::new(false),
            on_commit: RefCell::new(None),
        });
        let buffer = view.buffer();
        let this = Rc::downgrade(&input);
        buffer.connect_begin_user_action(move |buffer| {
            if let Some(this) = this.upgrade() {
                let offset = |mark: &gtk::TextMark| buffer.iter_at_mark(mark).offset();
                this.guard.borrow_mut().begin(Snapshot {
                    text: text_of(buffer),
                    cursor: offset(&buffer.get_insert()),
                    anchor: offset(&buffer.selection_bound()),
                });
            }
        });
        let this = Rc::downgrade(&input);
        buffer.connect_insert_text(move |buffer, _, inserted| {
            let Some(this) = this.upgrade() else { return };
            if this.programmatic.get() {
                return;
            }
            let decision = this.guard.borrow_mut().insert(&text_of(buffer), inserted);
            if let InsertDecision::Reject { attempted } = decision {
                buffer.stop_signal_emission_by_name("insert-text");
                // Inside a user action the error is shown once the snapshot is restored.
                if !this.guard.borrow().in_action() {
                    this.show_rejection(attempted);
                }
            }
        });
        let this = Rc::downgrade(&input);
        buffer.connect_end_user_action(move |buffer| {
            let Some(this) = this.upgrade() else { return };
            let decision = this.guard.borrow_mut().end();
            match decision {
                EndDecision::Commit => this.commit(),
                EndDecision::Restore {
                    snapshot,
                    attempted,
                } => this.restore(buffer, snapshot, attempted),
            }
        });
        let this = Rc::downgrade(&input);
        buffer.connect_changed(move |_| {
            if let Some(this) = this.upgrade()
                && !this.programmatic.get()
                && !this.guard.borrow().in_action()
            {
                this.commit();
            }
        });
        input
    }

    /// The field and its local error area as one unit (no extra spacing while there is no error).
    pub fn field(&self, field: &impl IsA<gtk::Widget>) -> gtk::Box {
        let unit = vbox(0);
        unit.append(field);
        self.error.add_css_class("limited-error");
        unit.append(&self.error);
        unit
    }

    /// This input as a [`text_field`] (label, field with a visible placeholder) followed by its
    /// local error area.
    pub fn labelled(&self, label: &str, placeholder: &str, min: i32, max: i32) -> gtk::Box {
        let unit = text_field(label, placeholder, &self.view, min, max);
        self.error.add_css_class("limited-error");
        unit.append(&self.error);
        unit
    }

    /// Called with the complete text after each accepted user edit.
    pub fn connect_commit(&self, f: impl Fn(&str) + 'static) {
        *self.on_commit.borrow_mut() = Some(Box::new(f));
    }

    pub fn text(&self) -> String {
        text_of(&self.view.buffer())
    }

    /// Replaces the text from the model (no commit, no limit check: model text is bounded).
    pub fn set_text(&self, text: &str) {
        self.programmatic.set(true);
        self.view.buffer().set_text(text);
        self.programmatic.set(false);
    }

    fn commit(&self) {
        clear(&self.error);
        let text = self.text();
        if let Some(f) = self.on_commit.borrow().as_ref() {
            f(&text);
        }
    }

    fn show_rejection(&self, attempted: usize) {
        let message = self.guard.borrow().rejection(&self.what, attempted);
        clear(&self.error);
        self.error.append(&error_line(&message));
        announce(
            &self.view,
            &message,
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    }

    /// Restores the snapshot inside GTK's end-user-action emission. `set_text` would start an
    /// irreversible history action, which is invalid there; a guarded delete/insert joins the
    /// current action instead. The oriented selection is restored on idle, after the X11 PRIMARY
    /// release caused by the deletion has been processed, and only if nothing changed meanwhile.
    fn restore(self: &Rc<Self>, buffer: &gtk::TextBuffer, snapshot: Snapshot, attempted: usize) {
        self.programmatic.set(true);
        buffer.delete(&mut buffer.start_iter(), &mut buffer.end_iter());
        buffer.insert(&mut buffer.start_iter(), &snapshot.text);
        buffer.place_cursor(&buffer.iter_at_offset(snapshot.cursor));
        self.programmatic.set(false);
        self.show_rejection(attempted);
        let generation = self.guard.borrow().generation();
        let this = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(this) = this.upgrade() else { return };
            let buffer = this.view.buffer();
            if this.guard.borrow().generation() == generation
                && this.view.has_focus()
                && buffer.cursor_position() == snapshot.cursor
                && !buffer.has_selection()
                && snapshot.cursor != snapshot.anchor
            {
                buffer.select_range(
                    &buffer.iter_at_offset(snapshot.cursor),
                    &buffer.iter_at_offset(snapshot.anchor),
                );
            }
        });
    }
}

fn text_of(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
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
        .overlay_scrolling(false)
        .child(view)
        .build();
    scroll.add_css_class("text-input");
    scroll
}

/// A multi-line field with a visible placeholder: [`bounded_text`] with a dim hint laid over its
/// first line (`GtkTextView` has no placeholder of its own). The hint shows only while the buffer
/// is empty and the field is unfocused; it is also the view's accessible description.
pub fn placeholder_text(
    view: &gtk::TextView,
    placeholder: &str,
    min: i32,
    max: i32,
) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&bounded_text(view, min, max)));
    let hint = label(placeholder, &["text-placeholder"]);
    hint.set_can_target(false);
    hint.set_valign(gtk::Align::Start);
    overlay.add_overlay(&hint);
    overlay.set_clip_overlay(&hint, true);
    set_accessible_description(view, placeholder);
    let update = {
        let hint = hint.clone();
        move |view: &gtk::TextView| {
            hint.set_visible(view.buffer().char_count() == 0 && !view.has_focus());
        }
    };
    update(view);
    let weak = view.downgrade();
    let on_change = update.clone();
    view.buffer().connect_changed(move |_| {
        if let Some(view) = weak.upgrade() {
            on_change(&view);
        }
    });
    view.connect_has_focus_notify(update);
    overlay
}

/// A labelled multi-line input: a metadata label above a [`placeholder_text`] field, grouped
/// tightly. The label is also the view's accessible name.
pub fn text_field(
    label_text: &str,
    placeholder: &str,
    view: &gtk::TextView,
    min: i32,
    max: i32,
) -> gtk::Box {
    let unit = vbox(space::TIGHT);
    unit.add_css_class("text-field");
    unit.append(&label(label_text, &["meta"]));
    set_accessible_label(view, label_text);
    unit.append(&placeholder_text(view, placeholder, min, max));
    unit
}

/// The kind of an [`inline_message`]: its sign, fill and accessible prefix.
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

/// A local message banner near the control or content it concerns: a sign and a plain sentence on
/// the kind's semantic fill. Technical details (service codes, raw errors) stay out of the sentence
/// in a collapsed "Details" disclosure and are the banner's accessible description; its accessible
/// name is "<Kind>: <text>".
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

/// A centred, calm empty state: a heading, an optional one-paragraph explanation (omitted when
/// empty) and the relevant action, if any. No illustration or motivational copy (design.md §5).
pub fn empty_state(title: &str, body: &str, action: Option<&gtk::Button>) -> gtk::Widget {
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
    block.upcast()
}

/// A region viewport: vertical scrolling when the content is taller than the window, never a
/// horizontal scrollbar, and non-overlay scrollbars (GTK's transient overlay sliders produced
/// unallocated-snapshot warnings in the IMP-002 checkpoint and can hide content under them).
pub fn region_scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
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

/// A disclosure: a quiet toggle button whose label wraps, showing or hiding `child` below it. The
/// child stays rooted while hidden — a collapsed `GtkExpander` caused a `gtk_root_get_focus`
/// critical during compact Tab traversal in the IMP-002 checkpoint, so it is not used.
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
    // The whole row is the target (text left-aligned), not only the words.
    toggle.set_halign(gtk::Align::Fill);
    let text = gtk::Label::new(None);
    text.set_wrap(true);
    text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.set_max_width_chars(60);
    text.set_xalign(0.0);
    toggle.set_child(Some(&text));
    set_accessible_label(&toggle, title);
    set_focus_key(&toggle, &format!("disclosure:{title}"));
    let child = child.as_ref().clone();
    let render = {
        let title = title.to_owned();
        let child = child.clone();
        move |toggle: &gtk::ToggleButton| {
            let open = toggle.is_active();
            text.set_text(&format!("{} {title}", if open { "▾" } else { "▸" }));
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

/// A sign followed by its (wrapping) text, never colour alone and no pill box (an Objective's
/// status: `presentation::panel::status_line`); the accessible name is the text.
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

/// Signed change counts "+N −M" with semantic ink (and the signs as text equivalents).
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

/// An action whose availability comes only from the service projection. A disabled action keeps
/// its place and shows the reason as visible text and tooltip; a not-offered one is hidden.
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

/// A secondary action in a list (More actions): a quiet, left-aligned button with the same
/// availability rules as [`action_row`] — a disabled one keeps its place and its reason is visible
/// below it, in the tooltip and as accessible description; a not-offered one is hidden.
/// `destructive` uses the destructive ink (never the primary treatment).
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

/// A compact action whose disabled reason is its tooltip and accessible description, for places
/// where a visible status line beside it already states why (the Activity header).
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

/// Local error line near the control that failed.
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

/// Key/value row for the inspector.
pub fn field(key: &str, value: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = hbox(space::GROUP);
    let key_label = label(key, &["meta", "field-key"]);
    key_label.set_valign(gtk::Align::Start);
    key_label.set_width_chars(10);
    key_label.set_max_width_chars(10);
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
        let outer = vbox(space::SECTION);
        outer.add_css_class("dialog-content");
        outer.append(&section_title(title));
        let body = vbox(space::GROUP);
        outer.append(&body);
        let error = vbox(0);
        outer.append(&error);
        let buttons = hbox(space::ITEM);
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
        self.error.set_spacing(space::ITEM);
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
