//! Closing the window or exiting: unsaved drafts are flushed or confirmed lost first.

use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::controller::intents::StopReason;
use crate::presentation::state::drafts::CLOSE_WAIT_MS;

use super::{App, dialogs, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseAfter {
    Window,
    Exit,
}

impl App {
    pub(super) fn confirm_exit(self: &Rc<Self>, opener: &gtk::Widget) {
        let controls = self.store.state().connection.is_connected();
        let body = if controls {
            "Exit stops the LADE service: running agents are stopped and recorded as interrupted, terminals end, and nothing resumes on its own when LADE starts again. To keep agents working in the background, close the window instead."
        } else {
            "This window does not control the LADE service, so Exit only closes the window. The service and its agents are not affected."
        };
        let weak = Rc::downgrade(self);
        dialogs::confirm(opener, "Exit LADE?", body, "Exit", controls, move || {
            if let Some(app) = weak.upgrade()
                && app.guard_unsaved(CloseAfter::Exit)
            {
                app.finish_close(CloseAfter::Exit);
            }
            Ok(())
        });
    }

    pub fn logout(&self) {
        self.controller.flush_drafts(widgets::monotonic_ms());
        self.controller.request_service_stop(StopReason::Logout);
    }

    pub(super) fn guard_unsaved(self: &Rc<Self>, after: CloseAfter) -> bool {
        if self.controller.unsaved_drafts().is_empty() {
            return true;
        }
        if self.close_pending.replace(Some(after)).is_some() {
            return false;
        }
        if self.store.state().connection.is_connected() {
            self.controller.flush_drafts(widgets::monotonic_ms());
            self.schedule_drafts();
            widgets::announce(
                &self.window,
                "Saving feedback drafts before closing",
                gtk::AccessibleAnnouncementPriority::Medium,
            );
            let weak = Rc::downgrade(self);
            let id =
                glib::timeout_add_local_once(Duration::from_millis(CLOSE_WAIT_MS), move || {
                    if let Some(app) = weak.upgrade() {
                        app.close_timer.borrow_mut().take();
                        app.confirm_loss();
                    }
                });
            if let Some(old) = self.close_timer.borrow_mut().replace(id) {
                old.remove();
            }
            self.check_pending_close();
        } else {
            self.confirm_loss();
        }
        false
    }

    pub(super) fn check_pending_close(self: &Rc<Self>) {
        let Some(after) = self.close_pending.get() else {
            return;
        };
        let unsaved = self.controller.unsaved_drafts();
        if unsaved.is_empty() {
            self.finish_close(after);
        } else if unsaved.iter().any(|d| !d.pending) && self.loss_dialog.borrow().is_none() {
            self.confirm_loss();
        }
    }

    fn confirm_loss(self: &Rc<Self>) {
        let Some(after) = self.close_pending.get() else {
            return;
        };
        if let Some(timer) = self.close_timer.borrow_mut().take() {
            timer.remove();
        }
        if self.loss_dialog.borrow().is_some() {
            return;
        }
        let unsaved = self.controller.unsaved_drafts();
        if unsaved.is_empty() {
            self.finish_close(after);
            return;
        }
        let connected = self.store.state().connection.is_connected();
        let weak = Rc::downgrade(self);
        let dialog = dialogs::unsaved_drafts(
            &self.window,
            after == CloseAfter::Exit,
            &unsaved,
            connected,
            move || {
                if let Some(app) = weak.upgrade() {
                    app.finish_close(after);
                }
            },
        );
        let weak = Rc::downgrade(self);
        dialog.connect_close_request(move |_| {
            if let Some(app) = weak.upgrade() {
                app.loss_dialog.borrow_mut().take();
                app.close_pending.set(None);
            }
            glib::Propagation::Proceed
        });
        *self.loss_dialog.borrow_mut() = Some(dialog);
    }

    fn finish_close(&self, after: CloseAfter) {
        self.close_pending.set(None);
        if let Some(timer) = self.close_timer.borrow_mut().take() {
            timer.remove();
        }
        let dialog = self.loss_dialog.borrow_mut().take();
        if let Some(dialog) = dialog {
            dialog.close();
        }
        match after {
            CloseAfter::Window => {
                self.close_confirmed.set(true);
                self.window.close();
            }
            CloseAfter::Exit => {
                if !self.controller.request_service_stop(StopReason::Exit) {
                    self.close_confirmed.set(true);
                    self.quit();
                }
            }
        }
    }

    pub(super) fn quit(&self) {
        match self.window.application() {
            Some(application) => application.quit(),
            None => self.window.close(),
        }
    }
}
