//! The Evidence tab: frozen candidates, service-observed checks, and running a check.

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::super::widgets::{
    self, MessageKind, action_row, hbox, inline_message, label, set_accessible_description,
    set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::state::model::{
    ActionName, ActionSurface, CandidateStatus, CandidateSummary, ObjectiveDetail,
    VerificationState, VerificationSummary,
};
use crate::presentation::text::format;

use super::{Inspector, section};

impl Inspector {
    pub(super) fn render_evidence(self: &Rc<Self>) {
        widgets::clear(&self.evidence);
        let Some(detail) = self.detail() else {
            return;
        };
        let configured = self
            .controller
            .store
            .state()
            .settings
            .verification_commands
            .clone();
        self.evidence.append(&candidates_section(&detail));
        self.evidence.append(&checks_section(&detail, &configured));
        self.evidence
            .append(&self.run_check_section(&detail, configured));
    }

    fn run_check_section(
        self: &Rc<Self>,
        detail: &ObjectiveDetail,
        configured: Vec<Vec<String>>,
    ) -> gtk::Box {
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
        run
    }
}

fn candidates_section(detail: &ObjectiveDetail) -> gtk::Box {
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
        candidates.append(&candidate_row(detail, c));
    }
    candidates
}

fn candidate_row(detail: &ObjectiveDetail, c: &CandidateSummary) -> gtk::Box {
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
    row
}

/// Configured checks not yet run for the current candidate come first, then every recorded
/// check, the current candidate's newest first.
fn checks_section(detail: &ObjectiveDetail, configured: &[Vec<String>]) -> gtk::Box {
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
    for command in configured {
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
        checks.append(&verification_row(detail, v));
    }
    checks
}

fn verification_row(detail: &ObjectiveDetail, v: &VerificationSummary) -> gtk::Box {
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
    row
}
