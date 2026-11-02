use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::dialogs;
use super::viewer::Viewer;
use super::widgets::{
    self, MessageKind, hbox, inline_message, label, set_accessible_label, set_focus_key, space,
    vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::format::{self, JOURNEY_STEPS, StepState};
use crate::presentation::model::{Basis, ChangedFile, Load, RepoPath};
use crate::presentation::panel;
use crate::presentation::store::{Change, PresentationState};

/// The work area's state for a Workspace without Objectives: one instruction and the New
/// Objective action (the same dialog as the Objectives list's). The inspector stays blank.
pub fn first_objective_state(controller: &Rc<Controller>) -> gtk::Widget {
    let create = widgets::button("New Objective", &["primary"]);
    set_focus_key(&create, "centre:new-objective");
    create.set_sensitive(controller.store.state().connection.is_connected());
    let opener = controller.clone();
    create.connect_clicked(move |b| dialogs::new_objective(b, &opener));
    widgets::empty_state(
        "Create your first Objective",
        "Describe an outcome; the agent pursues it on its own branch and worktree, and you \
         review the result here.",
        Some(&create),
        None,
    )
}

/// The Workspace has no Objectives at all (none active, none archived).
pub fn board_is_empty(state: &PresentationState) -> bool {
    let board = state.board();
    board.groups.iter().all(|(_, o)| o.is_empty()) && board.archive.is_empty()
}

pub struct Workbench {
    pub root: gtk::Stack,
    controller: Rc<Controller>,
    /// The page without a selected Objective (first Objective, or select one).
    none: gtk::Box,
    /// What the page shows: (no Objectives at all, connected).
    shown_none: Cell<Option<(bool, bool)>>,
    /// The file list and viewer, or the empty Change Set in their place.
    body: gtk::Box,
    no_changes: gtk::Box,
    heading: gtk::Box,
    basis: gtk::DropDown,
    bases: RefCell<Vec<Basis>>,
    change_heading: gtk::Box,
    version: gtk::Box,
    files: gtk::ListBox,
    file_paths: RefCell<Vec<RepoPath>>,
    files_status: gtk::Box,
    files_scroll: gtk::ScrolledWindow,
    pub viewer: Rc<Viewer>,
    updating: Cell<bool>,
    pub title_label: RefCell<Option<gtk::Label>>,
    details_toggle: gtk::ToggleButton,
    details_widgets: RefCell<Vec<gtk::Widget>>,
    show_details: Cell<bool>,
    compact: Cell<bool>,
    compact_status: RefCell<Option<gtk::Widget>>,
}

impl Workbench {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = gtk::Stack::new();
        root.add_css_class("centre");
        let none = vbox(0);
        none.set_vexpand(true);
        root.add_named(&none, Some("none"));

        let page = vbox(space::PAGE);
        page.add_css_class("centre-page");
        let top = hbox(space::SECTION);
        let heading = vbox(space::ITEM);
        heading.set_hexpand(true);
        top.append(&heading);
        let details_toggle = gtk::ToggleButton::with_label("Objective details");
        details_toggle.set_valign(gtk::Align::Start);
        details_toggle.set_tooltip_text(Some("Show or hide the collaboration journey"));
        details_toggle.set_visible(false);
        top.append(&details_toggle);
        page.append(&top);

        let change_set = vbox(space::GROUP);
        change_set.set_vexpand(true);
        let change_row = hbox(space::SECTION);
        let change_heading = hbox(space::GROUP);
        change_heading.set_hexpand(true);
        change_row.append(&change_heading);
        let basis = gtk::DropDown::from_strings(&["Working inspection"]);
        basis.add_css_class("basis-selector");
        basis.set_tooltip_text(Some("Inspected version"));
        set_accessible_label(&basis, "Inspected version");
        basis.set_valign(gtk::Align::Center);
        change_row.append(&basis);
        change_set.append(&change_row);
        let version = vbox(space::ITEM);
        change_set.append(&version);

        let body = hbox(space::SECTION);
        body.set_vexpand(true);
        let file_column = vbox(space::ITEM);
        let files_status = vbox(space::ITEM);
        file_column.append(&files_status);
        let files = gtk::ListBox::new();
        files.add_css_class("file-list");
        files.set_selection_mode(gtk::SelectionMode::Single);
        set_accessible_label(&files, "Changed files");
        let files_scroll = widgets::region_scroll(&files);
        files_scroll.set_vexpand(true);
        files_scroll.set_size_request(192, -1);
        files_scroll.add_css_class("file-scroll");
        file_column.append(&files_scroll);
        body.append(&file_column);
        let viewer = Viewer::new(controller);
        viewer.root.set_hexpand(true);
        body.append(&viewer.root);
        change_set.append(&body);
        let no_changes = vbox(0);
        no_changes.set_vexpand(true);
        no_changes.set_visible(false);
        change_set.append(&no_changes);
        page.append(&change_set);
        root.add_named(&page, Some("objective"));

        let workbench = Rc::new(Self {
            root,
            controller: controller.clone(),
            none,
            shown_none: Cell::new(None),
            body,
            no_changes,
            heading,
            basis,
            bases: RefCell::new(vec![Basis::Working]),
            change_heading,
            version,
            files,
            file_paths: RefCell::new(Vec::new()),
            files_status,
            files_scroll,
            viewer,
            updating: Cell::new(false),
            title_label: RefCell::new(None),
            details_toggle,
            details_widgets: RefCell::new(Vec::new()),
            show_details: Cell::new(true),
            compact: Cell::new(false),
            compact_status: RefCell::new(None),
        });
        workbench.connect();
        workbench.render_all();
        workbench
    }

    fn connect(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.details_toggle.connect_toggled(move |t| {
            if let Some(this) = this.upgrade()
                && !this.updating.get()
            {
                this.controller.set_compact_details(t.is_active());
            }
        });
        let this = Rc::downgrade(self);
        self.basis.connect_selected_notify(move |d| {
            if let Some(this) = this.upgrade()
                && !this.updating.get()
            {
                let basis = this.bases.borrow().get(d.selected() as usize).cloned();
                if let Some(basis) = basis {
                    this.controller.select_basis(basis);
                }
            }
        });
        let this = Rc::downgrade(self);
        self.files.connect_row_selected(move |_, row| {
            if let Some(this) = this.upgrade()
                && !this.updating.get()
                && let Some(row) = row
            {
                let path = usize::try_from(row.index())
                    .ok()
                    .and_then(|i| this.file_paths.borrow().get(i).cloned());
                if let Some(path) = path {
                    this.controller.select_file(&path);
                }
            }
        });
    }

    pub fn refresh(&self, changes: &[Change]) {
        let selected = self.controller.store.state().ui.selected.clone();
        let mine = |id: &crate::presentation::model::ObjectiveId| Some(id) == selected.as_ref();
        let mut heading = false;
        let mut change_set = false;
        for change in changes {
            match change {
                Change::Selection | Change::Connection => {
                    heading = true;
                    change_set = true;
                }
                Change::Detail(id) | Change::Inputs(id) | Change::Drafts(id) if mine(id) => {
                    heading = true;
                }
                Change::Changes(id) if mine(id) => change_set = true,
                Change::Objectives if selected.is_none() => heading = true,
                Change::Viewer => self.sync_file_selection(),
                _ => {}
            }
        }
        if heading {
            self.render_heading();
            self.render_basis();
        }
        if change_set {
            widgets::rebuild_preserving_focus(&self.files.clone(), || self.render_change_set());
        }
        self.viewer.refresh(changes);
    }

    fn render_all(&self) {
        self.render_heading();
        self.render_basis();
        self.render_change_set();
    }

    pub fn set_objective_details(&self, shown: bool, toggle_visible: bool) {
        self.show_details.set(shown);
        self.compact.set(toggle_visible);
        if let Some(status) = self.compact_status.borrow().as_ref() {
            status.set_visible(toggle_visible);
        }
        self.details_toggle.set_visible(toggle_visible);
        self.updating.set(true);
        self.details_toggle.set_active(shown);
        self.updating.set(false);
        for widget in self.details_widgets.borrow().iter() {
            widget.set_visible(shown);
        }
    }

    /// The page without a selected Objective: the first-Objective state when the Workspace has
    /// none, otherwise one line pointing to the list (rebuilt only when that changes).
    fn render_none(&self, board_empty: bool) {
        let connected = self.controller.store.state().connection.is_connected();
        // The connection decides whether New Objective is available.
        let key = (board_empty, connected);
        if self.shown_none.replace(Some(key)) == Some(key) {
            return;
        }
        widgets::clear(&self.none);
        self.none.append(&if board_empty {
            first_objective_state(&self.controller)
        } else {
            let create = widgets::button("New Objective", &[]);
            set_focus_key(&create, "centre:new-objective");
            create.set_sensitive(connected);
            let opener = self.controller.clone();
            create.connect_clicked(move |b| dialogs::new_objective(b, &opener));
            widgets::empty_state(
                "Select an Objective",
                "Choose one from Objectives to inspect its Change Set, evidence and feedback, \
                 or start a new one.",
                Some(&create),
                None,
            )
        });
    }

    pub fn set_file_list_width(&self, width: i32) {
        self.files_scroll.set_size_request(width, -1);
    }

    fn render_heading(&self) {
        widgets::clear(&self.heading);
        let mut details: Vec<gtk::Widget> = Vec::new();
        let state = self.controller.store.state();
        let Some(summary) = state.selected_summary().cloned() else {
            let empty = board_is_empty(&state);
            drop(state);
            self.render_none(empty);
            self.root.set_visible_child_name("none");
            return;
        };
        self.shown_none.set(None);
        self.root.set_visible_child_name("objective");
        let detail = state.selected_detail().cloned();
        let has_inputs = state
            .inputs
            .get(&summary.objective_id)
            .is_some_and(|i| !i.is_empty());
        drop(state);

        let text = vbox(space::TIGHT);
        let title = widgets::heading(&summary.title, "objective-heading");
        set_focus_key(&title, "centre:title");
        title.set_focusable(true);
        title.set_selectable(true);
        text.append(&title);
        *self.title_label.borrow_mut() = Some(title);
        self.heading.append(&text);
        let (sign, status_text) = panel::status_line(&summary);
        let status = widgets::sign_text(sign, &status_text);
        status.add_css_class("meta");
        set_accessible_label(&status, &format!("Status: {status_text}"));
        status.set_visible(self.compact.get());
        self.heading.append(&status);
        *self.compact_status.borrow_mut() = Some(status.upcast());

        if let Some(detail) = detail {
            let steps = format::journey(&detail, has_inputs);
            let journey = hbox(space::ITEM);
            journey.add_css_class("journey");
            let mut spoken = Vec::new();
            for (index, (name, step)) in JOURNEY_STEPS.iter().zip(steps).enumerate() {
                if index > 0 {
                    let connector = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    connector.add_css_class("journey-connector");
                    connector.set_valign(gtk::Align::Center);
                    journey.append(&connector);
                }
                let item = hbox(space::ITEM);
                let dot = gtk::Label::new(Some(match step {
                    StepState::Done => "✓",
                    StepState::Current | StepState::Upcoming => "",
                }));
                dot.add_css_class("journey-dot");
                dot.add_css_class(match step {
                    StepState::Done => "done",
                    StepState::Current => "current",
                    StepState::Upcoming => "upcoming",
                });
                dot.set_valign(gtk::Align::Center);
                item.append(&dot);
                let text = gtk::Label::new(Some(name));
                text.add_css_class("journey-label");
                if step == StepState::Current {
                    text.add_css_class("current");
                }
                item.append(&text);
                journey.append(&item);
                spoken.push(format!(
                    "{name} {}",
                    match step {
                        StepState::Done => "done",
                        StepState::Current => "current",
                        StepState::Upcoming => "upcoming",
                    }
                ));
            }
            set_accessible_label(
                &journey,
                &format!("Collaboration journey: {}", spoken.join(", ")),
            );
            journey.set_tooltip_text(Some(
                "Explains where the collaboration stands; it is not an automatic pipeline.",
            ));
            self.heading.append(&journey);
            details.push(journey.upcast());
        }
        let shown = self.show_details.get();
        for widget in &details {
            widget.set_visible(shown);
        }
        *self.details_widgets.borrow_mut() = details;
    }

    fn render_basis(&self) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.clone() else {
            return;
        };
        let detail = state.detail(&id).cloned();
        let current = state
            .selected_ui()
            .map_or(Basis::Working, |u| u.basis.clone());
        drop(state);
        let mut bases = vec![Basis::Working];
        let mut labels = vec!["Working inspection".to_owned()];
        if let Some(detail) = &detail {
            let mut candidates = detail.candidates.clone();
            candidates.sort_by_key(|c| std::cmp::Reverse(c.ordinal));
            for c in &candidates {
                bases.push(Basis::Candidate {
                    candidate_id: c.candidate_id.clone(),
                });
                labels.push(format!(
                    "Candidate {} ({})",
                    c.ordinal,
                    format::candidate_status(c.status)
                ));
            }
            if let [newest, previous, ..] = candidates.as_slice() {
                let since = Basis::Since {
                    from_candidate_id: previous.candidate_id.clone(),
                    to_candidate_id: newest.candidate_id.clone(),
                };
                labels.push(format::basis_label(&since, Some(detail)));
                bases.push(since);
            }
        }
        if !bases.contains(&current) {
            labels.push(format::basis_label(&current, detail.as_ref()));
            bases.push(current.clone());
        }
        self.updating.set(true);
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.basis.set_model(Some(&gtk::StringList::new(&refs)));
        if let Some(index) = bases.iter().position(|b| b == &current) {
            self.basis.set_selected(index as u32);
        }
        *self.bases.borrow_mut() = bases;
        self.updating.set(false);
    }

    fn render_change_set(&self) {
        widgets::clear(&self.change_heading);
        widgets::clear(&self.version);
        widgets::clear(&self.files_status);
        self.files_status.set_visible(false);
        widgets::clear(&self.no_changes);
        self.no_changes.set_visible(false);
        self.body.set_visible(true);
        self.updating.set(true);
        while let Some(row) = self.files.row_at_index(0) {
            self.files.remove(&row);
        }
        self.file_paths.borrow_mut().clear();
        self.updating.set(false);
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.clone() else {
            return;
        };
        let detail = state.detail(&id).cloned();
        let ui = state.selected_ui().cloned().unwrap_or_default();
        let working_updated = state.working_updated.contains(&id);
        let changes = state.changes.get(&id).cloned().unwrap_or(Load::NotLoaded);
        drop(state);

        let title = widgets::section_title("Change Set");
        self.change_heading.append(&title);
        match &changes {
            Load::NotLoaded | Load::Loading => {
                self.files_status.set_visible(true);
                self.files_status
                    .append(&label("Loading Change Set…", &["meta"]));
                return;
            }
            Load::Failed(message) => {
                // A plain sentence ("Couldn't load Change Set. …"), never an error code.
                self.files_status.set_visible(true);
                self.files_status
                    .append(&inline_message(MessageKind::Error, message, None));
                let retry = widgets::button("Retry", &[]);
                let controller = self.controller.clone();
                retry.connect_clicked(move |_| controller.refresh_changes());
                self.files_status.append(&retry);
                return;
            }
            Load::Loaded(_) => {}
        }
        let Load::Loaded(list) = changes else { return };
        let n = list.files.len();
        // An empty Change Set says so once, below, instead of "0 files changed +0 −0".
        if n > 0 {
            let summary = hbox(space::GROUP);
            summary.set_valign(gtk::Align::Center);
            let files_label = label(
                &format!("{n} file{} changed", if n == 1 { "" } else { "s" }),
                &["meta"],
            );
            files_label.set_wrap(false);
            summary.append(&files_label);
            summary.append(&widgets::counts(list.total_added, list.total_removed));
            self.change_heading.append(&summary);
        }

        let status = match &ui.basis {
            Basis::Candidate { candidate_id } => detail
                .as_ref()
                .and_then(|d| {
                    d.candidates
                        .iter()
                        .find(|c| &c.candidate_id == candidate_id)
                })
                .map(|c| c.status),
            _ => None,
        };
        let (short, full) =
            format::version_line(&ui.basis, &list.basis_label, status, &list.target);
        let version_label = label(&short, &["meta", "version-label"]);
        version_label.set_hexpand(true);
        version_label.set_tooltip_text(Some(&full));
        set_accessible_label(&version_label, &format!("Inspected version: {full}"));
        self.version.append(&version_label);
        if matches!(ui.basis, Basis::Since { .. }) {
            self.version.append(&label(
                "The revision delta may include target refresh effects; it does not attribute lines to feedback.",
                &["meta"],
            ));
        }
        if working_updated && ui.basis == Basis::Working {
            let row = hbox(space::GROUP);
            let message = inline_message(
                MessageKind::Warning,
                "The worktree changed after this capture. Your selection and drafts stay on the inspected version.",
                None,
            );
            message.set_hexpand(true);
            row.append(&message);
            let refresh = widgets::button("Refresh", &[]);
            refresh.set_valign(gtk::Align::Center);
            let controller = self.controller.clone();
            refresh.connect_clicked(move |_| controller.refresh_changes());
            row.append(&refresh);
            self.version.append(&row);
        }

        for notice in &list.notices {
            let text = match &notice.path {
                Some(path) => format!("{}: {}", path.display, notice.message),
                None => notice.message.clone(),
            };
            self.version
                .append(&inline_message(MessageKind::Warning, &text, None));
        }
        if list.files.is_empty() {
            // One empty state in place of the file list and the viewer (no toolbar, no frame).
            let (title, body) = match ui.basis {
                Basis::Working => ("No changes yet", "The agent's edits appear here."),
                Basis::Candidate { .. } => (
                    "No changes",
                    "This candidate has no changes against the target.",
                ),
                Basis::Since { .. } => ("No changes", "Nothing changed between these candidates."),
            };
            self.body.set_visible(false);
            self.no_changes
                .append(&widgets::empty_state(title, body, None, None));
            self.no_changes.set_visible(true);
        }
        self.updating.set(true);
        for file in &list.files {
            self.files.append(&file_row(file));
            self.file_paths.borrow_mut().push(file.path.clone());
        }
        self.updating.set(false);
        self.sync_file_selection();
    }

    fn sync_file_selection(&self) {
        let selected = self
            .controller
            .store
            .state()
            .selected_ui()
            .and_then(|u| u.file.clone());
        let index = selected.and_then(|p| self.file_paths.borrow().iter().position(|f| f == &p));
        self.updating.set(true);
        match index.and_then(|i| self.files.row_at_index(i32::try_from(i).ok()?)) {
            Some(row) => self.files.select_row(Some(&row)),
            None => self.files.unselect_all(),
        }
        self.updating.set(false);
    }
}

