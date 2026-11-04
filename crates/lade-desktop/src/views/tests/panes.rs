//! The window's panes over the shared fixture: what each shows for the selected Objective, and
//! how the frame, shortcuts, closing guard and dialogs behave.

use crate::presentation::controller::intents::{Intent, RecordingSink};
use crate::presentation::keys::{AppAction, Keymap};
use crate::presentation::state::model::{
    ActivityItem, ActivityItemId, ActivityRole, ConnectionState, ObjectiveId, Timestamp,
};
use crate::presentation::state::store::{InspectorTab, MainView, PresentationState};
use crate::presentation::test_fixture;
use crate::views::tests::{SETTLE, button_named, descendants, settle, shows};
use crate::views::*;

/// A presented, connected window over the fixture, with the main Objective selected, and the
/// intents it has sent.
fn connected(width: i32, height: i32) -> (Rc<App>, Rc<RecordingSink>) {
    load_css();
    let application = gtk::Application::builder()
        .application_id(format!(
            "dev.lade.Desktop.Panes{}",
            glib::uuid_string_random().replace('-', "")
        ))
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application
        .register(None::<&gtk::gio::Cancellable>)
        .unwrap();
    let mut state = PresentationState::new(ConnectionState::Connected);
    for update in test_fixture::fixture_updates() {
        state.apply(update);
    }
    state.connection = ConnectionState::Connected;
    let sink = Rc::new(RecordingSink::default());
    let app = App::new(&application, Store::new(state), sink.clone());
    app.window.set_default_size(width, height);
    app.present();
    app.controller
        .select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    settle(SETTLE);
    (app, sink)
}

fn toplevel(title: &str) -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.is_visible() && w.title().as_deref() == Some(title))
}

