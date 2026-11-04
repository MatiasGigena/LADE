//! Keyboard shortcuts from the keymap, and moving focus between the window regions.

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::keys::{self, AppAction};
use crate::presentation::layout::{self, LayoutClass, Region};
use crate::presentation::state::model::{NoticeLevel, NoticeScope, ServiceNotice};
use crate::presentation::state::store::{MainView, StoreUpdate};

use super::{App, dialogs};

impl App {
    pub(super) fn load_keymap(&self) {
        let Some(path) = &self.keys_path else { return };
        let (keymap, problem) = keys::load(path);
        self.controller.set_keymap(keymap);
        if let Some(message) = problem {
            self.store.apply(StoreUpdate::Notice(ServiceNotice {
                level: NoticeLevel::Warning,
                code: "keybindings_ignored".to_owned(),
                message,
                scope: NoticeScope::General,
            }));
        }
    }

    pub(super) fn install_shortcuts(self: &Rc<Self>) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let keymap = self.store.state().ui.keymap.clone();
        let mut shortcuts = Vec::new();
        for binding in keys::DEFAULT_APP_KEYBINDINGS {
            if binding.action == AppAction::CloseDialog {
                continue;
            }
            let weak = Rc::downgrade(self);
            let action = binding.action;
            let callback = gtk::CallbackAction::new(move |_, _| match weak.upgrade() {
                Some(app) => app.run_action(action),
                None => glib::Propagation::Proceed,
            });
            let shortcut = gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(&keymap.gtk(action)),
                Some(callback),
            );
            controller.add_shortcut(shortcut.clone());
            shortcuts.push((action, shortcut));
        }
        self.window.add_controller(controller);
        *self.shortcuts.borrow_mut() = shortcuts;
        self.header.set_shortcut_hints(&keymap);
        self.terminal.set_shortcut_hints(&keymap);
    }

    pub(super) fn apply_keymap(&self) {
        let keymap = self.store.state().ui.keymap.clone();
        for (action, shortcut) in self.shortcuts.borrow().iter() {
            shortcut.set_trigger(gtk::ShortcutTrigger::parse_string(&keymap.gtk(*action)));
        }
        self.header.set_shortcut_hints(&keymap);
        self.terminal.set_shortcut_hints(&keymap);
    }

    fn run_action(self: &Rc<Self>, action: AppAction) -> glib::Propagation {
        let in_terminal = self.terminal.has_focus();
        if in_terminal && !keys::reaches_window_from_terminal(action) {
            return glib::Propagation::Proceed;
        }
        match action {
            AppAction::ShowTerminal => self.enter_terminal(),
            AppAction::LeaveTerminal | AppAction::TerminalCopy | AppAction::TerminalPaste
                if !in_terminal =>
            {
                return glib::Propagation::Proceed;
            }
            AppAction::LeaveTerminal => self.leave_terminal(),
            AppAction::TerminalCopy => self.terminal.copy(),
            AppAction::TerminalPaste => self.terminal.paste(),
            AppAction::Exit => self.header.request_exit(),
            AppAction::NextRegion => self.cycle_regions(true),
            AppAction::PreviousRegion => self.cycle_regions(false),
            AppAction::ShowWorkbench => self.controller.set_view(MainView::Workbench),
            AppAction::ShowActivity => self.controller.set_view(MainView::Activity),
            AppAction::FocusSearch => self.header.focus_search(),
            AppAction::AddFeedback => self.add_feedback(),
            AppAction::NewObjective => {
                if self.store.state().active_workspace.is_some() {
                    dialogs::new_objective(&self.rail.new_button, &self.controller);
                }
            }
            AppAction::OpenWorkspace => {
                dialogs::open_workspace(self.window.upcast_ref(), &self.controller);
            }
            AppAction::ToggleViewerMode => self.controller.toggle_viewer_mode(),
            AppAction::ShowShortcuts => self.header.show_shortcuts(),
            AppAction::SendMessage => {
                if !self.activity.composer_has_focus() {
                    return glib::Propagation::Proceed;
                }
                self.activity.send();
            }
            AppAction::CloseDialog => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn enter_terminal(&self) {
        if !self.terminal.has_focus() {
            let previous = gtk::prelude::GtkWindowExt::focus(&self.window);
            *self.before_terminal.borrow_mut() = previous.map(|w| w.downgrade());
        }
        self.controller.set_view(MainView::Terminal);
        let pane = self.terminal.clone();
        glib::idle_add_local_once(move || pane.focus());
    }

    fn leave_terminal(&self) {
        let previous = self
            .before_terminal
            .borrow_mut()
            .take()
            .and_then(|w| w.upgrade())
            .filter(|w| w.is_mapped() && w.is_focusable() && !w.is_ancestor(&self.terminal.root));
        match previous {
            Some(widget) => {
                widget.grab_focus();
            }
            None => self.focus_region(Region::Objectives),
        }
        if self.terminal.has_focus() {
            self.focus_region(Region::Header);
        }
    }

    pub(super) fn region_widget(&self, region: Region) -> gtk::Widget {
        match region {
            Region::Header => self.header.root.clone().upcast(),
            Region::Objectives => self.rail.root.clone().upcast(),
            Region::Main => self.centre_region.clone().upcast(),
            Region::Inspector => self.inspector.root.clone().upcast(),
        }
    }

    pub(super) fn focused_region(&self) -> Option<Region> {
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window)?;
        layout::REGION_ORDER.into_iter().find(|region| {
            let widget = self.region_widget(*region);
            focus == widget || focus.is_ancestor(&widget)
        })
    }

    pub(super) fn focus_region(&self, region: Region) {
        let target = self.region_widget(region);
        if !target.child_focus(gtk::DirectionType::TabForward) {
            target.grab_focus();
        }
    }

    fn cycle_regions(&self, forward: bool) {
        let compact = self.store.state().ui.layout == LayoutClass::Compact;
        let mut target = layout::next_region(self.focused_region(), forward);
        if compact {
            if target != Region::Header {
                self.controller.show_region(target);
            }
        } else {
            for _ in 0..layout::REGION_ORDER.len() {
                if self.region_widget(target).is_mapped() {
                    break;
                }
                target = layout::next_region(Some(target), forward);
            }
        }
        self.focus_region(target);
    }
}
