//! Length-bounded text input: a text view that refuses edits past its limit and says so.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::presentation::text::bound::{
    EditGuard, EndDecision, InsertDecision, Snapshot, TextLimit,
};

use super::{
    announce, clear, error_line, label, set_accessible_description, set_accessible_label, space,
    vbox,
};

pub struct LimitedInput {
    pub view: gtk::TextView,
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
                } => this.restore(buffer, &snapshot, attempted),
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

    pub fn field(&self, field: &impl IsA<gtk::Widget>) -> gtk::Box {
        let unit = vbox(0);
        unit.append(field);
        self.error.add_css_class("limited-error");
        unit.append(&self.error);
        unit
    }

    pub fn labelled(&self, label: &str, placeholder: &str, min: i32, max: i32) -> gtk::Box {
        let unit = text_field(label, placeholder, &self.view, min, max);
        self.error.add_css_class("limited-error");
        unit.append(&self.error);
        unit
    }

    pub fn connect_commit(&self, f: impl Fn(&str) + 'static) {
        *self.on_commit.borrow_mut() = Some(Box::new(f));
    }

    pub fn text(&self) -> String {
        text_of(&self.view.buffer())
    }

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

    fn restore(self: &Rc<Self>, buffer: &gtk::TextBuffer, snapshot: &Snapshot, attempted: usize) {
        self.programmatic.set(true);
        buffer.delete(&mut buffer.start_iter(), &mut buffer.end_iter());
        buffer.insert(&mut buffer.start_iter(), &snapshot.text);
        buffer.place_cursor(&buffer.iter_at_offset(snapshot.cursor));
        self.programmatic.set(false);
        self.show_rejection(attempted);
        let generation = self.guard.borrow().generation();
        let (cursor, anchor) = (snapshot.cursor, snapshot.anchor);
        let this = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(this) = this.upgrade() else { return };
            let buffer = this.view.buffer();
            if this.guard.borrow().generation() == generation
                && this.view.has_focus()
                && buffer.cursor_position() == cursor
                && !buffer.has_selection()
                && cursor != anchor
            {
                buffer.select_range(
                    &buffer.iter_at_offset(cursor),
                    &buffer.iter_at_offset(anchor),
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

pub fn bounded_text(view: &gtk::TextView, min: i32, max: i32) -> gtk::ScrolledWindow {
    view.set_wrap_mode(gtk::WrapMode::WordChar);
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
        .overlay_scrolling(true)
        .child(view)
        .build();
    scroll.set_overflow(gtk::Overflow::Hidden);
    scroll.add_css_class("text-input");
    scroll
}

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
        move |buffer: &gtk::TextBuffer| hint.set_visible(buffer.char_count() == 0)
    };
    update(&view.buffer());
    view.buffer().connect_changed(update);
    overlay
}

pub fn text_field(
    label_text: &str,
    placeholder: &str,
    view: &gtk::TextView,
    min: i32,
    max: i32,
) -> gtk::Box {
    let unit = vbox(space::TIGHT);
    unit.add_css_class("text-field");
    unit.append(&label(label_text, &["field-label"]));
    set_accessible_label(view, label_text);
    unit.append(&placeholder_text(view, placeholder, min, max));
    unit
}
