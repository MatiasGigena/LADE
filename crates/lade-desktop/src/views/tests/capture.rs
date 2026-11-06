use crate::presentation::controller::intents::RecordingSink;
use crate::presentation::state::model::ConnectionState;
use crate::presentation::state::model::{
    ActivityItem, ActivityItemId, ActivityRole, Basis, ChangeList, ChangeStatus, ChangedFile,
    DiffHunk, DiffLine, DiffLineKind, FileDiff, ObjectiveId, RepoPath, Timestamp,
};
use crate::presentation::state::store::{InspectorTab, MainView, PresentationState};
use crate::presentation::test_fixture;
use crate::views::tests::settle;
use crate::views::*;

/// How long each step lets GTK lay out and draw before it is checked.
const SETTLE: Duration = Duration::from_millis(400);

fn app(width: i32, height: i32) -> Rc<App> {
    load_css();
    let application = gtk::Application::builder()
        .application_id(format!(
            "dev.lade.Desktop.Capture{}",
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
    state.apply(StoreUpdate::Connection(ConnectionState::Connected));
    let app = App::new(
        &application,
        Store::new(state),
        Rc::new(RecordingSink::default()),
        Environment::default(),
    );
    app.window.set_default_size(width, height);
    app.present();
    settle(SETTLE);
    app
}

fn main_id() -> ObjectiveId {
    ObjectiveId::new(test_fixture::MAIN_OBJECTIVE)
}

fn populate(app: &App) {
    populate_changes(app);
    populate_diff(app);
    populate_activity(app);
}

/// A working Change Set of five files against the main Objective's target.
fn populate_changes(app: &App) {
    let target = app
        .store
        .state()
        .selected_detail()
        .unwrap()
        .integration_target
        .clone();
    let file = |path: &str, status, added, removed| ChangedFile {
        path: RepoPath::utf8(path),
        old_path: None,
        status,
        added,
        removed,
        binary: false,
        filtered: false,
    };
    app.store.apply(StoreUpdate::Changes(ChangeList {
        objective_id: main_id(),
        basis: Basis::Working,
        basis_label: "Working inspection".into(),
        target,
        capture_id: None,
        candidate_id: None,
        files: vec![
            file("src/net/retry.ts", ChangeStatus::Modified, 18, 6),
            file("src/net/errors.ts", ChangeStatus::Added, 24, 0),
            file("src/net/client.ts", ChangeStatus::Modified, 4, 3),
            file("test/retry.test.ts", ChangeStatus::Modified, 31, 2),
            file("docs/networking.md", ChangeStatus::Modified, 6, 1),
        ],
        truncated: false,
        total_added: 83,
        total_removed: 12,
        notices: vec![],
    }));
}

/// Selects `src/net/retry.ts` and shows its one-hunk diff.
fn populate_diff(app: &App) {
    let path = RepoPath::utf8("src/net/retry.ts");
    app.controller.select_file(&path);
    let rows: [(DiffLineKind, &str); 14] = [
        (DiffLineKind::Context, "import { fetch } from './client';"),
        (
            DiffLineKind::Add,
            "import { NetworkError, TimeoutError } from './errors';",
        ),
        (DiffLineKind::Context, ""),
        (
            DiffLineKind::Context,
            "export async function fetchWithRetry(url: string, attempts = 3) {",
        ),
        (
            DiffLineKind::Context,
            "  for (let attempt = 1; attempt <= attempts; attempt++) {",
        ),
        (DiffLineKind::Context, "    try {"),
        (DiffLineKind::Del, "      return await fetch(url);"),
        (DiffLineKind::Del, "    } catch (error) {"),
        (
            DiffLineKind::Add,
            "      return await fetch(url, { signal: AbortSignal.timeout(5_000) });",
        ),
        (DiffLineKind::Add, "    } catch (cause) {"),
        (DiffLineKind::Add, "      if (attempt === attempts) {"),
        (
            DiffLineKind::Add,
            "        throw isTimeout(cause) ? new TimeoutError(url, { cause }) : new NetworkError(url, { cause });",
        ),
        (DiffLineKind::Add, "      }"),
        (DiffLineKind::Context, "    }"),
    ];
    let (mut old_no, mut new_no) = (1, 1);
    let lines = rows
        .into_iter()
        .map(|(kind, text)| {
            let old = (kind != DiffLineKind::Add).then(|| {
                old_no += 1;
                old_no - 1
            });
            let new = (kind != DiffLineKind::Del).then(|| {
                new_no += 1;
                new_no - 1
            });
            DiffLine {
                kind,
                old_no: old,
                new_no: new,
                text: text.into(),
                new_byte_start: None,
                old_byte_start: None,
                no_newline: false,
            }
        })
        .collect::<Vec<_>>();
    app.store.apply(StoreUpdate::Diff(FileDiff {
        objective_id: main_id(),
        basis: Basis::Working,
        path,
        old_digest: None,
        new_digest: None,
        binary: false,
        hunks: vec![DiffHunk {
            old_start: 1,
            old_lines: old_no - 1,
            new_start: 1,
            new_lines: new_no - 1,
            lines,
        }],
        truncated: false,
    }));
}

/// A short transcript: the request, the plan, a test run and the result.
fn populate_activity(app: &App) {
    let item = |seq: u64, role, title: &str, text: &str, tool: Option<&str>| ActivityItem {
        item_id: ActivityItemId::new(format!("capture-{seq}")),
        seq,
        at: Timestamp(1_790_000_000_000 - (10 - i64::try_from(seq).unwrap()) * 60_000),
        role,
        title: title.into(),
        text: text.into(),
        tool_name: tool.map(str::to_owned),
        is_error: false,
        truncated: false,
    };
    app.store.apply(StoreUpdate::ActivityPage {
        objective_id: main_id(),
        items: vec![
            item(1, ActivityRole::User, "", "Make retries safe: distinguish a timeout from a confirmed failure.", None),
            item(2, ActivityRole::Assistant, "", "I'll add explicit error types in src/net/errors.ts and route the final attempt through them, keeping the fetchWithRetry signature.", None),
            item(3, ActivityRole::Tool, "", "npm test -- retry\n✓ 12 passed", Some("bash")),
            item(4, ActivityRole::Assistant, "", "Both paths now keep the original cause. Tests cover timeout and network failure.", None),
        ],
        has_more: false,
    });
}

fn shoot(window: &gtk::Window, dir: &std::path::Path, name: &str) {
    settle(SETTLE);
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let (width, height) = (window.width(), window.height());
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().expect("window paints");
    let renderer = window.native().unwrap().renderer().unwrap();
    let texture = renderer.render_texture(
        &node,
        Some(&gtk::graphene::Rect::new(
            0.0,
            0.0,
            f32::from(u16::try_from(width).unwrap()),
            f32::from(u16::try_from(height).unwrap()),
        )),
    );
    texture
        .save_to_png(dir.join(format!("{name}.png")))
        .unwrap();
}

#[test]
#[ignore = "writes screenshots to LADE_CAPTURE_DIR; requires a graphical display"]
fn capture_fixture_views() {
    let Ok(dir) = std::env::var("LADE_CAPTURE_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    gtk::test_synced(move || {
        for (width, height, suffix) in [(1440, 900, "wide"), (800, 550, "compact")] {
            let app = app(width, height);
            let window = app.window.clone().upcast::<gtk::Window>();
            shoot(&window, &dir, &format!("empty-{suffix}"));
            app.controller.select_objective(&main_id());
            populate(&app);
            settle(SETTLE);
            shoot(&window, &dir, &format!("workbench-{suffix}"));
            app.controller.set_inspector_tab(InspectorTab::Evidence);
            shoot(&window, &dir, &format!("evidence-{suffix}"));
            app.controller.set_inspector_tab(InspectorTab::Feedback);
            shoot(&window, &dir, &format!("feedback-{suffix}"));
            app.controller.set_inspector_tab(InspectorTab::Overview);
            app.controller.set_view(MainView::Activity);
            shoot(&window, &dir, &format!("activity-{suffix}"));
            app.controller.set_view(MainView::Workbench);
            if width > 1000 {
                dialogs::new_objective(&app.window, &app.controller);
                settle(SETTLE);
                let dialog = gtk::Window::list_toplevels()
                    .into_iter()
                    .filter_map(|w| w.downcast::<gtk::Window>().ok())
                    .find(|w| w.title().as_deref() == Some("New Objective"))
                    .expect("Objective form");
                shoot(&dialog, &dir, "new-objective");
                dialog.close();
            }
            app.window.close();
            settle(SETTLE);
        }
    });
}
