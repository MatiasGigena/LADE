use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::dialogs;
use super::session;
use super::widgets::{
    self, MessageKind, action_row, field, hbox, inline_message, label, set_accessible_description,
    set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::drafts::SaveStatus;
use crate::presentation::format;
use crate::presentation::model::{
    ActionAvailability, ActionName, ActionSurface, CandidateStatus, DeliveryMode, DraftId,
    NoticeLevel, ObjectiveDetail, ObjectiveId, Timestamp, VerificationState,
};
use crate::presentation::panel::{self, PanelAction, Tone};
use crate::presentation::store::{Change, InspectorTab, MainView};
use crate::presentation::text_bound::TEXT_LIMIT;

const FEEDBACK_MODES: [DeliveryMode; 2] = [DeliveryMode::Steer, DeliveryMode::FollowUp];

struct DraftWidgets {
    root: gtk::Box,
    reference: gtk::Label,
    excerpt: gtk::Label,
    stale: gtk::Box,
    view: gtk::TextView,
    input: Rc<widgets::LimitedInput>,
    status: gtk::Label,
    actions: gtk::Box,
}

pub struct Inspector {
    pub root: gtk::Notebook,
    controller: Rc<Controller>,
    overview: gtk::Box,
    evidence: gtk::Box,
    drafts_box: gtk::Box,
    drafts_heading: gtk::Box,
    drafts_refusal: gtk::Box,
    submit_box: gtk::Box,
    drafts: RefCell<HashMap<DraftId, DraftWidgets>>,
    last_error: RefCell<Option<String>>,
    check_error: RefCell<Option<String>>,
    check_entry: gtk::Entry,
    updating: Cell<bool>,
    on_draft_edit: RefCell<Option<Rc<dyn Fn()>>>,
    dialogs: session::DialogHost,
    more_open: Cell<bool>,
    details_open: Cell<bool>,
}

fn page(name: &str) -> (gtk::ScrolledWindow, gtk::Box) {
    let content = vbox(space::REGION);
    content.add_css_class("inspector-page");
    let scroll = widgets::region_scroll(&content);
    scroll.set_propagate_natural_width(false);
    scroll.set_vexpand(true);
    set_accessible_label(&scroll, name);
    (scroll, content)
}

fn section(title: &str) -> gtk::Box {
    let section = vbox(space::GROUP);
    section.append(&widgets::section_title(title));
    section
}

fn group_separator() -> gtk::Separator {
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.add_css_class("list-separator");
    separator
}

impl Inspector {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = gtk::Notebook::new();
        root.add_css_class("inspector");
        root.set_show_border(false);
        let (overview_scroll, overview) = page("Overview");
        let (evidence_scroll, evidence) = page("Evidence");
        let (feedback_scroll, feedback) = page("Feedback");
        root.append_page(&overview_scroll, Some(&gtk::Label::new(Some("Overview"))));
        root.append_page(&evidence_scroll, Some(&gtk::Label::new(Some("Evidence"))));
        root.append_page(&feedback_scroll, Some(&gtk::Label::new(Some("Feedback"))));
        for scroll in [&overview_scroll, &evidence_scroll, &feedback_scroll] {
            root.page(scroll).set_tab_expand(true);
        }

        let drafts_heading = vbox(space::TIGHT);
        feedback.append(&drafts_heading);
        let drafts_refusal = vbox(0);
        feedback.append(&drafts_refusal);
        let drafts_box = vbox(space::SECTION);
        feedback.append(&drafts_box);
        let submit_box = vbox(space::GROUP);
        feedback.append(&submit_box);

        let check_entry = gtk::Entry::builder()
            .placeholder_text("Command, e.g. npm test")
            .build();
        check_entry.add_css_class("mono");
        set_accessible_label(&check_entry, "Check command");
        set_focus_key(&check_entry, "evidence:command");

        let inspector = Rc::new(Self {
            root,
            controller: controller.clone(),
            overview,
            evidence,
            drafts_box,
            drafts_heading,
            drafts_refusal,
            submit_box,
            drafts: RefCell::new(HashMap::new()),
            last_error: RefCell::new(None),
            check_error: RefCell::new(None),
            check_entry,
            updating: Cell::new(false),
            on_draft_edit: RefCell::new(None),
            dialogs: session::DialogHost::default(),
            more_open: Cell::new(false),
            details_open: Cell::new(false),
        });
        let this = Rc::downgrade(&inspector);
        inspector.root.connect_switch_page(move |_, _, index| {
            if let Some(this) = this.upgrade()
                && !this.updating.get()
            {
                this.controller.set_inspector_tab(match index {
                    1 => InspectorTab::Evidence,
                    2 => InspectorTab::Feedback,
                    _ => InspectorTab::Overview,
                });
            }
        });
        inspector.render_overview();
        inspector.render_evidence();
        inspector.reconcile_drafts();
        inspector.render_drafts_refusal();
        inspector.render_submit();
        inspector.sync_tab();
        inspector
    }

    pub fn set_on_draft_edit(&self, handler: Rc<dyn Fn()>) {
        *self.on_draft_edit.borrow_mut() = Some(handler);
    }

    pub fn refresh(self: &Rc<Self>, changes: &[Change]) {
        let selected = self.controller.store.state().ui.selected.clone();
        let mine = |id: &crate::presentation::model::ObjectiveId| Some(id) == selected.as_ref();
        let (mut overview, mut evidence, mut drafts, mut submit) = (false, false, false, false);
        for change in changes {
            match change {
                Change::Selection => {
                    self.last_error.replace(None);
                    self.check_error.replace(None);
                    (overview, evidence, drafts, submit) = (true, true, true, true);
                }
                Change::Catalog => overview = true,
                Change::Connection | Change::Workspaces => {
                    (overview, evidence, submit) = (true, true, true);
                }
                Change::Detail(id) if mine(id) => {
                    (overview, evidence, submit) = (true, true, true);
                    self.render_drafts_refusal();
                }
                Change::Drafts(id) if mine(id) => {
                    drafts = true;
                    submit = true;
                }
                Change::DraftStatus(id) => {
                    self.render_draft_status(id);
                    submit = true;
                }
                Change::InspectorTab => self.sync_tab(),
                Change::Dialogs => self.dialogs.sync(&self.root, &self.controller),
                _ => {}
            }
        }
        if overview {
            widgets::rebuild_preserving_focus(&self.overview.clone(), || self.render_overview());
        }
        if evidence {
            widgets::rebuild_preserving_focus(&self.evidence.clone(), || self.render_evidence());
        }
        if drafts {
            self.reconcile_drafts();
            self.render_drafts_refusal();
        }
        if submit {
            widgets::rebuild_preserving_focus(&self.submit_box.clone(), || self.render_submit());
        }
    }

    fn sync_tab(&self) {
        let tab = self.controller.store.state().ui.inspector_tab;
        self.updating.set(true);
        self.root.set_current_page(Some(match tab {
            InspectorTab::Overview => 0,
            InspectorTab::Evidence => 1,
            InspectorTab::Feedback => 2,
        }));
        self.updating.set(false);
    }

    fn report(self: &Rc<Self>, result: Result<(), String>) {
        if Self::keep_error(&self.last_error, result) {
            widgets::rebuild_preserving_focus(&self.overview.clone(), || self.render_overview());
        }
    }

    fn report_check(self: &Rc<Self>, result: Result<(), String>) {
        if Self::keep_error(&self.check_error, result) {
            widgets::rebuild_preserving_focus(&self.evidence.clone(), || self.render_evidence());
        }
    }

    fn keep_error(slot: &RefCell<Option<String>>, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => slot.replace(None).is_some(),
            Err(message) => {
                slot.replace(Some(message));
                true
            }
        }
    }

    fn refusal(&self, objective: &ObjectiveId, surface: ActionSurface) -> Option<gtk::Box> {
        let state = self.controller.store.state();
        let refusal = state.last_refusal(objective, surface)?;
        Some(inline_message(
            MessageKind::Warning,
            &refusal.message,
            Some(&refusal.details),
        ))
    }

    fn command_note(&self, objective: &ObjectiveId) -> Option<gtk::Label> {
        let state = self.controller.store.state();
        let note = state
            .command_note(objective)
            .filter(|n| n.level == NoticeLevel::Info)?;
        Some(label(&note.message, &["meta"]))
    }

    fn render_drafts_refusal(&self) {
        widgets::clear(&self.drafts_refusal);
        let selected = self.controller.store.state().ui.selected.clone();
        if let Some(message) = selected.and_then(|o| self.refusal(&o, ActionSurface::Drafts)) {
            self.drafts_refusal.append(&message);
        }
        self.drafts_refusal
            .set_visible(self.drafts_refusal.first_child().is_some());
    }

    fn detail(&self) -> Option<ObjectiveDetail> {
        self.controller.store.state().selected_detail().cloned()
    }

    fn render_overview(self: &Rc<Self>) {
        widgets::clear(&self.overview);
        let selected = self.controller.store.state().ui.selected.is_some();
        self.root.set_show_tabs(selected);
        let Some(detail) = self.detail() else {
            if selected {
                self.overview
                    .append(&label("Loading Objective…", &["meta"]));
            }
            return;
        };
        let summary = &detail.summary;

        let next_area = vbox(space::ITEM);
        next_area.add_css_class("next-action");
        if let Some(approval) = &detail.approval {
            next_area.append(&label(format::approval_label(approval.state), &["meta"]));
        }
        let blocker = panel::start_blocker(self.controller.store.state().compat.catalog.as_ref());
        let pointer = panel::agent_pointer(&detail, blocker.as_deref());
        if let Some(pointer) = &pointer {
            let row = vbox(space::TIGHT);
            row.append(&label(pointer, &["meta"]));
            let open = widgets::button("Open Activity", &["quiet", "list-action-button"]);
            open.set_halign(gtk::Align::Start);
            set_accessible_description(&open, pointer);
            set_focus_key(&open, "overview:open-activity");
            let controller = self.controller.clone();
            open.connect_clicked(move |_| controller.set_view(MainView::Activity));
            row.append(&open);
            next_area.append(&row);
        }
        let next = panel::next_action(&detail);
        match &next {
            Some(next) => next_area.append(&self.action_widget(&detail, next, true)),
            None if !summary.lifecycle.is_archived() && pointer.is_none() => {
                next_area.append(&label("No step is available right now.", &["meta"]))
            }
            None => {}
        }
        if let Some(error) = self.last_error.borrow().as_ref() {
            next_area.append(&inline_message(MessageKind::Error, error, None));
        }
        for surface in [ActionSurface::Review, ActionSurface::Closing] {
            if let Some(refusal) = self.refusal(&summary.objective_id, surface) {
                next_area.append(&refusal);
            }
        }
        if let Some(note) = self.command_note(&summary.objective_id) {
            next_area.append(&note);
        }
        if next_area.first_child().is_some() {
            self.overview.append(&next_area);
        }

        let (regular, destructive) = panel::more_actions(&detail, next.as_ref());
        if !regular.is_empty() || !destructive.is_empty() || detail.history_deleted {
            let list = vbox(space::TIGHT);
            for action in &regular {
                list.append(&self.action_widget(&detail, action, false));
            }
            if !destructive.is_empty() || detail.history_deleted {
                if !regular.is_empty() {
                    list.append(&group_separator());
                }
                for action in &destructive {
                    list.append(&self.action_widget(&detail, action, false));
                }
                if detail.history_deleted {
                    list.append(&label(
                        "Git history deleted: retention references and branch were removed on request.",
                        &["meta"],
                    ));
                }
            }
            let this = Rc::downgrade(self);
            self.overview.append(&widgets::disclosure(
                panel::MORE_ACTIONS_TITLE,
                self.more_open.get(),
                &list,
                move |open| {
                    if let Some(this) = this.upgrade() {
                        this.more_open.set(open);
                    }
                },
            ));
        }

        let intent = section("Intent");
        for (key, value) in [
            ("Outcome", &detail.intent.outcome),
            ("Constraints", &detail.intent.constraints),
            ("Acceptance", &detail.intent.acceptance),
        ] {
            let block = vbox(space::TIGHT);
            block.append(&label(key, &["meta"]));
            block.append(&label(
                if value.is_empty() { "—" } else { value },
                &["prose"],
            ));
            intent.append(&block);
        }
        intent.append(&label(
            &format!("Intent revision {}", detail.intent_revision),
            &["meta"],
        ));
        let this = Rc::downgrade(self);
        intent.append(&widgets::list_action_row(
            "Edit intent",
            &detail.availability(ActionName::ObjectiveUpdateIntent),
            false,
            move |b| {
                if let Some(this) = this.upgrade() {
                    dialogs::edit_intent(b, &this.controller);
                }
            },
        ));
        if let Some(refusal) = self.refusal(&summary.objective_id, ActionSurface::Intent) {
            intent.append(&refusal);
        }
        self.overview.append(&intent);

        let rows = vbox(space::ITEM);
        for row in panel::details_rows(&detail, Timestamp(widgets::now_ms())) {
            let value = if row.mono {
                widgets::line_label(&row.value, &["mono"], gtk::pango::EllipsizeMode::Middle)
            } else {
                label(&row.value, &[])
            };
            if let Some(full) = &row.full {
                value.set_tooltip_text(Some(full));
                set_accessible_description(&value, full);
            }
            rows.append(&field(row.key, &value));
        }
        let this = Rc::downgrade(self);
        self.overview.append(&widgets::disclosure(
            panel::DETAILS_TITLE,
            self.details_open.get(),
            &rows,
            move |open| {
                if let Some(this) = this.upgrade() {
                    this.details_open.set(open);
                }
            },
        ));
    }

    fn action_widget(
        self: &Rc<Self>,
        detail: &ObjectiveDetail,
        action: &PanelAction,
        prominent: bool,
    ) -> gtk::Box {
        let this = Rc::downgrade(self);
        let name = action.action;
        let on_click = move |b: &gtk::Button| {
            if let Some(this) = this.upgrade() {
                this.activate(name, b);
            }
        };
        let destructive = action.tone == Tone::Destructive;
        let enabled = action.availability == ActionAvailability::Enabled;
        let note = match name {
            ActionName::ReviewApprove if enabled && self.controller.approval_needs_override() => {
                Some(action.note.clone().unwrap_or_else(|| {
                    "Checks missing or not all passed: approval needs a recorded override reason."
                        .to_owned()
                }))
            }
            _ => action.note.clone(),
        }
        .filter(|_| enabled);
        let row = if prominent {
            let classes: &[&str] = match action.tone {
                Tone::Primary if note.is_none() => &["primary"],
                Tone::Destructive => &["destructive"],
                Tone::Primary | Tone::Neutral => &[],
            };
            action_row(&action.label, &action.availability, classes, on_click)
        } else {
            widgets::list_action_row(&action.label, &action.availability, destructive, on_click)
        };
        if let Some(note) = note {
            row.append(&label(&note, &["meta", "list-action-reason"]));
        }
        if !prominent {
            return row;
        }
        match name {
            ActionName::ReviewIntegrate => row.append(&label(
                &format!(
                    "One squash commit on {} from the approved candidate and target commit — a separate, explicit step.",
                    detail.integration_target.branch
                ),
                &["meta"],
            )),
            ActionName::ObjectiveCleanup => row.append(&label(
                "Removes the Objective's branch and worktree; records and evidence stay.",
                &["meta"],
            )),
            _ => {}
        }
        row
    }

    fn activate(self: &Rc<Self>, action: ActionName, button: &gtk::Button) {
        let Some(detail) = self.detail() else { return };
        let controller = self.controller.clone();
        match action {
            ActionName::ReviewFreeze => self.report(controller.freeze()),
            ActionName::ReviewRefresh => self.report(controller.refresh_candidate()),
            ActionName::ReviewReturn => dialogs::confirm(
                button,
                "Return for revision",
                "The current candidate becomes historical and any approval or override is invalidated before the agent may change the work again.",
                "Return for revision",
                false,
                move || controller.return_for_revision(),
            ),
            ActionName::ReviewApprove => {
                if controller.approval_needs_override() {
                    dialogs::approve_with_override(button, &controller);
                } else {
                    self.report(controller.approve(None));
                }
            }
            ActionName::ReviewIntegrate => {
                let target = detail.integration_target.label();
                dialogs::confirm(
                    button,
                    "Integrate approved candidate",
                    &format!(
                        "Create a squash commit on {target} from the approved candidate. The target must be unchanged and its checkout clean."
                    ),
                    "Integrate",
                    false,
                    move || controller.integrate(),
                );
            }
            ActionName::ObjectiveComplete => dialogs::confirm(
                button,
                "Complete without changes",
                "The Objective is completed with no repository change.",
                "Complete",
                false,
                move || controller.complete(),
            ),
            ActionName::ObjectiveDiscard => {
                let title = detail.summary.title.clone();
                dialogs::confirm(
                    button,
                    "Discard Objective",
                    &format!(
                        "“{title}” will be abandoned without integration and its agent stopped. Branch and worktree stay until you clean up."
                    ),
                    "Discard",
                    true,
                    move || controller.discard(),
                );
            }
            ActionName::ObjectiveCleanup => {
                dialogs::cleanup(button, &controller, &detail.branch, detail.unsent_drafts);
            }
            ActionName::ObjectiveDeleteHistory => {
                dialogs::delete_history(button, &controller, &detail.summary.title);
            }
            ActionName::ObjectiveUpdateIntent => dialogs::edit_intent(button, &controller),
            ActionName::ExecutionStart
            | ActionName::ExecutionCancel
            | ActionName::ExecutionResume
            | ActionName::MessageSend
            | ActionName::FeedbackSubmit
            | ActionName::ReviewVerify => {}
        }
    }

    fn render_evidence(self: &Rc<Self>) {
        widgets::clear(&self.evidence);
        let Some(detail) = self.detail() else {
            return;
        };
        let candidates = section("Candidates");
        if detail.candidates.is_empty() {
            candidates.append(&label(
                "No frozen candidate yet. Freeze for review to preserve an exact candidate.",
                &["meta"],
            ));
        }
        let mut sorted = detail.candidates.clone();
        sorted.sort_by_key(|c| std::cmp::Reverse(c.ordinal));
        for c in &sorted {
            let row = vbox(space::TIGHT);
            row.add_css_class("evidence-row");
            let title_line = hbox(space::ITEM);
            let title = label(
                &format!(
                    "Candidate {} · {}",
                    c.ordinal,
                    format::candidate_status(c.status)
                ),
                &["strong"],
            );
            title.set_hexpand(true);
            title_line.append(&title);
            let counts = widgets::counts(c.added, c.removed);
            counts.set_valign(gtk::Align::Center);
            title_line.append(&counts);
            row.append(&title_line);
            row.append(&label(
                &format!(
                    "{} on {}@{} · {} file{}",
                    c.commit.short(),
                    detail.integration_target.branch,
                    c.base_target_commit.short(),
                    c.files,
                    if c.files == 1 { "" } else { "s" }
                ),
                &["meta"],
            ));
            if c.base_target_commit != detail.integration_target.commit
                && c.status == CandidateStatus::Current
            {
                row.append(&inline_message(
                    MessageKind::Warning,
                    &format!(
                        "Target moved: candidate was frozen on {}, target is now {}",
                        c.base_target_commit.short(),
                        detail.integration_target.commit.short()
                    ),
                    None,
                ));
            }
            set_accessible_label(
                &row,
                &format!(
                    "Candidate {}, {}, commit {}",
                    c.ordinal,
                    format::candidate_status(c.status),
                    c.commit.short()
                ),
            );
            candidates.append(&row);
        }
        self.evidence.append(&candidates);

        let checks = vbox(space::GROUP);
        let checks_heading = hbox(space::ITEM);
        let checks_title = widgets::section_title("Checks");
        checks_heading.append(&checks_title);
        let about = "Service-observed: LADE ran these commands and recorded exit and output. \
                     Agent statements are claims, not evidence.";
        let info = gtk::Image::builder()
            .icon_name("help-about-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .valign(gtk::Align::Center)
            .tooltip_text(about)
            .build();
        info.add_css_class("info-sign");
        set_accessible_description(&checks_title, about);
        checks_title.set_tooltip_text(Some(about));
        checks_heading.append(&info);
        checks.append(&checks_heading);
        let current = detail.current_candidate().map(|c| c.candidate_id.clone());
        let configured = self
            .controller
            .store
            .state()
            .settings
            .verification_commands
            .clone();
        for command in &configured {
            let ran = detail
                .verifications
                .iter()
                .any(|v| Some(&v.candidate_id) == current.as_ref() && &v.command == command);
            if current.is_some() && !ran {
                let row = vbox(space::TIGHT);
                row.add_css_class("evidence-row");
                row.append(&label(&format::command_line(command), &["mono"]));
                row.append(&label(
                    "○ Not run for the current candidate — unverified",
                    &["warning-text"],
                ));
                checks.append(&row);
            }
        }
        if detail.verifications.is_empty() {
            checks.append(&label("No checks recorded.", &["meta"]));
        }
        let mut verifications = detail.verifications.clone();
        verifications.sort_by(|a, b| {
            (Some(&b.candidate_id) == current.as_ref())
                .cmp(&(Some(&a.candidate_id) == current.as_ref()))
                .then(b.started_at.cmp(&a.started_at))
        });
        for v in &verifications {
            let row = vbox(space::TIGHT);
            row.add_css_class("evidence-row");
            let top = hbox(space::ITEM);
            let command = widgets::line_label(
                &format::command_line(&v.command),
                &["mono"],
                gtk::pango::EllipsizeMode::End,
            );
            command.set_hexpand(true);
            top.append(&command);
            let (sign, text) = format::verification_result(v.state);
            let result = gtk::Label::new(Some(&format!("{sign} {text}")));
            result.add_css_class(match v.state {
                VerificationState::Passed => "added",
                VerificationState::Failed | VerificationState::Error => "removed",
                _ => "warning-text",
            });
            result.add_css_class("strong");
            top.append(&result);
            row.append(&top);
            let ordinal = detail
                .candidates
                .iter()
                .find(|c| c.candidate_id == v.candidate_id)
                .map_or_else(
                    || "unknown candidate".to_owned(),
                    |c| format!("Candidate {}", c.ordinal),
                );
            let exit = v
                .exit_code
                .map_or_else(|| "no exit code".to_owned(), |c| format!("exit {c}"));
            row.append(&label(
                &format!(
                    "{ordinal} · {exit} · {} · service-observed",
                    format::duration_label(v.started_at, v.ended_at)
                ),
                &["meta"],
            ));
            if !v.output_tail.is_empty() {
                let output = label(&v.output_tail, &["mono", "output"]);
                output.set_selectable(true);
                output.set_wrap_mode(gtk::pango::WrapMode::Char);
                row.append(&widgets::disclosure(
                    if v.truncated {
                        "Output (last lines; earlier output truncated)"
                    } else {
                        "Output"
                    },
                    v.state == VerificationState::Failed,
                    &output,
                    |_| {},
                ));
            }
            set_accessible_label(
                &row,
                &format!(
                    "{}: {text}, {exit}, {ordinal}",
                    format::command_line(&v.command)
                ),
            );
            checks.append(&row);
        }
        self.evidence.append(&checks);

        let run = section("Run check");
        let verify = detail.availability(ActionName::ReviewVerify);
        if !configured.is_empty() {
            let presets = gtk::FlowBox::new();
            presets.set_selection_mode(gtk::SelectionMode::None);
            presets.set_max_children_per_line(3);
            for command in configured {
                let text = format::command_line(&command);
                let preset = gtk::Button::new();
                preset.set_child(Some(&widgets::line_label(
                    &text,
                    &[],
                    gtk::pango::EllipsizeMode::End,
                )));
                preset.add_css_class("quiet");
                preset.add_css_class("mono");
                set_focus_key(&preset, &format!("preset:{text}"));
                let entry = self.check_entry.clone();
                preset.connect_clicked(move |_| entry.set_text(&text));
                presets.insert(&preset, -1);
            }
            run.append(&presets);
        }
        if let Some(parent) = self.check_entry.parent()
            && let Ok(parent) = parent.downcast::<gtk::Box>()
        {
            parent.remove(&self.check_entry);
        }
        run.append(&self.check_entry);
        let this = Rc::downgrade(self);
        let entry = self.check_entry.clone();
        run.append(&action_row("Run check", &verify, &[], move |_| {
            if let Some(this) = this.upgrade() {
                let result = this.controller.verify(&entry.text());
                this.report_check(result.clone());
                if let Err(message) = result {
                    entry.set_tooltip_text(Some(&message));
                }
            }
        }));
        if let Some(error) = self.check_error.borrow().as_ref() {
            run.append(&inline_message(MessageKind::Error, error, None));
        }
        if let Some(refusal) = self.refusal(&detail.summary.objective_id, ActionSurface::Checks) {
            run.append(&refusal);
        }
        run.append(&label(
            "Runs in an isolated verification copy of the current candidate; it cannot change the candidate.",
            &["meta"],
        ));
        self.evidence.append(&run);
    }

    fn reconcile_drafts(self: &Rc<Self>) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.clone() else {
            drop(state);
            widgets::clear(&self.drafts_box);
            widgets::clear(&self.drafts_heading);
            self.drafts.borrow_mut().clear();
            return;
        };
        let detail = state.detail(&id).cloned();
        let editors: Vec<_> = state.unsent_drafts(&id).cloned().collect();
        drop(state);
        widgets::clear(&self.drafts_heading);
        self.drafts_heading.append(&widgets::section_title(&format!(
            "Drafts ({})",
            editors.len()
        )));
        self.drafts_heading.append(&label(
            "Saved to the service; sent only when you submit them together.",
            &["meta"],
        ));
        if editors.is_empty() {
            let none = vbox(space::TIGHT);
            none.append(&label(
                "No unsent drafts. Accepted feedback and its delivery are listed in Activity.",
                &["meta"],
            ));
            let open = widgets::button("Open Activity", &["quiet", "list-action-button"]);
            open.set_halign(gtk::Align::Start);
            set_accessible_description(&open, "Accepted inputs and their delivery state");
            set_focus_key(&open, "feedback:open-activity");
            let controller = self.controller.clone();
            open.connect_clicked(move |_| controller.set_view(MainView::Activity));
            none.append(&open);
            self.drafts_heading.append(&none);
        }
        let mut widgets_map = self.drafts.borrow_mut();
        widgets_map.retain(|draft_id, w| {
            let keep = editors.iter().any(|e| &e.draft_id == draft_id);
            if !keep {
                self.drafts_box.remove(&w.root);
            }
            keep
        });
        let mut previous: Option<gtk::Widget> = None;
        for editor in &editors {
            let w = widgets_map
                .entry(editor.draft_id.clone())
                .or_insert_with(|| self.draft_widgets(&editor.draft_id));
            if w.root.parent().is_none() {
                self.drafts_box.append(&w.root);
            }
            self.drafts_box
                .reorder_child_after(&w.root, previous.as_ref());
            previous = Some(w.root.clone().upcast());
            w.reference
                .set_text(&format::reference_label(&editor.reference, detail.as_ref()));
            let excerpt = editor
                .reference
                .selected_text
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join("\n");
            w.excerpt.set_text(&excerpt);
            if w.input.text() != editor.body {
                w.input.set_text(&editor.body);
            }
            set_accessible_label(
                &w.view,
                &format!(
                    "Feedback on {}",
                    format::reference_label(&editor.reference, detail.as_ref())
                ),
            );
        }
        drop(widgets_map);
        for editor in &editors {
            self.render_draft_status(&editor.draft_id);
        }
    }

    fn draft_widgets(self: &Rc<Self>, draft_id: &DraftId) -> DraftWidgets {
        let root = vbox(space::ITEM);
        root.add_css_class("draft");
        let reference = label("", &["meta", "draft-reference"]);
        root.append(&reference);
        let excerpt = label("", &["mono", "excerpt"]);
        excerpt.set_wrap_mode(gtk::pango::WrapMode::Char);
        root.append(&excerpt);
        let stale = vbox(0);
        root.append(&stale);
        let view = gtk::TextView::new();
        set_focus_key(&view, &format!("draft:{}", draft_id.as_str()));
        let input = widgets::LimitedInput::new(&view, TEXT_LIMIT, "this feedback draft");
        root.append(&input.field(&widgets::bounded_text(&view, 88, 200)));
        let status_row = vbox(space::TIGHT);
        let status = label("", &["meta", "save-state"]);
        status.set_hexpand(true);
        status_row.append(&status);
        let actions = hbox(space::ITEM);
        status_row.append(&actions);
        root.append(&status_row);
        let controller = self.controller.clone();
        let id = draft_id.clone();
        let this = Rc::downgrade(self);
        input.connect_commit(move |text| {
            controller.edit_draft(&id, text, widgets::monotonic_ms());
            if let Some(this) = this.upgrade() {
                let handler = this.on_draft_edit.borrow().clone();
                if let Some(handler) = handler {
                    handler();
                }
            }
        });
        DraftWidgets {
            root,
            reference,
            excerpt,
            stale,
            view,
            input,
            status,
            actions,
        }
    }

    fn render_draft_status(self: &Rc<Self>, draft_id: &DraftId) {
        let map = self.drafts.borrow();
        let Some(w) = map.get(draft_id) else { return };
        let state = self.controller.store.state();
        let Some(editor) = state
            .drafts
            .values()
            .flatten()
            .find(|d| &d.draft_id == draft_id)
        else {
            return;
        };
        let status = editor.status(state.connection.is_connected());
        let stale = editor.stale;
        drop(state);
        let shown = w.stale.first_child().is_some();
        if shown != stale {
            widgets::clear(&w.stale);
            if stale {
                w.stale.append(&inline_message(
                    MessageKind::Warning,
                    "Stale reference — the inspected bytes changed. Re-anchor or remove this item; it blocks the whole batch.",
                    None,
                ));
            }
        }
        w.status.set_text(&status.label());
        let details = match &status {
            SaveStatus::Failed { details, .. } if !details.is_empty() => Some(details.as_str()),
            _ => None,
        };
        w.status.set_tooltip_text(details);
        set_accessible_description(&w.status, details.unwrap_or(""));
        for class in ["save-failed", "save-warning", "save-ok"] {
            w.status.remove_css_class(class);
        }
        w.status.add_css_class(match status {
            SaveStatus::Failed { .. } => "save-failed",
            SaveStatus::Disconnected | SaveStatus::Conflict { .. } => "save-warning",
            SaveStatus::Saved { .. } => "save-ok",
            _ => "save-neutral",
        });
        set_accessible_label(&w.status, &format!("Draft save state: {}", status.label()));
        widgets::clear(&w.actions);
        let id = draft_id.clone();
        match status {
            SaveStatus::Failed { .. } => {
                let retry = widgets::button("Retry", &["quiet"]);
                set_focus_key(&retry, &format!("retry:{}", id.as_str()));
                let this = Rc::downgrade(self);
                let rid = id.clone();
                retry.connect_clicked(move |_| {
                    if let Some(this) = this.upgrade() {
                        this.controller.retry_draft(&rid, widgets::monotonic_ms());
                        let handler = this.on_draft_edit.borrow().clone();
                        if let Some(handler) = handler {
                            handler();
                        }
                    }
                });
                w.actions.append(&retry);
            }
            SaveStatus::Conflict { .. } => {
                for (text, keep) in [("Keep mine", true), ("Use saved", false)] {
                    let b = widgets::button(text, &["quiet"]);
                    let this = Rc::downgrade(self);
                    let cid = id.clone();
                    b.connect_clicked(move |_| {
                        if let Some(this) = this.upgrade() {
                            this.controller.resolve_draft_conflict(
                                &cid,
                                keep,
                                widgets::monotonic_ms(),
                            );
                            let handler = this.on_draft_edit.borrow().clone();
                            if let Some(handler) = handler {
                                handler();
                            }
                        }
                    });
                    w.actions.append(&b);
                }
            }
            _ => {}
        }
        let delete = widgets::button("Remove draft…", &["quiet", "destructive-quiet"]);
        delete.set_tooltip_text(Some("Delete this draft from the service"));
        set_focus_key(&delete, &format!("remove:{}", id.as_str()));
        let controller = self.controller.clone();
        let reference = w.reference.text().to_string();
        delete.connect_clicked(move |b| {
            let controller = controller.clone();
            let id = id.clone();
            dialogs::confirm(
                b,
                "Remove draft",
                &format!(
                    "The draft on {reference} and its text are deleted. This cannot be undone; nothing is sent to the agent."
                ),
                "Remove draft",
                true,
                move || controller.delete_draft(&id),
            );
        });
        w.actions.append(&delete);
    }

    fn render_submit(self: &Rc<Self>) {
        widgets::clear(&self.submit_box);
        let refusal = self
            .controller
            .store
            .state()
            .ui
            .selected
            .clone()
            .and_then(|o| self.refusal(&o, ActionSurface::Feedback));
        let Some(batch) = self
            .controller
            .feedback_batch()
            .filter(|b| !b.items.is_empty())
        else {
            if let Some(refusal) = refusal {
                self.submit_box.append(&refusal);
            }
            return;
        };
        let s = section("Submit feedback");
        let mode = widgets::dropdown_or(&["Steer", "Follow-up"], "No delivery mode available");
        set_accessible_label(&mode, "Delivery mode");
        set_focus_key(&mode, "submit:mode");
        if let Some(index) = FEEDBACK_MODES.iter().position(|m| *m == batch.mode) {
            mode.set_selected(index as u32);
        }
        let controller = self.controller.clone();
        mode.connect_selected_notify(move |d| {
            if let Some(m) = FEEDBACK_MODES.get(d.selected() as usize) {
                controller.set_feedback_mode(*m);
            }
        });
        let explanation = format::mode_explanation(batch.mode);
        mode.set_tooltip_text(Some(explanation));
        set_accessible_description(&mode, explanation);
        s.append(&field("Delivery", &mode));
        let destination = batch.destination.as_ref().map_or_else(
            || "No Agent Session".to_owned(),
            |d| {
                format!(
                    "Session {} · {} · gen {}",
                    d.session_id.short(),
                    d.model.label(),
                    d.execution_generation
                )
            },
        );
        s.append(&field("Destination", &label(&destination, &[])));
        for blocker in &batch.blockers {
            s.append(&inline_message(MessageKind::Warning, blocker, None));
        }
        let availability = if batch.blockers.is_empty() {
            ActionAvailability::Enabled
        } else {
            ActionAvailability::Disabled {
                reason: "Resolve the items above before submitting".to_owned(),
            }
        };
        let count = batch.items.len();
        let this = Rc::downgrade(self);
        s.append(&action_row(
            &format!(
                "Review and submit {count} item{}…",
                if count == 1 { "" } else { "s" }
            ),
            &availability,
            &["primary"],
            move |b| {
                if let Some(this) = this.upgrade()
                    && let Some(batch) = this.controller.feedback_batch()
                {
                    dialogs::submit_feedback(b, &this.controller, batch);
                }
            },
        ));
        if let Some(refusal) = refusal {
            s.append(&refusal);
        }
        self.submit_box.append(&s);
    }

    pub fn focus_draft(&self, draft_id: &DraftId) {
        self.root.set_current_page(Some(2));
        let Some((view, draft)) = self
            .drafts
            .borrow()
            .get(draft_id)
            .map(|w| (w.view.clone(), w.root.clone()))
        else {
            return;
        };
        view.grab_focus();
        let viewport = self
            .root
            .nth_page(Some(2))
            .and_downcast::<gtk::ScrolledWindow>()
            .and_then(|s| s.child())
            .and_downcast::<gtk::Viewport>();
        let Some(viewport) = viewport else { return };
        let allocated = Cell::new(false);
        view.add_tick_callback(move |view, _| {
            if !allocated.replace(true) {
                return gtk::glib::ControlFlow::Continue;
            }
            if view.has_focus() {
                viewport.scroll_to(&draft, None);
            }
            gtk::glib::ControlFlow::Break
        });
    }
}
