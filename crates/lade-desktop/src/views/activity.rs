//! Activity: the Agent Session transcript with incremental appends and in-place delta updates,
//! plus a composer (Prompt / Steer / Follow-up) and Cancel / Resume. New items never take focus.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{self, action_row, hbox, label, set_accessible_label, set_focus_key, vbox};
use crate::presentation::controller::Controller;
use crate::presentation::format;
use crate::presentation::model::{
    ActionName, ActivityItem, ActivityRole, DeliveryMode, ObjectiveId,
};
use crate::presentation::store::{ActivityChange, Change};
use crate::presentation::text_bound::TEXT_LIMIT;

const MODES: [DeliveryMode; 3] = [
    DeliveryMode::Prompt,
    DeliveryMode::Steer,
    DeliveryMode::FollowUp,
];

pub struct Activity {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    title: gtk::Label,
    status: gtk::Label,
    controls: gtk::Box,
    earlier: gtk::Button,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    text_labels: RefCell<Vec<gtk::Label>>,
    shown_for: RefCell<Option<ObjectiveId>>,
    pub composer: gtk::TextView,
    composer_input: Rc<widgets::LimitedInput>,
    mode: gtk::DropDown,
    send_area: gtk::Box,
    composer_error: gtk::Box,
    updating: Cell<bool>,
    follow: Cell<bool>,
    auto_scrolling: Cell<bool>,
    scroll_pending: Cell<bool>,
}

impl Activity {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = vbox(16);
        root.add_css_class("activity");
        let head = hbox(16);
        let titles = vbox(4);
        titles.set_hexpand(true);
        let title = widgets::heading("Activity", "objective-heading");
        set_focus_key(&title, "activity:title");
        title.set_focusable(true);
        titles.append(&title);
        let status = label("", &["meta"]);
        titles.append(&status);
        head.append(&titles);
        let controls = hbox(8);
        controls.set_hexpand(false);
        controls.set_valign(gtk::Align::Start);
        head.append(&controls);
        root.append(&head);

        let earlier = widgets::button("Load earlier activity", &["quiet"]);
        earlier.set_halign(gtk::Align::Start);
        root.append(&earlier);
        let list = gtk::ListBox::new();
        list.add_css_class("transcript");
        list.set_selection_mode(gtk::SelectionMode::None);
        set_accessible_label(&list, "Agent Session transcript");
        let scroll = widgets::region_scroll(&list);
        scroll.set_vexpand(true);
        scroll.set_min_content_height(160);
        root.append(&scroll);

        let composer_box = vbox(8);
        composer_box.add_css_class("composer");
        let composer = gtk::TextView::new();
        set_accessible_label(&composer, "Message to the agent");
        set_focus_key(&composer, "activity:composer");
        let composer_input =
            widgets::LimitedInput::new(&composer, TEXT_LIMIT, "this message to the agent");
        composer_box.append(&composer_input.field(&widgets::bounded_text(&composer, 64, 160)));
        let row = hbox(12);
        let mode = gtk::DropDown::from_strings(&["Prompt", "Steer", "Follow-up"]);
        set_accessible_label(&mode, "Delivery mode");
        mode.set_valign(gtk::Align::Start);
        row.append(&mode);
        let explain = label("", &["meta"]);
        explain.set_hexpand(true);
        row.append(&explain);
        let send_area = vbox(4);
        send_area.set_hexpand(false);
        send_area.set_size_request(160, -1);
        row.append(&send_area);
        composer_box.append(&row);
        let composer_error = vbox(0);
        composer_box.append(&composer_error);
        root.append(&composer_box);

