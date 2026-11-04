mod capture;
mod launch_qa;

use super::*;
use crate::presentation::controller::intents::RecordingSink;
use crate::presentation::state::model::ConnectionState;
use crate::presentation::state::store::PresentationState;
use crate::presentation::test_fixture;

/// Runs the default main context for `duration`, dispatching events as they arrive, so
/// layout, frame clock ticks and idle handlers queued by the step under test complete.
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
