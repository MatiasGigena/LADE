mod capture;
mod launch_qa;
mod panes;
mod widgets;

use std::time::Instant;

use super::*;
use crate::presentation::controller::intents::RecordingSink;
use crate::presentation::state::model::ConnectionState;
use crate::presentation::state::store::PresentationState;
use crate::presentation::test_fixture;

/// How long a step may take to reach its expected state before the test fails. Generous, so a
/// loaded display server does not fail a test; a step that is ready sooner does not wait for it.
const PATIENCE: Duration = Duration::from_secs(10);

pub(super) fn app(width: i32, height: i32) -> Rc<App> {
    load_css();
    let application = gtk::Application::builder()
        .application_id(format!(
            "dev.lade.Desktop.LaunchQa{}",
            glib::uuid_string_random().replace('-', "")
        ))
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application
        .register(None::<&gtk::gio::Cancellable>)
        .unwrap();
    let mut state = PresentationState::default();
    for update in test_fixture::fixture_updates() {
        state.apply(update);
    }
    let app = App::new(
        &application,
        Store::new(state),
        Rc::new(RecordingSink::default()),
    );
    app.window.set_default_size(width, height);
    app.present();
    settle_until("the window is laid out", || laid_out(&app.window));
    app
}

/// Whether `widget` is mapped and has been given a size.
pub(super) fn laid_out(widget: &impl IsA<gtk::Widget>) -> bool {
    let widget = widget.as_ref();
    widget.is_mapped() && widget.width() > 0 && widget.height() > 0
}

pub(super) fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        found.extend(descendants(&widget));
    }
    found
}

/// The text of every mapped label under `widget`, in tree order.
pub(super) fn shown_text(widget: &gtk::Widget) -> Vec<String> {
    descendants(widget)
        .into_iter()
        .filter(gtk::Widget::is_mapped)
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Whether some mapped label under `widget` contains `needle`.
pub(super) fn shows(widget: &impl IsA<gtk::Widget>, needle: &str) -> bool {
    shown_text(widget.as_ref())
        .iter()
        .any(|t| t.contains(needle))
}

/// The mapped button under `widget` whose label or accessible face reads `text`.
pub(super) fn button_named(widget: &impl IsA<gtk::Widget>, text: &str) -> Option<gtk::Button> {
    descendants(widget.as_ref())
        .into_iter()
        .filter(gtk::Widget::is_mapped)
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| {
            b.label().as_deref() == Some(text)
                || b.child()
                    .and_downcast::<gtk::Label>()
                    .is_some_and(|l| l.text() == text)
        })
}

/// Runs the default main context, dispatching layout, frame clock ticks and idle handlers, until
/// `condition` holds. Panics naming `what` if it still does not hold after [`PATIENCE`].
pub(super) fn settle_until(what: &str, mut condition: impl FnMut() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + PATIENCE;
    // Wakes the blocking iteration regularly so the condition is checked even when idle.
    let tick = glib::timeout_add_local(Duration::from_millis(10), || glib::ControlFlow::Continue);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        context.iteration(true);
    }
    tick.remove();
}

/// Runs the default main context for `duration` whatever the state. Only for screenshot
/// capture, which has no condition to wait for beyond the drawing having finished.
pub(super) fn settle(duration: Duration) {
    let context = glib::MainContext::default();
    let elapsed = Rc::new(Cell::new(false));
    let flag = elapsed.clone();
    glib::timeout_add_local_once(duration, move || flag.set(true));
    while !elapsed.get() {
        context.iteration(true);
    }
}

#[test]
#[ignore = "requires a graphical display"]
fn discarding_unsaved_feedback_closes_its_confirmation_without_reentrant_borrows() {
    gtk::test_synced(|| {
        let application = gtk::Application::builder()
            .application_id("dev.lade.Desktop.CloseTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application
            .register(None::<&gtk::gio::Cancellable>)
            .unwrap();
        let mut state = PresentationState::default();
        for update in test_fixture::fixture_updates() {
            state.apply(update);
        }
        state.connection = ConnectionState::Disconnected {
            reason: "test disconnect".into(),
        };
        let app = App::new(
            &application,
            Store::new(state),
            Rc::new(RecordingSink::default()),
        );
        app.controller.edit_draft(
            &DraftId::new(test_fixture::SAVED_DRAFT),
            "Unsaved feedback before closing",
            1_000,
        );
        assert!(!app.guard_unsaved(CloseAfter::Window));
        let confirmation = app.loss_dialog.borrow().clone().expect("loss confirmation");
        assert!(confirmation.is_visible());
        let mut widgets = vec![confirmation.clone().upcast::<gtk::Widget>()];
        let mut discarded = false;
        while let Some(widget) = widgets.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.label().as_deref() == Some("Close and discard unsaved text")
            {
                button.emit_clicked();
                discarded = true;
                break;
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                widgets.push(widget);
            }
        }
        assert!(discarded, "discard action was available");
        assert!(app.loss_dialog.borrow().is_none());
        assert!(!confirmation.is_visible());
        assert!(app.close_confirmed.get());
    });
}
