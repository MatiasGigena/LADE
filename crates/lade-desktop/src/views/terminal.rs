//! The terminal pane: the contained terminal of the selected Objective, its keyboard and paste
//! input, and its open, refused and ended states.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;

use super::widgets::{
    self, MessageKind, hbox, label, set_accessible_description, set_accessible_label, space, vbox,
};
use super::workbench;
use crate::presentation::controller::Controller;
use crate::presentation::keys::{AppAction, Keymap};
use crate::presentation::state::model::{
    ActionAvailability, ActionSurface, ObjectiveDetail, ObjectiveId, Refusal,
};
use crate::presentation::state::store::{Change, MainView, StoreUpdate};
use crate::presentation::terminal::{
    MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, Mods, TermKey, TerminalMode, TerminalSession,
    TerminalStatus, awaits_first_start, encode_key, paste_text,
};

const SCOPE_LINE: &str =
    "Sandboxed: this worktree only · no network · no credentials · ends with the window";
const SCOPE_FULL: &str = "A shell in this Objective's sandbox: its worktree only, no network, no \
     credentials, the same limits as the agent. Read-only while a Change Set is frozen for \
     review. Closing the window or losing the connection ends it; nothing restarts on its own.";
const SCOPE_SENTENCE: &str = "A shell in this Objective's sandbox: its worktree only, no \
     network, no credentials. Closing the window ends it.";
const DISCONNECTED: &str = "A terminal opens while the window is connected to the service.";
const NOT_OFFERED: &str = "The service offers no terminal for this Objective.";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Empty {
    FirstObjective,
    NoSelection,
    Disconnected,
    Unavailable { reason: String, activity: bool },
    Opening,
    CanOpen { note: Option<String> },
}

pub struct TerminalPane {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    title: gtk::Label,
    status: gtk::Label,
    scope: gtk::Label,
    problem: gtk::Box,
    reopen_button: gtk::Button,
    close_button: gtk::Button,
    open_button: gtk::Button,
    activity_button: gtk::Button,
    body: gtk::Stack,
    empty: gtk::Box,
    shown_empty: RefCell<Option<Empty>>,
    shown_problem: RefCell<Option<Refusal>>,
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
        let scope = label(SCOPE_LINE, &["meta"]);
        scope.set_tooltip_text(Some(SCOPE_FULL));
        set_accessible_description(&scope, SCOPE_FULL);
        titles.append(&scope);
        head.append(&titles);
        let controls = hbox(space::ITEM);
        controls.set_valign(gtk::Align::Start);
        let reopen_button = widgets::button("Open a new terminal", &[]);
        let close_button = widgets::button("Close terminal", &[]);
        controls.append(&reopen_button);
        controls.append(&close_button);
        head.append(&controls);
        root.append(&head);
        let problem = vbox(0);
        root.append(&problem);

        let open_button = widgets::button("Open terminal", &["primary"]);
        set_accessible_description(&open_button, SCOPE_FULL);
        let activity_button = widgets::button("Open Activity", &["quiet"]);
        let empty = vbox(0);
        empty.set_vexpand(true);