        let activity = Rc::new(Self {
            root,
            controller: controller.clone(),
            title,
            status,
            controls,
            earlier,
            list,
            scroll,
            text_labels: RefCell::new(Vec::new()),
            shown_for: RefCell::new(None),
            composer,
            composer_input,
            mode,
            send_area,
            composer_error,
            updating: Cell::new(false),
            follow: Cell::new(true),
            auto_scrolling: Cell::new(false),
            scroll_pending: Cell::new(false),
        });
        let set_explain = move |d: &gtk::DropDown| {
            explain.set_text(match MODES.get(d.selected() as usize) {
                Some(DeliveryMode::Steer) => {
                    "Delivered after the current tools finish; not an instant stop."
                }
                Some(DeliveryMode::FollowUp) => {
                    "Delivered after the agent finishes its current work."
                }
                _ => "Starts a turn when the agent is idle.",
            });
        };
        set_explain(&activity.mode);
        let this = Rc::downgrade(&activity);
        activity.mode.connect_selected_notify(move |d| {
            set_explain(d);
            if let Some(this) = this.upgrade()
                && !this.updating.get()
            {
                this.store_composer();
            }
        });
        let this = Rc::downgrade(&activity);
        activity.composer_input.connect_commit(move |_| {
            if let Some(this) = this.upgrade() {
                this.store_composer();
            }
        });
        // Follow the newest item only while the reader is at the bottom; never move focus.
        let adjustment = activity.scroll.vadjustment();
        let this = Rc::downgrade(&activity);
        adjustment.connect_value_changed(move |adj| {
            if let Some(this) = this.upgrade()
                && !this.auto_scrolling.get()
            {
                this.follow
                    .set(adj.value() + adj.page_size() >= adj.upper() - 8.0);
            }
        });
        // Adjustments change during size allocation; apply the new position on the next idle so
        // the viewport re-allocates (setting it inside the notification leaves a stale view).
        let follow_tail = {
            let this = Rc::downgrade(&activity);
            move |_: &gtk::Adjustment| {
                let Some(this) = this.upgrade() else { return };
                if !this.follow.get() || this.scroll_pending.replace(true) {
                    return;
                }
                let weak = Rc::downgrade(&this);
                gtk::glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        this.scroll_pending.set(false);
                        let adj = this.scroll.vadjustment();
                        this.auto_scrolling.set(true);
                        adj.set_value(adj.upper() - adj.page_size());
                        this.auto_scrolling.set(false);
                    }
                });
            }
        };
        adjustment.connect_upper_notify(follow_tail.clone());
        adjustment.connect_page_size_notify(follow_tail);
        let controller = controller.clone();
        activity
            .earlier
            .connect_clicked(move |_| controller.load_older_activity());
        activity.render_header();
        activity.render_items();
        activity
    }

    fn composer_text(&self) -> String {
        self.composer_input.text()
    }

    fn store_composer(&self) {
        let mode = MODES[self.mode.selected() as usize % MODES.len()];
        self.controller.set_composer(&self.composer_text(), mode);
    }

    pub fn send(&self) {
        let mode = MODES[self.mode.selected() as usize % MODES.len()];
        widgets::clear(&self.composer_error);
        match self.controller.send_message(&self.composer_text(), mode) {
            Ok(()) => {
                self.composer_input.set_text("");
                self.store_composer();
            }
            Err(message) => self.composer_error.append(&widgets::error_line(&message)),
        }
    }

    pub fn composer_has_focus(&self) -> bool {
        // `is_focus`: the window's focus widget, also while the toplevel is not the active window.
        self.composer.is_focus()
    }

    pub fn refresh(self: &Rc<Self>, changes: &[Change]) {
        let selected = self.controller.store.state().ui.selected.clone();
        for change in changes {
            match change {
                Change::Selection => {
                    self.restore_composer();
                    self.render_header();
                    self.render_items();
                }
                Change::Detail(id) if Some(id) == selected.as_ref() => self.render_header(),
                Change::Connection => self.render_header(),
                Change::Activity(id, kind) if Some(id) == selected.as_ref() => match kind {
                    ActivityChange::Reset => self.render_items(),
                    ActivityChange::Appended { from_index } => self.append_items(*from_index),
                    ActivityChange::Delta { index } => self.apply_delta(*index),
                },
                _ => {}
            }
        }
    }

    fn restore_composer(&self) {
        let (text, mode) = self
            .controller
            .store
            .state()
            .selected_ui()
            .map(|u| (u.composer_text.clone(), u.composer_mode))
            .unwrap_or((String::new(), DeliveryMode::Prompt));
        self.composer_input.set_text(&text);
        self.updating.set(true);
        if let Some(index) = MODES.iter().position(|m| *m == mode) {
            self.mode.set_selected(index as u32);
        }
        self.updating.set(false);
    }

    fn render_header(self: &Rc<Self>) {
        let state = self.controller.store.state();
        let title = state.selected_summary().map_or_else(
            || "Activity".to_owned(),
            |s| format!("Activity — {}", s.title),
        );
        let detail = state.selected_detail().cloned();
        drop(state);
        self.title.set_text(&title);
        self.status.set_text(
            &self
                .controller
                .execution_line()
                .unwrap_or_else(|| "Select an Objective to see its Agent Session.".to_owned()),
        );
        widgets::clear(&self.controls);
        widgets::clear(&self.send_area);
        let Some(detail) = detail else { return };
        for (text, kind) in [
            ("Cancel agent", ActionName::ExecutionCancel),
            ("Resume agent", ActionName::ExecutionResume),
        ] {
            let controller = self.controller.clone();
            let row = action_row(text, &detail.availability(kind), &[], move |_| {
                let _ = match kind {
                    ActionName::ExecutionCancel => controller.cancel(),
                    _ => controller.resume(),
                };
            });
            row.set_size_request(160, -1);
            self.controls.append(&row);
        }
        let this = Rc::downgrade(self);
        self.send_area.append(&action_row(
            "Send",
            &detail.availability(ActionName::MessageSend),
            &["primary"],
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.send();
                }
            },
        ));
    }

    fn item_row(item: &ActivityItem) -> (gtk::ListBoxRow, gtk::Label) {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.add_css_class("activity-item");
        if item.role == ActivityRole::User {
            row.add_css_class("from-user");
        }
        let content = vbox(4);
        let meta = hbox(8);
        let role = format::role_label(item.role, item.tool_name.as_deref());
        meta.append(&label(&role, &["strong", "role"]));
        if !item.title.is_empty() && item.title != role && item.role != ActivityRole::Assistant {
            meta.append(&widgets::line_label(
                &item.title,
                &["meta"],
                gtk::pango::EllipsizeMode::End,
            ));
        }
        if item.role == ActivityRole::Assistant {
            meta.append(&label("agent claim", &["meta"]));
        }
        if item.is_error {
            meta.append(&label("✕ Error", &["removed", "strong"]));
        }
        content.append(&meta);
        let mut classes = vec!["activity-text"];
        if item.role == ActivityRole::Tool {
            classes.push("mono");
        }
        if item.role == ActivityRole::Thinking {
            classes.push("secondary");
        }
        let text = label(&item.text, &classes);
        text.set_selectable(true);
        text.set_focusable(false);
        content.append(&text);
        if item.truncated {
            content.append(&widgets::warning_line(
                "Truncated — this item exceeded the retained size; the shown text is incomplete.",
            ));
        }
        row.set_child(Some(&content));
        let mut spoken = role;
        if item.is_error {
            spoken.push_str(", error");
        }
        if item.truncated {
            spoken.push_str(", truncated");
        }
        set_accessible_label(&row, &spoken);
        (row, text)
    }

    fn render_items(&self) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        self.text_labels.borrow_mut().clear();
        let state = self.controller.store.state();
        let id = state.ui.selected.clone();
        *self.shown_for.borrow_mut() = id.clone();
        let Some(id) = id else {
            self.earlier.set_visible(false);
            return;
        };
        let log = state.activity.get(&id).cloned().unwrap_or_default();
        drop(state);
        self.earlier.set_visible(log.has_more);
        if !log.loaded {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label("Loading activity…", &["meta"])));
            self.list.append(&row);
            return;
        }
        if log.items.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label("No activity yet.", &["meta"])));
            self.list.append(&row);
        }
        for item in &log.items {
            let (row, text) = Self::item_row(item);
            self.list.append(&row);
            self.text_labels.borrow_mut().push(text);
        }
        self.follow.set(true);
    }

    fn append_items(&self, from_index: usize) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.clone() else {
            return;
        };
        let Some(log) = state.activity.get(&id) else {
            return;
        };
        if self.text_labels.borrow().len() != from_index {
            drop(state);
            self.render_items();
            return;
        }
        for item in &log.items[from_index..] {
            let (row, text) = Self::item_row(item);
            self.list.append(&row);
            self.text_labels.borrow_mut().push(text);
        }
        drop(state);
    }

    fn apply_delta(&self, index: usize) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.as_ref() else {
            return;
        };
        let Some(item) = state.activity.get(id).and_then(|l| l.items.get(index)) else {
            return;
        };
        if let Some(text) = self.text_labels.borrow().get(index) {
            text.set_text(&item.text);
        }
        drop(state);
    }
}
