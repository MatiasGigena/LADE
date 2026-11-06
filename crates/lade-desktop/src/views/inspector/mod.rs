//! The inspector region: the Overview, Evidence and Feedback tabs of the selected Objective.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::session;
use super::widgets::{
    self, MessageKind, inline_message, label, set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::state::model::{
    ActionSurface, DraftId, NoticeLevel, ObjectiveDetail, ObjectiveId,
};
use crate::presentation::state::store::{Change, InspectorTab};

mod evidence;
mod feedback;
mod overview;

use feedback::DraftWidgets;

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
        let mine = |id: &ObjectiveId| Some(id) == selected.as_ref();
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
}