fn file_row(file: &ChangedFile) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-row");
    set_focus_key(&row, &format!("file:{}", file.path.display));
    let content = hbox(space::ITEM);
    let (symbol, status) = crate::presentation::format::file_status(file.status);
    let symbol_label = gtk::Label::new(Some(symbol));
    symbol_label.add_css_class("file-status");
    symbol_label.add_css_class("mono");
    symbol_label.set_valign(gtk::Align::Start);
    symbol_label.set_tooltip_text(Some(status));
    content.append(&symbol_label);
    let text = vbox(0);
    text.set_hexpand(true);
    let (directory, name) = match file.path.display.rsplit_once('/') {
        Some((directory, name)) => (Some(directory), name),
        None => (None, file.path.display.as_str()),
    };
    text.append(&label(name, &["file-name"]));
    let second = hbox(space::ITEM);
    let directory = widgets::line_label(
        directory.unwrap_or_default(),
        &["meta", "file-directory"],
        gtk::pango::EllipsizeMode::Start,
    );
    directory.set_tooltip_text(None);
    directory.set_hexpand(true);
    second.append(&directory);
    let detail = if file.binary {
        label("binary", &["meta"]).upcast::<gtk::Widget>()
    } else {
        widgets::counts(file.added, file.removed).upcast::<gtk::Widget>()
    };
    second.append(&detail);
    text.append(&second);
    if file.filtered {
        text.append(&label("filtered (LFS) — raw bytes, unsupported", &["meta"]));
    }
    content.append(&text);
    row.set_child(Some(&content));
    let mut spoken = format!("{}, {status}", file.path.display);
    if let Some(old) = &file.old_path {
        spoken.push_str(&format!(", from {}", old.display));
    }
    if file.binary {
        spoken.push_str(", binary");
    } else {
        spoken.push_str(&format!(", {} added, {} removed", file.added, file.removed));
    }
    if file.filtered {
        spoken.push_str(", filtered content compared as raw bytes, unsupported");
    }
    set_accessible_label(&row, &spoken);
    row.set_tooltip_text(Some(&file.path.display));
    row
}
