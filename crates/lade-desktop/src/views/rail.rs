//! The Objectives rail: Objectives grouped by attention, and the archive of closed ones.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{self, hbox, label, set_accessible_label, set_focus_key, space, vbox};
use crate::presentation::controller::Controller;
use crate::presentation::state::model::{ObjectiveId, ObjectiveSummary};
use crate::presentation::state::store::Change;
use crate::presentation::text::format;
use crate::presentation::text::objective;

pub type ButtonHandler = Rc<dyn Fn(&gtk::Button)>;

pub struct Rail {
    pub root: gtk::ScrolledWindow,
    controller: Rc<Controller>,
    content: gtk::Box,
    lists: RefCell<Vec<(gtk::ListBox, Vec<ObjectiveId>)>>,
    pub new_button: gtk::Button,
    on_new: RefCell<Option<ButtonHandler>>,
    on_open: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Rail {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let outer = vbox(space::SECTION);
        outer.add_css_class("rail");
        let heading_row = hbox(space::ITEM);
        let heading = widgets::heading("Objectives", "section-title");
        heading.set_hexpand(true);
        heading_row.append(&heading);
        let new_button = gtk::Button::from_icon_name("list-add-symbolic");
        new_button.add_css_class("icon-button");
        new_button.set_tooltip_text(Some("New Objective"));
        set_accessible_label(&new_button, "New Objective");
        heading_row.append(&new_button);
        outer.append(&heading_row);
        let content = vbox(space::PAGE);
        outer.append(&content);
        let root = widgets::region_scroll(&outer);
        root.add_css_class("rail-scroll");
        set_accessible_label(&root, "Objectives");
        let rail = Rc::new(Self {
            root,
            controller: controller.clone(),
            content,
            lists: RefCell::new(Vec::new()),
            new_button,
            on_new: RefCell::new(None),
            on_open: RefCell::new(None),
        });
        let this = Rc::downgrade(&rail);
        rail.new_button.connect_clicked(move |b| {
            if let Some(this) = this.upgrade() {
                let handler = this.on_new.borrow().clone();
                if let Some(handler) = handler {
                    handler(b);
                }
            }
        });
        rail.rebuild();
        rail
    }

    pub fn set_on_new(&self, handler: ButtonHandler) {
        *self.on_new.borrow_mut() = Some(handler);
    }

    pub fn set_on_open(&self, handler: Rc<dyn Fn()>) {
        *self.on_open.borrow_mut() = Some(handler);
    }

    pub fn refresh(&self, changes: &[Change]) {
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::Objectives | Change::Workspaces | Change::Connection
            )
        }) {
            widgets::rebuild_preserving_focus(&self.content.clone(), || self.rebuild());
        } else if changes.contains(&Change::Selection) {
            self.sync_selection();
        }
    }

    fn row(objective: &ObjectiveSummary) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.add_css_class("objective-row");
        set_focus_key(
            &row,
            &format!("objective:{}", objective.objective_id.as_str()),
        );
        let content = hbox(space::ITEM);
        let title = widgets::line_label(
            &objective.title,
            &["objective-title"],
            gtk::pango::EllipsizeMode::End,
        );
        title.set_hexpand(true);
        title.set_has_tooltip(false);
        content.append(&title);
        let (sign, state) = format::status_pill(objective.lifecycle, objective.attention);
        let line = objective::objective_row_line(objective);
        if line.is_some() {
            let sign = label(sign, &["objective-sign"]);
            sign.set_wrap(false);
            content.append(&sign);
        }
        row.set_child(Some(&content));
        let mut tooltip = objective.title.clone();
        for extra in [line.as_deref().unwrap_or(""), objective.outcome.as_str()] {
            if !extra.is_empty() {
                tooltip.push('\n');
                tooltip.push_str(extra);
            }
        }
        row.set_tooltip_text(Some(&tooltip));
        let badges = if objective.badges.is_empty() {
            String::new()
        } else {
            format!(", {}", objective.badge_text())
        };
        set_accessible_label(&row, &format!("{}, {state}{badges}", objective.title));
        if !objective.outcome.is_empty() {
            widgets::set_accessible_description(&row, &objective.outcome);
        }
        row
    }

    fn list(objectives: &[&ObjectiveSummary], name: &str) -> gtk::ListBox {
        let list = gtk::ListBox::new();
        list.add_css_class("objective-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        set_accessible_label(&list, name);
        for objective in objectives {
            list.append(&Self::row(objective));
        }
        list
    }

    fn rebuild(&self) {
        widgets::clear(&self.content);
        self.lists.borrow_mut().clear();
        let state = self.controller.store.state();
        if state.active_workspace().is_none() {
            let empty = label(
                if state.connection.is_connected() {
                    "Open a Git repository to list its Objectives."
                } else {
                    "Objectives appear when the service is reachable."
                },
                &["meta"],
            );
            self.content.append(&empty);
            self.new_button.set_sensitive(false);
            return;
        }
        self.new_button
            .set_sensitive(state.connection.is_connected());
        let board = state.board();
        if board.groups.is_empty() && board.archive.is_empty() {
            self.content.append(&label("No Objectives yet.", &["meta"]));
            return;
        }
        let mut lists = Vec::new();
        for (attention, objectives) in &board.groups {
            let group = vbox(space::TIGHT);
            let name = format::attention_label(*attention);
            let heading = widgets::heading(name, "group-heading");
            group.append(&heading);
            let list = Self::list(objectives, name);
            group.append(&list);
            self.content.append(&group);
            lists.push((
                list,
                objectives
                    .iter()
                    .map(|o| o.objective_id.clone())
                    .collect::<Vec<_>>(),
            ));
        }
        if !board.archive.is_empty() {
            let list = Self::list(&board.archive, "Completed and discarded");
            let controller = self.controller.clone();
            let archive = widgets::disclosure(
                &format!("Completed and discarded ({})", board.archive.len()),
                state.ui.archive_expanded,
                &list,
                move |expanded| controller.set_archive_expanded(expanded),
            );
            archive.add_css_class("archive");
            self.content.append(&archive);
            lists.push((
                list,
                board
                    .archive
                    .iter()
                    .map(|o| o.objective_id.clone())
                    .collect::<Vec<_>>(),
            ));
        }
        drop(state);
        for (list, ids) in &lists {
            let open = self.on_open.borrow().clone();
            list.connect_row_activated(move |_, _| {
                if let Some(open) = &open {
                    open();
                }
            });
            let controller = self.controller.clone();
            let ids = ids.clone();
            list.connect_row_selected(move |_, row| {
                if let Some(row) = row
                    && let Some(id) = usize::try_from(row.index()).ok().and_then(|i| ids.get(i))
                {
                    controller.select_objective(id);
                }
            });
        }
        *self.lists.borrow_mut() = lists;
        self.sync_selection();
    }

    fn sync_selection(&self) {
        let selected = self.controller.store.state().ui.selected.clone();
        for (list, ids) in self.lists.borrow().iter() {
            let index = selected
                .as_ref()
                .and_then(|s| ids.iter().position(|id| id == s));
            match index.and_then(|i| list.row_at_index(i32::try_from(i).ok()?)) {
                Some(row) => {
                    if list.selected_row().as_ref() != Some(&row) {
                        list.select_row(Some(&row));
                    }
                }
                None => list.unselect_all(),
            }
        }
    }
}