#[test]
#[ignore = "requires a graphical display"]
fn the_rail_lists_objectives_and_selecting_one_shows_it_in_the_workbench() {
    gtk::test_synced(|| {
        let (app, sink) = connected(1440, 900);
        assert!(shows(&app.rail.root, "Make retries safe"));
        assert!(shows(&app.rail.root, "Explain interrupted work"));
        assert!(shows(&app.workbench.root, "Make retries safe"));
        sink.intents.borrow_mut().clear();
        app.controller
            .select_objective(&ObjectiveId::new(test_fixture::SECOND_OBJECTIVE));
        settle(SETTLE);
        assert!(shows(&app.workbench.root, "Explain interrupted work"));
        assert!(
            sink.intents
                .borrow()
                .iter()
                .any(|i| matches!(i, Intent::LoadObjective { .. }))
        );
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn the_header_names_the_connection_and_switches_the_main_view() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        assert!(shows(&app.header.root, "Connected"));
        app.store
            .apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
                attempt: 2,
            }));
        settle(SETTLE);
        assert!(shows(&app.header.root, "Reconnecting"));
        assert_eq!(
            header::connection_text(&ConnectionState::Disconnected {
                reason: "closed".into()
            }),
            (
                "○ Disconnected".to_owned(),
                "Disconnected — closed".to_owned()
            )
        );
        app.controller.set_view(MainView::Activity);
        settle(SETTLE);
        assert!(app.activity.root.is_mapped());
        assert!(!app.workbench.root.is_mapped());
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn the_inspector_tabs_show_the_objective_its_checks_and_its_drafts() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        assert!(shows(&app.inspector.root, "Harden network calls"));
        app.controller.set_inspector_tab(InspectorTab::Evidence);
        settle(SETTLE);
        assert_eq!(app.inspector.root.current_page(), Some(1));
        assert!(shows(&app.inspector.root, "npm test"));
        app.controller.set_inspector_tab(InspectorTab::Feedback);
        settle(SETTLE);
        assert_eq!(app.inspector.root.current_page(), Some(2));
        assert!(!shows(&app.inspector.root, "npm test"));
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn activity_states_what_the_agent_is_doing() {
    gtk::test_synced(|| {
        let (app, sink) = connected(1440, 900);
        app.controller.set_view(MainView::Activity);
        settle(SETTLE);
        assert!(shows(
            &app.activity.root,
            "The agent is idle and waiting for your message."
        ));
        assert!(
            sink.intents
                .borrow()
                .iter()
                .any(|i| matches!(i, Intent::LoadActivity { .. })),
            "showing activity loads it"
        );
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn the_terminal_pane_offers_to_open_a_terminal_for_the_objective() {
    gtk::test_synced(|| {
        let (app, sink) = connected(1440, 900);
        app.controller.set_view(MainView::Terminal);
        settle(SETTLE);
        let open = button_named(&app.terminal.root, "Open terminal").expect("open action");
        sink.intents.borrow_mut().clear();
        open.emit_clicked();
        settle(SETTLE);
        assert!(
            sink.intents
                .borrow()
                .iter()
                .any(|i| matches!(i, Intent::OpenTerminal { .. }))
        );
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn a_compact_window_shows_the_region_switches() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        assert!(!app.region_bar.is_visible());
        app.apply_width(800);
        settle(SETTLE);
        assert!(app.region_bar.is_visible());
        assert_eq!(
            app.store.state().ui.layout,
            crate::presentation::layout::LayoutClass::Compact
        );
        app.apply_width(1440);
        settle(SETTLE);
        assert!(!app.region_bar.is_visible());
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn every_rebindable_action_has_a_shortcut_that_follows_the_keymap() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        let installed: Vec<AppAction> = app.shortcuts.borrow().iter().map(|(a, _)| *a).collect();
        let expected: Vec<AppAction> = keys::DEFAULT_APP_KEYBINDINGS
            .iter()
            .map(|b| b.action)
            .filter(|a| *a != AppAction::CloseDialog)
            .collect();
        assert_eq!(installed, expected);
        let entries: Vec<(AppAction, String)> = keys::DEFAULT_APP_KEYBINDINGS
            .iter()
            .map(|b| match b.action {
                AppAction::ShowActivity => (b.action, "F9".to_owned()),
                action => (action, b.default.to_owned()),
            })
            .collect();
        let keymap = Keymap::propose(&entries).expect("F9 is free");
        app.controller.set_keymap(keymap);
        settle(SETTLE);
        let trigger = app
            .shortcuts
            .borrow()
            .iter()
            .find(|(a, _)| *a == AppAction::ShowActivity)
            .and_then(|(_, s)| s.trigger())
            .map(|t| t.to_str().to_string());
        assert_eq!(trigger.as_deref(), Some("F9"));
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn closing_without_unsaved_text_needs_no_confirmation() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        let unsaved = !app.controller.unsaved_drafts().is_empty();
        assert_eq!(app.guard_unsaved(CloseAfter::Window), !unsaved);
        assert_eq!(app.loss_dialog.borrow().is_some(), unsaved);
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn a_confirmation_closes_on_success_and_keeps_a_refusal_in_view() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        let attempts = Rc::new(Cell::new(0));
        let count = attempts.clone();
        dialogs::confirm(
            &app.window,
            "Discard Objective",
            "Gone for good.",
            "Discard",
            true,
            move || {
                count.set(count.get() + 1);
                if count.get() == 1 {
                    Err("The service refused".into())
                } else {
                    Ok(())
                }
            },
        );
        settle(SETTLE);
        let window = toplevel("Discard Objective").expect("confirmation");
        let discard = button_named(&window, "Discard").expect("confirm action");
        assert!(discard.has_css_class("destructive"));
        discard.emit_clicked();
        settle(SETTLE);
        assert!(window.is_visible(), "a refusal keeps the dialog open");
        assert!(shows(&window, "The service refused"));
        discard.emit_clicked();
        settle(SETTLE);
        assert!(!window.is_visible());
        assert_eq!(attempts.get(), 2);
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn the_session_sections_show_the_model_and_the_resources_awaiting_approval() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        let window = gtk::Window::new();
        let content = widgets::vbox(0);
        content.append(&session::model_picker(&app.controller));
        content.append(&session::resources_section(&app.controller));
        window.set_child(Some(&content));
        window.present();
        settle(SETTLE);
        assert!(
            shows(&content, "ask.js"),
            "{:?}",
            tests::shown_text(content.upcast_ref())
        );
        assert!(
            descendants(content.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::DropDown>().ok())
                .any(|d| d.is_mapped()),
            "a provider or model choice"
        );
        window.close();
        app.window.close();
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn the_activity_log_shows_history_and_streamed_text() {
    gtk::test_synced(|| {
        let (app, _) = connected(1440, 900);
        app.controller.set_view(MainView::Activity);
        let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
        app.store.apply(StoreUpdate::ActivityPage {
            objective_id: id.clone(),
            items: vec![ActivityItem {
                item_id: ActivityItemId::new("item-1"),
                seq: 1,
                at: Timestamp(1_790_000_000_000),
                role: ActivityRole::Assistant,
                title: String::new(),
                text: "Reading retry.ts".into(),
                tool_name: None,
                is_error: false,
                truncated: false,
            }],
            has_more: false,
        });
        settle(SETTLE);
        assert!(shows(&app.activity.root, "Reading retry.ts"));
        app.store.apply(StoreUpdate::ActivityDelta {
            objective_id: id,
            item_id: ActivityItemId::new("item-1"),
            append_text: " and its tests".into(),
        });
        settle(SETTLE);
        assert!(shows(&app.activity.root, "Reading retry.ts and its tests"));
        app.window.close();
    });
}
