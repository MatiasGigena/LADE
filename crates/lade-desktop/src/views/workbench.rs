//! Centre Workbench: Objective heading, journey indicator, basis selector and the Change Set with
//! its changed-file list and viewer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::viewer::Viewer;
use super::widgets::{self, hbox, label, set_accessible_label, set_focus_key, vbox};
use crate::presentation::controller::Controller;
use crate::presentation::format::{self, JOURNEY_STEPS, StepState};
use crate::presentation::model::{Basis, CandidateStatus, ChangedFile, Load, RepoPath, Timestamp};
use crate::presentation::store::Change;

pub struct Workbench {
    pub root: gtk::Stack,
    controller: Rc<Controller>,
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
    /// Compact composition: collapses the outcome, status details, breadcrumb and journey so
    /// the Change Set stays dominant. Persistent (not rebuilt) so it keeps focus when toggled.
    details_toggle: gtk::ToggleButton,
    details_widgets: RefCell<Vec<gtk::Widget>>,
    show_details: Cell<bool>,
}

impl Workbench {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = gtk::Stack::new();
        root.add_css_class("centre");
        let none = vbox(12);
        none.add_css_class("centre-empty");
        none.append(&widgets::heading("Select an Objective", "empty-heading"));
        none.append(&label(
            "Choose an Objective from the list to inspect its Change Set, evidence and feedback.",
            &["secondary"],
        ));
        root.add_named(&none, Some("none"));

        let page = vbox(24);
        page.add_css_class("centre-page");
        let top = hbox(16);
        let heading = vbox(8);
        heading.set_hexpand(true);
        top.append(&heading);
        let details_toggle = gtk::ToggleButton::with_label("Objective details");
        details_toggle.set_valign(gtk::Align::Start);
        details_toggle.set_tooltip_text(Some(
            "Show or hide the outcome, status details and collaboration journey",
        ));
        details_toggle.set_visible(false);
        top.append(&details_toggle);
        page.append(&top);

        let change_set = vbox(12);
        change_set.set_vexpand(true);
        let change_row = hbox(16);
        let change_heading = hbox(12);
        change_heading.set_hexpand(true);
        change_row.append(&change_heading);
        let basis = gtk::DropDown::from_strings(&["Working inspection"]);
        basis.add_css_class("basis-selector");
        basis.set_tooltip_text(Some("Inspected version"));
        set_accessible_label(&basis, "Inspected version");
        basis.set_valign(gtk::Align::Center);
        change_row.append(&basis);
        change_set.append(&change_row);
        let version = vbox(8);
        change_set.append(&version);

        let body = hbox(16);
        body.set_vexpand(true);
        let file_column = vbox(8);
        let files_status = vbox(8);
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
        page.append(&change_set);
        root.add_named(&page, Some("objective"));

