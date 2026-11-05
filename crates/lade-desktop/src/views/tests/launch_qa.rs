use crate::presentation::layout::LayoutClass;
use crate::presentation::state::model::{
    Basis, ChangeList, ChangeStatus, ChangedFile, NoticeLevel, NoticeScope, ObjectiveId, RepoPath,
    ServiceNotice,
};
use crate::presentation::test_fixture;
use crate::views::tests::{app, descendants, laid_out, settle_until};
use crate::views::*;

#[test]
#[ignore = "requires a graphical display"]
fn launch_qa_compact_objective_form_keeps_actions_on_screen() {
    gtk::test_synced(|| {
        let app = app(800, 550);
        dialogs::new_objective(&app.window, &app.controller);
        let form = || {
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.title().as_deref() == Some("New Objective"))
        };
        settle_until("the Objective form opens", || {
            form().is_some_and(|w| laid_out(&w))
        });
        let dialog = form().expect("Objective form");
        let size = (dialog.width(), dialog.height());
        let create = descendants(dialog.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Create Objective"))
            .expect("Create Objective action");
        create.emit_clicked();
        let validation = || {
            descendants(dialog.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .find(|l| l.label().as_str() == "Give the Objective a title of 1–200 characters")
        };
        settle_until("the validation message is shown", || {
            validation().is_some_and(|l| laid_out(&l))
        });
        let error = validation().expect("validation message");
        let error_bounds = error.compute_bounds(&dialog).unwrap();
        let height = dialog.height();
        dialog.close();
        app.window.close();
        assert!(
            size.0 <= 800 && size.1 <= 550,
            "form is {size:?} on an 800×550 window"
        );
        assert!(
            error_bounds.y() >= 0.0
                && error_bounds.y() + error_bounds.height()
                    < f32::from(u16::try_from(height - 60).unwrap()),
            "validation is outside the visible form: {error_bounds:?} in height {height}"
        );
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn launch_qa_narrow_navigation_fits_requested_width() {
    gtk::test_synced(|| {
        let app = app(375, 812);
        let menu = || {
            descendants(app.window.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::MenuButton>().ok())
                .find(|b| b.icon_name().as_deref() == Some("open-menu-symbolic"))
        };
        settle_until("the compact header shows its menu", || {
            app.store.state().ui.layout == LayoutClass::Compact
                && menu().is_some_and(|m| laid_out(&m))
        });
        let width = app.window.width();
        let menu = menu().expect("header menu");
        let bounds = menu.compute_bounds(&app.window).unwrap();
        app.window.close();
        assert_eq!(width, 375, "navigation must not force a wider window");
        assert!(
            bounds.x() >= 0.0 && bounds.x() + bounds.width() <= 375.0,
            "header menu is clipped: {bounds:?}"
        );
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn launch_qa_narrow_change_set_heading_stays_readable() {
    gtk::test_synced(|| {
        let app = app(375, 812);
        app.controller
            .select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        let target = app
            .store
            .state()
            .selected_detail()
            .unwrap()
            .integration_target
            .clone();
        app.store.apply(StoreUpdate::Changes(ChangeList {
            objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE),
            basis: Basis::Working,
            basis_label: "Working inspection".into(),
            target,
            capture_id: None,
            candidate_id: None,
            files: vec![ChangedFile {
                path: RepoPath::utf8("hello.txt"),
                old_path: None,
                status: ChangeStatus::Added,
                added: 1,
                removed: 0,
                binary: false,
                filtered: false,
            }],
            truncated: false,
            total_added: 1,
            total_removed: 0,
            notices: vec![],
        }));
        let heading = || {
            descendants(app.window.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .find(|l| laid_out(l) && l.label().as_str() == "Change Set")
        };
        settle_until("the Change Set heading is shown", || heading().is_some());
        let heading = heading().expect("Change Set heading");
        let size = (heading.width(), heading.height());
        app.window.close();
        assert!(
            size.0 >= 75 && size.1 <= 40,
            "Change Set label was squeezed into {size:?}"
        );
    });
}

#[test]
#[ignore = "requires a graphical display"]
fn launch_qa_opening_workspace_resolves_its_previous_error() {
    gtk::test_synced(|| {
        let app = app(1440, 900);
        app.store.apply(StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Warning,
            code: "invalid_request".into(),
            message:
                "Couldn't open workspace. Path is not the top level of a non-bare Git repository."
                    .into(),
            scope: NoticeScope::WorkspaceOpen,
        }));
        assert!(app.notice.is_visible(), "failed open is visible");
        let settings = app.store.state().settings.clone();
        app.store.apply(StoreUpdate::Settings(settings));
        assert!(
            app.notice.is_visible(),
            "settings updates keep the failed open"
        );
        let workspace = app.store.state().active_workspace().unwrap().clone();
        app.store.apply(StoreUpdate::WorkspaceOpened(workspace));
        let stale = app.notice.is_visible();
        app.store.apply(StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Warning,
            code: "storage_pressure".into(),
            message: "Storage is running low.".into(),
            scope: NoticeScope::General,
        }));
        let workspace = app.store.state().active_workspace().unwrap().clone();
        app.store.apply(StoreUpdate::WorkspaceOpened(workspace));
        assert!(app.notice.is_visible(), "unrelated warnings remain visible");
        app.window.close();
        assert!(!stale, "a successful open still shows its previous failure");
    });
}
