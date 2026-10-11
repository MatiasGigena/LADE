//! Right inspector: Overview (summary, intent, actions), Evidence (candidates, service-observed
//! checks) and Feedback (drafts with save state, submission, accepted inputs).
//!
//! Every action's availability comes from `available_actions`; disabled actions keep their place
//! and show the service's reason.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::dialogs;
use super::session;
use super::widgets::{
    self, action_row, field, hbox, label, set_accessible_label, set_focus_key, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::drafts::SaveStatus;
use crate::presentation::format;
use crate::presentation::model::{
    ActionAvailability, ActionName, CandidateStatus, DeliveryMode, DraftId, NoticeLevel,
    ObjectiveDetail, Timestamp, VerificationState,
};
use crate::presentation::store::{Change, InspectorTab};
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
    submit_box: gtk::Box,
    inputs_box: gtk::Box,
    drafts: RefCell<HashMap<DraftId, DraftWidgets>>,
    last_error: RefCell<Option<String>>,
    check_entry: gtk::Entry,
    updating: Cell<bool>,
    on_draft_edit: RefCell<Option<Rc<dyn Fn()>>>,
    /// Extension dialogs of the live sessions (IMP-011), answered only by the developer.
    dialogs: session::DialogHost,
}

fn page(name: &str) -> (gtk::ScrolledWindow, gtk::Box) {
    let content = vbox(24);
    content.add_css_class("inspector-page");
    // No horizontal scrolling: content is width-constrained to the column (labels wrap, ids
    // ellipsize with tooltips, drop-downs ellipsize, buttons wrap).
    let scroll = widgets::region_scroll(&content);
    scroll.set_propagate_natural_width(false);
    scroll.set_vexpand(true);
    set_accessible_label(&scroll, name);
    (scroll, content)
}