        let workbench = Rc::new(Self {
            root,
            controller: controller.clone(),
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

    /// Wide: details always shown, no toggle. Compact: the toggle decides.
    pub fn set_objective_details(&self, shown: bool, toggle_visible: bool) {
        self.show_details.set(shown);
        self.details_toggle.set_visible(toggle_visible);
        self.updating.set(true);
        self.details_toggle.set_active(shown);
        self.updating.set(false);
        for widget in self.details_widgets.borrow().iter() {
            widget.set_visible(shown);
        }
    }

    pub fn set_file_list_width(&self, width: i32) {
        self.files_scroll.set_size_request(width, -1);
    }

    fn render_heading(&self) {
        widgets::clear(&self.heading);
        let mut details: Vec<gtk::Widget> = Vec::new();
        let state = self.controller.store.state();
        let Some(summary) = state.selected_summary().cloned() else {
            self.root.set_visible_child_name("none");
            return;
        };
        self.root.set_visible_child_name("objective");
        let detail = state.selected_detail().cloned();
        let has_inputs = state
            .inputs
            .get(&summary.objective_id)
            .is_some_and(|i| !i.is_empty());
        drop(state);

        let breadcrumb = hbox(6);
        breadcrumb.add_css_class("breadcrumb");
        breadcrumb.append(&label("Objectives", &["meta"]));
        breadcrumb.append(&label("/", &["meta"]));
        let crumb = widgets::line_label(
            &summary.title,
            &["breadcrumb-current"],
            gtk::pango::EllipsizeMode::End,
        );
        breadcrumb.append(&crumb);
        set_accessible_label(&breadcrumb, &format!("Objectives / {}", summary.title));
        self.heading.append(&breadcrumb);
        details.push(breadcrumb.upcast());

        let title_row = hbox(16);
        let text = vbox(4);
        text.set_hexpand(true);
        let title = widgets::heading(&summary.title, "objective-heading");
        set_focus_key(&title, "centre:title");
        title.set_focusable(true);
        title.set_selectable(true);
        text.append(&title);
        *self.title_label.borrow_mut() = Some(title);
        let outcome = label(&summary.outcome, &["secondary", "outcome-line"]);
        text.append(&outcome);
        details.push(outcome.upcast());
        title_row.append(&text);
        let status = vbox(4);
        status.set_halign(gtk::Align::End);
        let pill = widgets::status_pill(summary.lifecycle, summary.attention);
        pill.set_halign(gtk::Align::End);
        status.append(&pill);
        let updated = label(
            &format!(
                "Updated {}",
                format::relative_time(Timestamp(widgets::now_ms()), summary.updated_at)
            ),
            &["meta"],
        );
        updated.set_xalign(1.0);
        updated.set_wrap(false);
        status.append(&updated);
        details.push(updated.upcast());
        if !summary.badges.is_empty() {
            let badges = label(&summary.badge_text(), &["meta"]);
            badges.set_xalign(1.0);
            status.append(&badges);
            details.push(badges.upcast());
        }
        title_row.append(&status);
        self.heading.append(&title_row);

        if let Some(detail) = detail {
            let steps = format::journey(&detail, has_inputs);
            let journey = hbox(8);
            journey.add_css_class("journey");
            journey.set_margin_top(8);
            let mut spoken = Vec::new();
            for (index, (name, step)) in JOURNEY_STEPS.iter().zip(steps).enumerate() {
                if index > 0 {
                    let connector = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    connector.add_css_class("journey-connector");
                    connector.set_valign(gtk::Align::Center);
                    connector.set_hexpand(true);
                    journey.append(&connector);
                }
                let item = hbox(8);
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
                // Non-wrapping: a wrapping label in this row reports inconsistent heights to GTK
                // (a measure critical, as found in the IMP-002 checkpoint).
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

        let title = widgets::section_title("Change set");
        self.change_heading.append(&title);
        match &changes {
            Load::NotLoaded | Load::Loading => {
                self.files_status
                    .append(&label("Loading Change Set…", &["meta"]));
                return;
            }
            Load::Failed(message) => {
                self.files_status.append(&widgets::error_line(&format!(
                    "Change Set unavailable — {message}"
                )));
                let retry = widgets::button("Retry", &[]);
                let controller = self.controller.clone();
                retry.connect_clicked(move |_| controller.refresh_changes());
                self.files_status.append(&retry);
                return;
            }
            Load::Loaded(_) => {}
        }
        let Load::Loaded(list) = changes else { return };
        let summary = hbox(12);
        summary.set_valign(gtk::Align::Center);
        let n = list.files.len();
        let files_label = label(
            &format!("{n} file{} changed", if n == 1 { "" } else { "s" }),
            &["meta"],
        );
        files_label.set_wrap(false);
        summary.append(&files_label);
        summary.append(&widgets::counts(list.total_added, list.total_removed));
        self.change_heading.append(&summary);

        let line = hbox(8);
        line.add_css_class("version-line");
        let kind = match &ui.basis {
            Basis::Working => "working capture — may change",
            Basis::Candidate { candidate_id } => {
                match detail
                    .as_ref()
                    .and_then(|d| {
                        d.candidates
                            .iter()
                            .find(|c| &c.candidate_id == candidate_id)
                    })
                    .map(|c| c.status)
                {
                    Some(CandidateStatus::Historical) => "frozen candidate — historical",
                    Some(CandidateStatus::Invalidated) => "frozen candidate — invalidated",
                    _ => "frozen candidate",
                }
            }
            Basis::Since { .. } => "comparing two frozen candidates",
        };
        let version_text = format!(
            "{} · {kind} · compared with {}",
            list.basis_label,
            list.target.label()
        );
        let version_label = label(&version_text, &["meta", "version-label"]);
        version_label.set_hexpand(true);
        line.append(&version_label);
        set_accessible_label(&line, &format!("Inspected version: {version_text}"));
        self.version.append(&line);
        if matches!(ui.basis, Basis::Since { .. }) {
            self.version.append(&label(
                "The revision delta may include target refresh effects; it does not attribute lines to feedback.",
                &["meta"],
            ));
        }
        if working_updated && ui.basis == Basis::Working {
            let row = hbox(12);
            row.append(&widgets::warning_line(
                "The worktree changed after this capture. Your selection and drafts stay on the inspected version.",
            ));
            let refresh = widgets::button("Refresh", &[]);
            refresh.set_valign(gtk::Align::Center);
            let controller = self.controller.clone();
            refresh.connect_clicked(move |_| controller.refresh_changes());
            row.append(&refresh);
            self.version.append(&row);
        }

        if list.files.is_empty() {
            self.files_status
                .append(&label("No changes against the target.", &["meta"]));
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
    let content = hbox(8);
    let (symbol, status) = crate::presentation::format::file_status(file.status);
    let symbol_label = gtk::Label::new(Some(symbol));
    symbol_label.add_css_class("file-status");
    symbol_label.add_css_class("mono");
    symbol_label.set_valign(gtk::Align::Start);
    symbol_label.set_tooltip_text(Some(status));
    content.append(&symbol_label);
    let text = vbox(2);
    let path = label(&file.path.display, &["file-name"]);
    text.append(&path);
    let detail = if file.binary {
        let l = label("binary", &["meta"]);
        l.upcast::<gtk::Widget>()
    } else {
        widgets::counts(file.added, file.removed).upcast::<gtk::Widget>()
    };
    text.append(&detail);
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
    set_accessible_label(&row, &spoken);
    row.set_tooltip_text(Some(&file.path.display));
    row
}
