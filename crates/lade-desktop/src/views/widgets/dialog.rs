//! A modal dialog with an error area, a button row and the close shortcut.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::keys;

use super::{announce, button, clear, error_line, hbox, heading, no_hyphens, space, vbox};

fn install_close_shortcut(window: &gtk::Window) {
    let controller = gtk::ShortcutController::new();
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

pub struct Dialog {
    pub window: gtk::Window,
    pub body: gtk::Box,
    pub buttons: gtk::Box,
    pub error: gtk::Box,
    scroll: gtk::ScrolledWindow,
}

impl Dialog {
    pub fn new(opener: &impl IsA<gtk::Widget>, title: &str, width: i32) -> Self {
        let opener = opener.as_ref().clone();
        let parent = opener.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let available_width = parent.as_ref().map_or(width, |w| (w.width() - 32).max(280));
        let available_height = parent.as_ref().map_or(800, |w| w.height().max(320));
        let window = gtk::Window::builder()
            .modal(true)
            .title(title)
            .default_width(width.min(available_width))
            .resizable(true)
            .build();
        window.set_transient_for(parent.as_ref());
        window.add_css_class("lade-dialog");
        let outer = vbox(0);
        let content = vbox(space::SECTION);
        content.add_css_class("dialog-content");
        content.append(&heading(title, "dialog-title"));
        let error = vbox(0);
        error.set_visible(false);
        content.append(&error);
        let body = vbox(space::GROUP);
        content.append(&body);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height((available_height - 144).max(160))
            .vexpand(true)
            .child(&content)
            .build();
        outer.append(&scroll);
        let buttons = hbox(space::ITEM);
        buttons.add_css_class("dialog-footer");
        buttons.set_halign(gtk::Align::Fill);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        buttons.append(&spacer);
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
            scroll,
        }
    }

    pub fn show_error(&self, message: &str) {
        self.show_errors(&[message.to_owned()]);
    }

    pub fn show_errors(&self, messages: &[String]) {
        clear(&self.error);
        self.error.set_visible(!messages.is_empty());
        self.error.set_spacing(space::ITEM);
        for message in messages {
            self.error.append(&error_line(message));
        }
        let scroll = self.scroll.clone();
        glib::idle_add_local_once(move || scroll.vadjustment().set_value(0.0));
        announce(
            &self.window,
            &messages.join(" "),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
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
        if let Some(label) = b.child().and_downcast::<gtk::Label>() {
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            no_hyphens(&label);
        }
        self.buttons.append(&b);
        b
    }

    pub fn present(&self) {
        self.window.present();
    }
}