fn section(title: &str) -> gtk::Box {
    let section = vbox(12);
    section.append(&widgets::section_title(title));
    section
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

        let drafts_heading = vbox(8);
        feedback.append(&drafts_heading);
        let drafts_box = vbox(16);
        feedback.append(&drafts_box);
        let submit_box = vbox(12);
        feedback.append(&submit_box);
        let inputs_box = vbox(12);
        feedback.append(&inputs_box);

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
            submit_box,
            inputs_box,
            drafts: RefCell::new(HashMap::new()),
            last_error: RefCell::new(None),
            check_entry,
            updating: Cell::new(false),
            on_draft_edit: RefCell::new(None),
            dialogs: session::DialogHost::default(),
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
        inspector.render_submit();
        inspector.render_inputs();
        inspector.sync_tab();
        inspector
    }

    pub fn set_on_draft_edit(&self, handler: Rc<dyn Fn()>) {
        *self.on_draft_edit.borrow_mut() = Some(handler);
    }

    pub fn refresh(self: &Rc<Self>, changes: &[Change]) {
        let selected = self.controller.store.state().ui.selected.clone();
        let mine = |id: &crate::presentation::model::ObjectiveId| Some(id) == selected.as_ref();
        let (mut overview, mut evidence, mut drafts, mut submit, mut inputs) =
            (false, false, false, false, false);
        for change in changes {
            match change {
                Change::Selection => {
                    self.last_error.replace(None);
                    (overview, evidence, drafts, submit, inputs) = (true, true, true, true, true);
                }
                Change::Connection | Change::Workspaces => {
                    (overview, evidence, submit) = (true, true, true);
                }
                Change::Detail(id) if mine(id) => {
                    (overview, evidence, submit) = (true, true, true);
                }
                Change::Drafts(id) if mine(id) => {
                    drafts = true;
                    submit = true;
                }
                Change::DraftStatus(id) => {
                    self.render_draft_status(id);
                    submit = true;
                }
                Change::Inputs(id) if mine(id) => {
                    inputs = true;
                    overview = true;
                }
                Change::InspectorTab => self.sync_tab(),
                Change::Catalog | Change::Resources => overview = true,
                Change::Session(id) if mine(id) => overview = true,
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
        }
        if submit {
            widgets::rebuild_preserving_focus(&self.submit_box.clone(), || self.render_submit());
        }
        if inputs {
            self.render_inputs();
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
        let changed = match result {
            Ok(()) => self.last_error.replace(None).is_some(),
            Err(message) => {
                self.last_error.replace(Some(message));
                true
            }
        };
        if changed {
            widgets::rebuild_preserving_focus(&self.overview.clone(), || self.render_overview());
        }
    }

    fn detail(&self) -> Option<ObjectiveDetail> {
        self.controller.store.state().selected_detail().cloned()
    }

    fn render_overview(self: &Rc<Self>) {
        widgets::clear(&self.overview);
        let Some(detail) = self.detail() else {
            self.overview.append(&label(
                if self.controller.store.state().ui.selected.is_some() {
                    "Loading Objective…"
                } else {
                    "Select an Objective to see its summary and actions."
                },
                &["meta"],
            ));
            return;
        };
        let summary = &detail.summary;
        let s = section("Objective summary");
        s.append(&label(&detail.intent.outcome, &["secondary"]));
        let status = vbox(4);
        status.append(&widgets::status_pill(summary.lifecycle, summary.attention));
        status.append(&label(
            &format!(
                "Updated {}",
                format::relative_time(Timestamp(widgets::now_ms()), summary.updated_at)
            ),
            &["meta"],
        ));
        s.append(&field("Status", &status));
        s.append(&field(
            "Lifecycle",
            &label(format::lifecycle_label(summary.lifecycle), &[]),
        ));
        let agent = vbox(2);
        match &detail.session {
            Some(session) => {
                agent.append(&label(&session.model.label(), &[]));
                agent.append(&label(&format::execution_label_for(&detail), &["meta"]));
                let ids = label(
                    &format!(
                        "Session {} · generation {}",
                        session.session_id.short(),
                        session.execution_generation
                    ),
                    &["meta"],
                );
                ids.set_tooltip_text(Some(&format!(
                    "Agent Session {} · execution generation {}",
                    session.session_id.as_str(),
                    session.execution_generation
                )));
                agent.append(&ids);
            }
            None => agent.append(&label("No Agent Session yet", &["meta"])),
        }
        s.append(&field("Agent", &agent));
        let files = hbox(12);
        files.append(&label(
            &format!(
                "{} file{}",
                detail.change_counts.files,
                if detail.change_counts.files == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            &[],
        ));
        files.append(&widgets::counts(
            detail.change_counts.added,
            detail.change_counts.removed,
        ));
        s.append(&field("Files changed", &files));
        let ellipsized =
            |text: &str| widgets::line_label(text, &["mono"], gtk::pango::EllipsizeMode::Middle);
        s.append(&field("Branch", &ellipsized(&detail.branch)));
        s.append(&field(
            "Target",
            &ellipsized(&detail.integration_target.label()),
        ));
        let start = ellipsized(detail.start_commit.short());
        start.set_tooltip_text(Some(&detail.start_commit.hex));
        s.append(&field("Started at", &start));
        if !summary.badges.is_empty() {
            s.append(&field("Badges", &label(&summary.badge_text(), &[])));
        }
        self.overview.append(&s);

        let intent = section("Intent");
        for (key, value) in [
            ("Outcome", &detail.intent.outcome),
            ("Constraints", &detail.intent.constraints),
            ("Acceptance", &detail.intent.acceptance),
        ] {
            let block = vbox(2);
            block.append(&label(key, &["meta"]));
            block.append(&label(if value.is_empty() { "—" } else { value }, &[]));
            intent.append(&block);
        }
        intent.append(&label(
            &format!("Intent revision {}", detail.intent_revision),
            &["meta"],
        ));
        let this = Rc::downgrade(self);
        intent.append(&action_row(
            "Edit intent",
            &detail.availability(ActionName::ObjectiveUpdateIntent),
            &[],
            move |b| {
                if let Some(this) = this.upgrade() {
                    dialogs::edit_intent(b, &this.controller);
                }
            },
        ));
        self.overview.append(&intent);

        let actions = section("Actions");
        if let Some(error) = self.last_error.borrow().as_ref() {
            actions.append(&widgets::error_line(error));
        }
        // The service's answer to the latest command (rejection with its code, or recovery).
        let status = self
            .controller
            .store
            .state()
            .command_status
            .get(&summary.objective_id)
            .cloned();
        if let Some(status) = status {
            actions.append(&match status.level {
                NoticeLevel::Info => label(&status.message, &["meta"]).upcast::<gtk::Widget>(),
                NoticeLevel::Warning => widgets::warning_line(&status.message).upcast(),
                NoticeLevel::Error => widgets::error_line(&status.message).upcast(),
            });
        }
        // Agent session.
        let start = detail.availability(ActionName::ExecutionStart);
        let stopped = detail.session.is_some()
            && !matches!(
                detail.summary.execution.state,
                crate::presentation::model::ExecutionState::Running
                    | crate::presentation::model::ExecutionState::Starting
            );
        if start != ActionAvailability::NotOffered || stopped {
            // Provider, model and thinking from the service's SDK catalogue; restricted providers
            // stay listed with their reason. The same choice applies to "Apply model and thinking".
            let picker = session::model_picker(&self.controller);
            picker.set_visible(start == ActionAvailability::Enabled || stopped);
            actions.append(&picker);
        }
        if start != ActionAvailability::NotOffered {
            let this = Rc::downgrade(self);
            actions.append(&action_row(
                "Start agent",
                &start,
                &["primary"],
                move |_| {
                    if let Some(this) = this.upgrade() {
                        let result =
                            this.controller
                                .chosen_model()
                                .and_then(|(model, thinking)| {
                                    this.controller.start_execution(model, Some(thinking))
                                });
                        this.report(result);
                    }
                },
            ));
        }
        let this = Rc::downgrade(self);
        actions.append(&action_row(
            "Cancel agent",
            &detail.availability(ActionName::ExecutionCancel),
            &[],
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.report(this.controller.cancel());
                }
            },
        ));
        let this = Rc::downgrade(self);
        actions.append(&action_row(
            "Resume agent",
            &detail.availability(ActionName::ExecutionResume),
            &[],
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.report(this.controller.resume());
                }
            },
        ));
        let this = Rc::downgrade(self);
        actions.append(&action_row(
            "Freeze for review",
            &detail.availability(ActionName::ReviewFreeze),
            &[],
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.report(this.controller.freeze());
                }
            },
        ));
        let this = Rc::downgrade(self);
        actions.append(&action_row(
            "Return for revision",
            &detail.availability(ActionName::ReviewReturn),
            &[],
            move |b| {
                if let Some(this) = this.upgrade() {
                    let controller = this.controller.clone();
                    dialogs::confirm(
                        b,
                        "Return for revision",
                        "The current candidate becomes historical and any approval or override is invalidated before the agent may change the work again.",
                        "Return for revision",
                        false,
                        move || controller.return_for_revision(),
                    );
                }
            },
        ));
        let this = Rc::downgrade(self);
        actions.append(&action_row(
            "Refresh against target",
            &detail.availability(ActionName::ReviewRefresh),
            &[],
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.report(this.controller.refresh_candidate());
                }
            },
        ));
        let approve = detail.availability(ActionName::ReviewApprove);
        let needs_override = self.controller.approval_needs_override();
        let this = Rc::downgrade(self);
        let approve_label = match detail.current_candidate() {
            Some(c) => format!("Approve Candidate {}", c.ordinal),
            None => "Approve".to_owned(),
        };
        actions.append(&action_row(
            &approve_label,
            &approve,
            &["primary"],
            move |b| {
                if let Some(this) = this.upgrade() {
                    if needs_override {
                        dialogs::approve_with_override(b, &this.controller);
                    } else {
                        this.report(this.controller.approve(None));
                    }
                }
            },
        ));
        if approve == ActionAvailability::Enabled && needs_override {
            actions.append(&label(
                "Checks are missing or not all passed — approval requires a recorded override reason.",
                &["meta"],
            ));
        }
        if let Some(approval) = &detail.approval {
            actions.append(&label(format::approval_label(approval.state), &["meta"]));
        }

        // Protected integration.
        let integrate = detail.availability(ActionName::ReviewIntegrate);
        if integrate != ActionAvailability::NotOffered {
            let protected = vbox(8);
            protected.add_css_class("protected");
            protected.append(&label("Protected integration", &["strong"]));
            protected.append(&label(
                &format!(
                    "Creates one squash commit on {} only for the approved candidate and target commit. Integration is a separate, explicit step.",
                    detail.integration_target.branch
                ),
                &["secondary"],
            ));
            let this = Rc::downgrade(self);
            let target = detail.integration_target.label();
            protected.append(&action_row(
                &format!("Integrate into {}", detail.integration_target.branch),
                &integrate,
                &["primary"],
                move |b| {
                    if let Some(this) = this.upgrade() {
                        let controller = this.controller.clone();
                        dialogs::confirm(
                            b,
                            "Integrate approved candidate",
                            &format!(
                                "Create a squash commit on {target} from the approved candidate. The target must be unchanged and its checkout clean."
                            ),
                            "Integrate",
                            false,
                            move || controller.integrate(),
                        );
                    }
                },
            ));
            actions.append(&protected);
        }
        let this = Rc::downgrade(self);
        actions.append(&action_row(
            "Complete without changes",
            &detail.availability(ActionName::ObjectiveComplete),
            &[],
            move |b| {
                if let Some(this) = this.upgrade() {
                    let controller = this.controller.clone();
                    dialogs::confirm(
                        b,
                        "Complete without changes",
                        "The Objective is completed with no repository change.",
                        "Complete",
                        false,
                        move || controller.complete(),
                    );
                }
            },
        ));
        let this = Rc::downgrade(self);
        let title = summary.title.clone();
        actions.append(&action_row(
            "Discard Objective",
            &detail.availability(ActionName::ObjectiveDiscard),
            &["destructive"],
            move |b| {
                if let Some(this) = this.upgrade() {
                    let controller = this.controller.clone();
                    dialogs::confirm(
                        b,
                        "Discard Objective",
                        &format!(
                            "“{title}” will be abandoned without integration and its agent stopped. Branch and worktree stay until you clean up."
                        ),
                        "Discard",
                        true,
                        move || controller.discard(),
                    );
                }
            },
        ));
        let this = Rc::downgrade(self);
        let branch = detail.branch.clone();
        let unsent = detail.unsent_drafts;
        actions.append(&action_row(
            "Clean up branch and worktree",
            &detail.availability(ActionName::ObjectiveCleanup),
            &["destructive"],
            move |b| {
                if let Some(this) = this.upgrade() {
                    dialogs::cleanup(b, &this.controller, &branch, unsent);
                }
            },
        ));
        if detail.history_deleted {
            actions.append(&label(
                "Git history deleted: retention references and branch were removed on request.",
                &["meta"],
            ));
        } else {
            let this = Rc::downgrade(self);
            let title = summary.title.clone();
            actions.append(&action_row(
                "Delete Git history",
                &detail.availability(ActionName::ObjectiveDeleteHistory),
                &["destructive"],
                move |b| {
                    if let Some(this) = this.upgrade() {
                        dialogs::delete_history(b, &this.controller, &title);
                    }
                },
            ));
        }
        self.overview.append(&actions);
        if detail.session.is_some() {
            let this = Rc::downgrade(self);
            let report: Rc<dyn Fn(Result<(), String>)> = Rc::new(move |result| {
                if let Some(this) = this.upgrade() {
                    this.report(result);
                }
            });
            self.overview
                .append(&session::session_section(&self.controller, &detail, report));
        }
        self.overview
            .append(&session::resources_section(&self.controller));
    }

    fn render_evidence(self: &Rc<Self>) {
        widgets::clear(&self.evidence);
        let Some(detail) = self.detail() else {
            self.evidence.append(&label(
                "Select an Objective to see its evidence.",
                &["meta"],
            ));
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
            let row = vbox(2);
            row.add_css_class("evidence-row");
            row.append(&label(
                &format!(
                    "Candidate {} · {}",
                    c.ordinal,
                    format::candidate_status(c.status)
                ),
                &["strong"],
            ));
            let detail_line = hbox(8);
            detail_line.append(&label(
                &format!(
                    "{} on {}@{} · {} file{}",
                    c.commit.short(),
                    detail.integration_target.branch,
                    c.base_target_commit.short(),
                    c.files,
                    if c.files == 1 { "" } else { "s" }
                ),
                &["meta", "mono"],
            ));
            detail_line.append(&widgets::counts(c.added, c.removed));
            row.append(&detail_line);
            if c.base_target_commit != detail.integration_target.commit
                && c.status == CandidateStatus::Current
            {
                row.append(&widgets::warning_line(&format!(
                    "Target moved: candidate was frozen on {}, target is now {}",
                    c.base_target_commit.short(),
                    detail.integration_target.commit.short()
                )));
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

        let checks = section("Checks");
        checks.append(&label(
            "Service-observed: LADE ran these commands and recorded exit and output. Agent statements in Activity are claims, not evidence.",
            &["meta"],
        ));
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
                let row = vbox(2);
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
            let row = vbox(4);
            row.add_css_class("evidence-row");
            let top = hbox(8);
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
                this.report(result.clone());
                if let Err(message) = result {
                    entry.set_tooltip_text(Some(&message));
                }
            }
        }));
        run.append(&label(
            "Runs in an isolated verification copy of the current candidate; it cannot change the candidate.",
            &["meta"],
        ));
        self.evidence.append(&run);
    }

    /// Creates/updates/removes draft widgets without disturbing the text being typed.
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
            "Drafts are saved to the service but not sent until you submit them together.",
            &["meta"],
        ));
        if editors.is_empty() {
            self.drafts_heading.append(&label(
                "Select text in the viewer and choose Add feedback to start a draft.",
                &["meta"],
            ));
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
        let root = vbox(8);
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
        let status_row = vbox(4);
        let status = label("", &["meta", "save-state"]);
        status.set_hexpand(true);
        status_row.append(&status);
        let actions = hbox(8);
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
        // Staleness arrives with draft updates, so it is rendered with the save state.
        let shown = w.stale.first_child().is_some();
        if shown != stale {
            widgets::clear(&w.stale);
            if stale {
                w.stale.append(&widgets::warning_line(
                    "Stale reference — the inspected bytes changed. Re-anchor or remove this item; it blocks the whole batch.",
                ));
            }
        }
        w.status.set_text(&status.label());
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
                move || {
                    controller.delete_draft(&id);
                    Ok(())
                },
            );
        });
        w.actions.append(&delete);
    }

    fn render_submit(self: &Rc<Self>) {
        widgets::clear(&self.submit_box);
        // Nothing to submit: the Drafts heading already says how to start one.
        let Some(batch) = self
            .controller
            .feedback_batch()
            .filter(|b| !b.items.is_empty())
        else {
            return;
        };
        let s = section("Submit feedback");
        let mode = widgets::dropdown(&["Steer", "Follow-up"]);
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
        s.append(&field("Delivery", &mode));
        s.append(&label(
            match batch.mode {
                DeliveryMode::FollowUp => {
                    "Follow-up waits until the agent finishes its current work."
                }
                _ => {
                    "Steer is delivered after the current tools finish; it is not an instant stop."
                }
            },
            &["meta"],
        ));
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
            s.append(&widgets::warning_line(blocker));
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
        self.submit_box.append(&s);
    }

    fn render_inputs(&self) {
        widgets::clear(&self.inputs_box);
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.as_ref() else {
            return;
        };
        let inputs = state.inputs.get(id).cloned().unwrap_or_default();
        drop(state);
        let s = section("Accepted inputs");
        if inputs.is_empty() {
            s.append(&label("Nothing accepted yet.", &["meta"]));
        }
        for input in inputs.iter().rev() {
            let row = vbox(2);
            row.add_css_class("evidence-row");
            let line = format::input_line(input);
            let state_label = label(&line, &["strong"]);
            if matches!(
                input.delivery,
                crate::presentation::model::DeliveryState::Uncertain
                    | crate::presentation::model::DeliveryState::Blocked
            ) {
                state_label.add_css_class("warning-text");
            }
            row.append(&state_label);
            row.append(&label(&input.text_preview, &[]));
            row.append(&label(
                &format!(
                    "Accepted {} · input {}",
                    format::relative_time(Timestamp(widgets::now_ms()), input.accepted_at),
                    input.input_id.short()
                ),
                &["meta"],
            ));
            set_accessible_label(&row, &format!("{line}: {}", input.text_preview));
            s.append(&row);
        }
        self.inputs_box.append(&s);
    }

    /// Focuses a draft's text view (after Add feedback), switching to the Feedback tab, and
    /// reveals the whole draft (field, error and save state) once it has been allocated. The
    /// page or compact region may have just been shown, so revealing on idle could use stale
    /// bounds; a frame callback waits for the first allocated frame instead.
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
