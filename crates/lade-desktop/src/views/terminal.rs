//! The Objective terminal pane (IMP-012): renders the service-owned PTY of the selected Objective
//! and sends its key bytes. The shell runs only inside the Objective's sandbox in the service;
//! this view never starts a process.
//!
//! Keyboard: the window's shortcut controller sees every key first. While the terminal has focus
//! it lets only the escape routes through (F6 / Shift+F6, the configurable Leave terminal key),
//! plus terminal copy/paste and Exit (`keys::reaches_window_from_terminal`); every other key is
//! translated to the bytes a VT100 sends (`presentation::terminal::encode_key`) — that is the
//! terminal's own input, not an application shortcut.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;

use super::widgets::{
    self, hbox, label, set_accessible_description, set_accessible_label, space, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::keys::{AppAction, Keymap};
use crate::presentation::model::{Lifecycle, ObjectiveId};
use crate::presentation::store::{Change, StoreUpdate};
use crate::presentation::terminal::{
    MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, Mods, TermKey, TerminalMode, TerminalStatus,
    encode_key, paste_text,
};

pub struct TerminalPane {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    title: gtk::Label,
    status: gtk::Label,
    problem: gtk::Box,
    pub open_button: gtk::Button,
    close_button: gtk::Button,
    pub view: gtk::TextView,
    size: Cell<(u16, u16)>,
    render_pending: Cell<bool>,
    resize_timer: RefCell<Option<glib::SourceId>>,
    shown_text: RefCell<String>,
}

fn term_key(key: gdk::Key) -> Option<TermKey> {
    use gdk::Key as K;
    Some(match key {
        K::Return | K::KP_Enter | K::ISO_Enter => TermKey::Enter,
        K::BackSpace => TermKey::Backspace,
        K::Tab | K::KP_Tab => TermKey::Tab,
        K::ISO_Left_Tab => TermKey::BackTab,
        K::Escape => TermKey::Escape,
        K::Up | K::KP_Up => TermKey::Up,
        K::Down | K::KP_Down => TermKey::Down,
        K::Left | K::KP_Left => TermKey::Left,
        K::Right | K::KP_Right => TermKey::Right,
        K::Home | K::KP_Home => TermKey::Home,
        K::End | K::KP_End => TermKey::End,
        K::Page_Up | K::KP_Page_Up => TermKey::PageUp,
        K::Page_Down | K::KP_Page_Down => TermKey::PageDown,
        K::Insert | K::KP_Insert => TermKey::Insert,
        K::Delete | K::KP_Delete => TermKey::Delete,
        K::F1 => TermKey::Function(1),
        K::F2 => TermKey::Function(2),
        K::F3 => TermKey::Function(3),
        K::F4 => TermKey::Function(4),
        K::F5 => TermKey::Function(5),
        K::F6 => TermKey::Function(6),
        K::F7 => TermKey::Function(7),
        K::F8 => TermKey::Function(8),
        K::F9 => TermKey::Function(9),
        K::F10 => TermKey::Function(10),
        K::F11 => TermKey::Function(11),
        K::F12 => TermKey::Function(12),
        other => TermKey::Char(other.to_unicode()?),
    })
}

impl TerminalPane {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = vbox(space::GROUP);
        root.add_css_class("terminal-pane");
        let head = hbox(space::SECTION);
        let titles = vbox(space::TIGHT);
        titles.set_hexpand(true);
        let title = widgets::heading("Terminal", "objective-heading");
        title.set_focusable(true);
        titles.append(&title);
        let status = label("", &["meta"]);
        titles.append(&status);
        head.append(&titles);
        let controls = hbox(space::ITEM);
        controls.set_valign(gtk::Align::Start);
        let open_button = widgets::button("Open terminal", &["primary"]);
        let close_button = widgets::button("Close terminal", &[]);
        controls.append(&open_button);
        controls.append(&close_button);
        head.append(&controls);
        root.append(&head);
        // The scope in one line; the full statement is its tooltip and accessible description.
        let full_scope = "A shell in this Objective's sandbox: its worktree only, no network, no \
             credentials, the same limits as the agent. Read-only while a Change Set is frozen for \
             review. Closing the window or losing the connection ends it; nothing restarts on its \
             own.";
        let scope = label(
            "Sandboxed: this worktree only · no network · no credentials · ends with the window",
            &["meta"],
        );
        scope.set_tooltip_text(Some(full_scope));
        set_accessible_description(&scope, full_scope);
        titles.append(&scope);
        let problem = vbox(0);
        root.append(&problem);

        let view = gtk::TextView::new();
        view.add_css_class("terminal-view");
        view.set_editable(false);
        view.set_cursor_visible(true);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::None);
        view.set_focusable(true);
        view.set_left_margin(8);
        view.set_top_margin(6);
        set_accessible_label(&view, "Terminal output");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hexpand(true)
            .min_content_height(160)
            .build();
        scroll.add_css_class("terminal-frame");
        // The probe reports the area the grid gets, so the PTY size follows the window.
        let probe = gtk::DrawingArea::new();
        probe.set_hexpand(true);
        probe.set_vexpand(true);
        probe.set_can_target(false);
        probe.set_focusable(false);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&probe));
        overlay.add_overlay(&scroll);
        overlay.set_vexpand(true);
        root.append(&overlay);

        let pane = Rc::new(Self {
            root,
            controller: controller.clone(),
            title,
            status,
            problem,
            open_button,
            close_button,
            view,
            size: Cell::new((80, 24)),
            render_pending: Cell::new(false),
            resize_timer: RefCell::new(None),
            shown_text: RefCell::new(String::new()),
        });
        pane.connect(&probe);
        pane.render();
        pane
    }

    fn connect(self: &Rc<Self>, probe: &gtk::DrawingArea) {
        let weak = Rc::downgrade(self);
        self.open_button.connect_clicked(move |_| {
            if let Some(pane) = weak.upgrade() {
                pane.open();
            }
        });
        let weak = Rc::downgrade(self);
        self.close_button.connect_clicked(move |_| {
            if let Some(pane) = weak.upgrade() {
                pane.controller.close_terminal();
            }
        });
        let weak = Rc::downgrade(self);
        probe.connect_resize(move |_, width, height| {
            if let Some(pane) = weak.upgrade() {
                pane.schedule_resize(width, height);
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
            Some(pane) => pane.key(key, state),
            None => glib::Propagation::Proceed,
        });
        self.view.add_controller(keys);
    }

    fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let Some(term) = term_key(key) else {
            return glib::Propagation::Proceed;
        };
        let mods = Mods {
            control: state.contains(gdk::ModifierType::CONTROL_MASK),
            alt: state.contains(gdk::ModifierType::ALT_MASK),
            shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        };
        let application_cursor = self.with_session(|s| s.screen.application_cursor);
        let Some(bytes) = encode_key(term, mods, application_cursor.unwrap_or(false)) else {
            return glib::Propagation::Proceed;
        };
        if self.controller.terminal_input(&bytes) {
            glib::Propagation::Stop
        } else {
            // No open terminal: Tab and the other keys keep their usual meaning.
            glib::Propagation::Proceed
        }
    }

    fn with_session<T>(
        &self,
        f: impl FnOnce(&crate::presentation::terminal::TerminalSession) -> T,
    ) -> Option<T> {
        let state = self.controller.store.state();
        let id = state.ui.selected.as_ref()?;
        state.terminal_for(id).map(f)
    }

    fn selected(&self) -> Option<ObjectiveId> {
        self.controller.store.state().ui.selected.clone()
    }

    fn open(&self) {
        let (cols, rows) = self.size.get();
        match self.controller.open_terminal(cols, rows) {
            Ok(()) => {
                self.view.grab_focus();
            }
            Err(message) => widgets::announce(
                &self.root,
                &message,
                gtk::AccessibleAnnouncementPriority::Medium,
            ),
        };
    }

    /// Moves focus into the terminal (or to Open terminal when none is open).
    pub fn focus(&self) {
        let open = self.with_session(|s| s.is_open()).unwrap_or(false);
        if open {
            self.view.grab_focus();
        } else if self.open_button.is_sensitive() {
            self.open_button.grab_focus();
        } else {
            self.title.grab_focus();
        }
    }

    pub fn has_focus(&self) -> bool {
        // The focus widget of the window, whether or not the window is active (no WM, a dialog).
        self.view.is_focus()
    }

    /// Copies the selected terminal text.
    pub fn copy(&self) {
        let buffer = self.view.buffer();
        if let Some((start, end)) = buffer.selection_bounds() {
            self.view
                .clipboard()
                .set_text(buffer.text(&start, &end, false).as_str());
        }
    }

    /// Pastes the clipboard into the open terminal (bounded; bracketed when the shell asked).
    pub fn paste(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.view
            .clipboard()
            .read_text_async(None::<&gtk::gio::Cancellable>, move |text| {
                let Some(pane) = weak.upgrade() else { return };
                let Ok(Some(text)) = text else { return };
                let bracketed = pane
                    .with_session(|s| s.screen.bracketed_paste)
                    .unwrap_or(false);
                match paste_text(&text, bracketed) {
                    Ok(data) => {
                        pane.controller.terminal_input(&data);
                    }
                    Err(message) => {
                        if let Some(objective_id) = pane.selected() {
                            pane.controller.store.apply(StoreUpdate::TerminalProblem {
                                objective_id,
                                message,
                            });
                        }
                    }
                }
            });
    }

    pub fn set_shortcut_hints(&self, keymap: &Keymap) {
        set_accessible_description(
            &self.view,
            &format!(
                "Keys go to the shell. Leave the terminal with {} or F6; copy {}, paste {}.",
                keymap.label(AppAction::LeaveTerminal),
                keymap.label(AppAction::TerminalCopy),
                keymap.label(AppAction::TerminalPaste)
            ),
        );
        self.view.set_tooltip_text(Some(&format!(
            "Leave the terminal: {} (or F6)",
            keymap.label(AppAction::LeaveTerminal)
        )));
    }

    fn schedule_resize(self: &Rc<Self>, width: i32, height: i32) {
        let layout = self.view.create_pango_layout(Some("M"));
        let (cw, ch) = layout.pixel_size();
        if cw <= 0 || ch <= 0 {
            return;
        }
        let cols = usize::try_from((width - 24) / cw)
            .unwrap_or(MIN_COLS)
            .clamp(MIN_COLS, MAX_COLS);
        let rows = usize::try_from((height - 16) / ch)
            .unwrap_or(MIN_ROWS)
            .clamp(MIN_ROWS, MAX_ROWS);
        let size = (
            u16::try_from(cols).unwrap_or(80),
            u16::try_from(rows).unwrap_or(24),
        );
        if self.size.replace(size) == size {
            return;
        }
        if let Some(timer) = self.resize_timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(150), move || {
            if let Some(pane) = weak.upgrade() {
                pane.resize_timer.borrow_mut().take();
                let (cols, rows) = pane.size.get();
                pane.controller.resize_terminal(cols, rows);
            }
        });
        *self.resize_timer.borrow_mut() = Some(id);
    }

    pub fn refresh(self: &Rc<Self>, changes: &[Change]) {
        let selected = self.selected();
        let relevant = changes.iter().any(|c| match c {
            Change::Terminal(id) | Change::Detail(id) => Some(id) == selected.as_ref(),
            Change::Selection | Change::Connection | Change::Objectives | Change::Keymap => true,
            _ => false,
        });
        if relevant && !self.render_pending.replace(true) {
            // Output bursts render once per main-loop iteration.
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(pane) = weak.upgrade() {
                    pane.render_pending.set(false);
                    pane.render();
                }
            });
        }
    }

    fn render(&self) {
        let state = self.controller.store.state();
        let summary = state.selected_summary().cloned();
        let connected = state.connection.is_connected() && !state.is_preview();
        let session = summary
            .as_ref()
            .and_then(|s| state.terminal_for(&s.objective_id))
            .cloned();
        drop(state);
        let Some(summary) = summary else {
            self.title.set_text("Terminal");
            self.status
                .set_text("Select an Objective to open a terminal in its sandbox.");
            self.open_button.set_sensitive(false);
            self.open_button.remove_css_class("primary");
            self.close_button.set_sensitive(false);
            self.set_text("");
            widgets::clear(&self.problem);
            return;
        };
        self.title
            .set_text(&format!("Terminal — {}", summary.title));
        let admits = matches!(
            summary.lifecycle,
            Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen | Lifecycle::Approved
        );
        let status = match &session {
            None if !admits => "This Objective is finished; it has no terminal.".to_owned(),
            None => "No terminal is open.".to_owned(),
            Some(s) => match &s.status {
                TerminalStatus::Opening => "Opening the sandboxed shell…".to_owned(),
                TerminalStatus::Open => s.mode.map_or("Open", TerminalMode::label).to_owned(),
                TerminalStatus::Ended(end) => end.message(),
            },
        };
        self.status.set_text(&status);
        let open = session.as_ref().is_some_and(|s| s.is_open());
        let busy = session
            .as_ref()
            .is_some_and(|s| matches!(s.status, TerminalStatus::Opening | TerminalStatus::Open));
        let can_open = connected && admits && !busy;
        self.open_button.set_sensitive(can_open);
        // The black primary treatment is for an available action only.
        if can_open {
            self.open_button.add_css_class("primary");
        } else {
            self.open_button.remove_css_class("primary");
        }
        self.open_button.set_label(if session.is_some() {
            "Open a new terminal"
        } else {
            "Open terminal"
        });
        self.close_button.set_sensitive(open);
        set_accessible_label(
            &self.view,
            &match session.as_ref().and_then(|s| s.mode) {
                Some(TerminalMode::ReadOnly) => {
                    format!("Terminal output, read-only, {}", summary.title)
                }
                Some(TerminalMode::ReadWrite) => format!("Terminal output, {}", summary.title),
                None => "Terminal output".to_owned(),
            },
        );
        widgets::clear(&self.problem);
        if let Some(problem) = session.as_ref().and_then(|s| s.problem.clone()) {
            self.problem.append(&widgets::warning_line(&problem));
        }
        let (text, cursor) = session
            .as_ref()
            .map(|s| s.screen.render())
            .unwrap_or_default();
        self.set_text(&text);
        if open {
            let buffer = self.view.buffer();
            let at_cursor = buffer.iter_at_offset(i32::try_from(cursor).unwrap_or(i32::MAX));
            if buffer.selection_bounds().is_none() {
                buffer.place_cursor(&at_cursor);
            }
            let mark = buffer.create_mark(None, &at_cursor, false);
            self.view.scroll_mark_onscreen(&mark);
            buffer.delete_mark(&mark);
        }
    }

    fn set_text(&self, text: &str) {
        if *self.shown_text.borrow() == text {
            return;
        }
        self.view.buffer().set_text(text);
        text.clone_into(&mut self.shown_text.borrow_mut());
    }
}