        let view = gtk::TextView::new();
        view.add_css_class("terminal-view");
        view.set_editable(false);
        view.set_cursor_visible(true);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::None);
        view.set_focusable(true);
        view.set_left_margin(8);
        view.set_top_margin(8);
        set_accessible_label(&view, "Terminal output");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hexpand(true)
            .min_content_height(160)
            .build();
        scroll.add_css_class("terminal-frame");
        let body = gtk::Stack::new();
        body.add_named(&empty, Some("empty"));
        body.add_named(&scroll, Some("terminal"));
        let probe = gtk::DrawingArea::new();
        probe.set_hexpand(true);
        probe.set_vexpand(true);
        probe.set_can_target(false);
        probe.set_focusable(false);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&probe));
        overlay.add_overlay(&body);
        overlay.connect_get_child_position(|overlay, _| {
            Some(gdk::Rectangle::new(0, 0, overlay.width(), overlay.height()))
        });
        overlay.set_vexpand(true);
        root.append(&overlay);

        let pane = Rc::new(Self {
            root,
            controller: controller.clone(),
            title,
            status,
            scope,
            problem,
            reopen_button,
            close_button,
            open_button,
            activity_button,
            body,
            empty,
            shown_empty: RefCell::new(None),
            shown_problem: RefCell::new(None),
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
        for button in [&self.open_button, &self.reopen_button] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(pane) = weak.upgrade() {
                    pane.open();
                }
            });
        }
        let controller = self.controller.clone();
        self.activity_button
            .connect_clicked(move |_| controller.set_view(MainView::Activity));
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
        if self.controller.terminal_input(&bytes).is_ok() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    fn with_session<T>(&self, f: impl FnOnce(&TerminalSession) -> T) -> Option<T> {
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
        }
    }

    pub fn focus(&self) {
        let open = self.with_session(TerminalSession::is_open).unwrap_or(false);
        let usable = |b: &gtk::Button| b.is_mapped() && b.is_sensitive();
        if open {
            self.view.grab_focus();
        } else if let Some(button) = [
            &self.open_button,
            &self.reopen_button,
            &self.activity_button,
        ]
        .into_iter()
        .find(|b| usable(b))
        {
            button.grab_focus();
        } else {
            self.title.grab_focus();
        }
    }

    pub fn has_focus(&self) -> bool {
        self.view.is_focus()
    }

    pub fn copy(&self) {
        let buffer = self.view.buffer();
        if let Some((start, end)) = buffer.selection_bounds() {
            self.view
                .clipboard()
                .set_text(buffer.text(&start, &end, false).as_str());
        }
    }

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
                let pasted = paste_text(&text, bracketed)
                    .and_then(|data| pane.controller.terminal_input(&data));
                if let (Err(message), Some(objective_id)) = (pasted, pane.selected()) {
                    pane.controller.store.apply(StoreUpdate::TerminalProblem {
                        objective_id,
                        message,
                    });
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
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(pane) = weak.upgrade() {
                    pane.render_pending.set(false);
                    pane.render();
                }
            });
        }
    }

    fn show_header(&self, status: Option<&str>, reopen: bool, can_open: bool) {
        let exists = status.is_some();
        self.status.set_text(status.unwrap_or(""));
        self.status.set_visible(exists);
        self.scope.set_visible(exists);
        self.reopen_button.set_visible(exists && reopen);
        self.reopen_button.set_sensitive(can_open);
        if can_open {
            self.reopen_button.add_css_class("primary");
        } else {
            self.reopen_button.remove_css_class("primary");
        }
        self.close_button.set_visible(exists && !reopen);
    }

    fn show_problem(&self, problem: Option<Refusal>) {
        if *self.shown_problem.borrow() == problem {
            return;
        }
        widgets::clear(&self.problem);
        self.shown_problem.replace(problem.clone());
        if let Some(refusal) = problem {
            let kind = if refusal.details.is_empty() {
                MessageKind::Warning
            } else {
                MessageKind::Info
            };
            self.problem.append(&widgets::inline_message(
                kind,
                &refusal.message,
                Some(&refusal.details),
            ));
        }
    }

    fn show_empty(&self, kind: Empty) {
        self.body.set_visible_child_name("empty");
        if self.shown_empty.borrow().as_ref() == Some(&kind) {
            return;
        }
        for button in [&self.open_button, &self.activity_button] {
            if let Some(parent) = button.parent().and_downcast::<gtk::Box>() {
                parent.remove(button);
            }
        }
        widgets::clear(&self.empty);
        let (title, body, action, scope) = match &kind {
            Empty::FirstObjective => {
                self.empty
                    .append(&workbench::first_objective_state(&self.controller));
                self.shown_empty.replace(Some(kind));
                return;
            }
            Empty::NoSelection => (
                "No Objective selected",
                "Select an Objective to open a terminal in its sandbox.",
                None,
                false,
            ),
            Empty::Disconnected => ("No terminal open", DISCONNECTED, None, true),
            Empty::Unavailable { reason, activity } => {
                let action = activity.then(|| {
                    set_accessible_description(&self.activity_button, reason);
                    &self.activity_button
                });
                ("No terminal open", reason.as_str(), action, *activity)
            }
            Empty::Opening => ("Opening the terminal…", "", None, true),
            Empty::CanOpen { note } => (
                "No terminal open",
                note.as_deref().unwrap_or(""),
                Some(&self.open_button),
                true,
            ),
        };
        self.empty.append(&widgets::empty_state(
            title,
            body,
            action,
            scope.then_some(SCOPE_SENTENCE),
        ));
        self.shown_empty.replace(Some(kind));
    }

    fn render(&self) {
        let state = self.controller.store.state();
        let summary = state.selected_summary().cloned();
        let detail = state.selected_detail();
        let availability = detail.map(ObjectiveDetail::terminal_availability);
        let note = detail
            .and_then(ObjectiveDetail::terminal_note)
            .map(str::to_owned);
        let connected = state.connection.is_connected();
        let board_empty = workbench::board_is_empty(&state);
        let session = summary
            .as_ref()
            .and_then(|s| state.terminal_for(&s.objective_id))
            .cloned();
        let refusal = summary
            .as_ref()
            .and_then(|s| state.last_refusal(&s.objective_id, ActionSurface::Terminal))
            .cloned();
        drop(state);
        let Some(summary) = summary else {
            self.title.set_text("Terminal");
            self.show_header(None, false, false);
            self.show_problem(None);
            self.set_text("");
            self.show_empty(if board_empty {
                Empty::FirstObjective
            } else {
                Empty::NoSelection
            });
            return;
        };
        self.title
            .set_text(&format!("Terminal — {}", summary.title));
        let open = session.as_ref().is_some_and(TerminalSession::is_open);
        let opening = session
            .as_ref()
            .is_some_and(|s| s.status == TerminalStatus::Opening);
        let exists = session.is_some() && !opening;
        let enabled = availability == Some(ActionAvailability::Enabled);
        let can_open = connected && enabled && !open && !opening;
        let unavailable = match availability {
            _ if !connected => Some(DISCONNECTED.to_owned()),
            Some(ActionAvailability::Disabled { reason }) => Some(reason),
            Some(ActionAvailability::NotOffered) => Some(NOT_OFFERED.to_owned()),
            Some(ActionAvailability::Enabled) | None => None,
        };
        if !exists {
            self.show_problem(refusal.filter(|_| unavailable.is_none()));
            self.show_header(None, false, false);
            self.set_text("");
            self.open_button.set_sensitive(can_open);
            self.show_empty(match unavailable {
                _ if opening => Empty::Opening,
                _ if !connected => Empty::Disconnected,
                Some(reason) => Empty::Unavailable {
                    reason,
                    activity: awaits_first_start(&summary),
                },
                None => Empty::CanOpen { note },
            });
            return;
        }
        self.show_problem(refusal);
        self.shown_empty.replace(None);
        self.body.set_visible_child_name("terminal");
        let status = session
            .as_ref()
            .map_or_else(String::new, |s| match &s.status {
                TerminalStatus::Opening => "Opening the sandboxed shell…".to_owned(),
                TerminalStatus::Open => s.mode.map_or("Open", TerminalMode::label).to_owned(),
                TerminalStatus::Ended(end) => end.message(),
            });
        self.show_header(Some(&status), !open, can_open);
        let hint = unavailable.or(note).unwrap_or_default();
        self.reopen_button
            .set_tooltip_text((!hint.is_empty()).then_some(hint.as_str()));
        set_accessible_description(&self.reopen_button, &hint);
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
        self.show_screen(session.as_ref(), open);
    }

    /// Shows the session's screen and, while it is open, keeps its cursor in view.
    fn show_screen(&self, session: Option<&TerminalSession>, open: bool) {
        let (text, cursor) = session.map(|s| s.screen.render()).unwrap_or_default();
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
