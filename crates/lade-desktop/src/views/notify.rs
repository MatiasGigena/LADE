//! Desktop notifications through `GApplication` (IMP-012). The in-app board stays the source of
//! truth (`presentation::attention`); this is only a second channel.
//!
//! Availability is checked once per window on the session bus: without a bus, or without an
//! owner of `org.freedesktop.Notifications` / `org.gtk.Notifications`, every request fails with
//! a reason and the window shows one in-app notice instead. `GApplication` reports no delivery
//! errors itself, so a server that accepts and then drops a notification is not detectable here.

use std::cell::OnceCell;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::attention::{AttentionRequest, Notifier};

/// The application action a notification activates; its target is the Objective id.
pub const OPEN_OBJECTIVE_ACTION: &str = "open-objective";

pub struct DesktopNotifier {
    app: gtk::Application,
    available: OnceCell<Result<(), String>>,
}

impl DesktopNotifier {
    pub fn new(app: &gtk::Application) -> Self {
        Self {
            app: app.clone(),
            available: OnceCell::new(),
        }
    }

    fn check() -> Result<(), String> {
        let bus = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
            .map_err(|_| "no desktop session bus".to_owned())?;
        let owned = |name: &str| {
            bus.call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                Some(&(name,).to_variant()),
                Some(glib::VariantTy::new("(b)").expect("static type string")),
                gio::DBusCallFlags::NONE,
                500,
                None::<&gio::Cancellable>,
            )
            .ok()
            .and_then(|reply| reply.get::<(bool,)>())
            .is_some_and(|(owned,)| owned)
        };
        if owned("org.freedesktop.Notifications") || owned("org.gtk.Notifications") {
            Ok(())
        } else {
            Err("no notification service is running".to_owned())
        }
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, request: &AttentionRequest) -> Result<(), String> {
        self.available.get_or_init(Self::check).clone()?;
        if !self.app.is_registered() {
            return Err("the application is not registered on the session bus".to_owned());
        }
        let notification = gio::Notification::new(&request.title);
        notification.set_body(Some(&request.body));
        notification.set_priority(gio::NotificationPriority::Normal);
        // Activation navigates only (see `App::open_from_notification`).
        notification.set_default_action_and_target_value(
            &format!("app.{OPEN_OBJECTIVE_ACTION}"),
            Some(&request.objective_id.as_str().to_variant()),
        );
        self.app.send_notification(
            Some(&format!("objective-{}", request.objective_id.as_str())),
            &notification,
        );
        Ok(())
    }
}
