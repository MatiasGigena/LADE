use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{
    self, Dialog, MessageKind, field, hbox, label, set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::controller::{Controller, SessionUse};
use crate::presentation::state::agent_session::{
    ForkPosition, ModelChoice, ModelInfo, ModelPicker, ModelRow, PickerChoices, ProviderInfo,
    ProviderStatus, Readiness, ResourceKind, ResourceState, SessionState, SessionTree,
    ThinkingLevel, UiAnswer, UiDialog, UiPrompt,
};
use crate::presentation::state::model::{ActionAvailability, ObjectiveDetail};

const TREE_ROWS: usize = 60;

fn section(title: &str) -> gtk::Box {
    let section = vbox(space::GROUP);
    section.append(&widgets::section_title(title));
    section
}

fn picker_field(title: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let field = vbox(space::TIGHT);
    field.append(&label(title, &["meta"]));
    field.append(control);
    field
}

fn import_profile_dialog(opener: &gtk::Button, controller: &Rc<Controller>) {
    let Some(window) = opener.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Choose a pi agent directory (for example ~/.pi/agent)")
        .accept_label("Import a copy")
        .modal(true)
        .build();
    let controller = controller.clone();
    dialog.select_folder(Some(&window), None::<&gio::Cancellable>, move |result| {
        if let Ok(Some(path)) = result.map(|f| f.path()) {
            controller.import_profile(path);
        }
    });
}

const IMPORT_PROFILE: &str = "Import a pi profile…";
const IMPORT_PROFILE_TOOLTIP: &str = "Copies AGENTS.md, skills, prompts, extensions and a valid models.json; the source is never changed";

fn select(dropdown: &gtk::DropDown, index: Option<usize>) {
    if let Some(index) = index.and_then(|i| u32::try_from(i).ok()) {
        dropdown.set_selected(index);
    }
}

fn no_providers(controller: &Rc<Controller>, error: Option<&str>) -> gtk::Box {
    let block = vbox(space::GROUP);
    let sentence = match error {
        Some(_) => "LADE can't load models right now.",
        None => "No model providers are available.",
    };
    block.append(&widgets::inline_message(
        MessageKind::Warning,
        sentence,
        error,
    ));
    let actions = hbox(space::ITEM);
    let retry = widgets::button("Retry loading models", &[]);
    let c = controller.clone();
    retry.connect_clicked(move |_| c.load_models(None));
    actions.append(&retry);
    let import = widgets::button(IMPORT_PROFILE, &[]);
    import.set_tooltip_text(Some(IMPORT_PROFILE_TOOLTIP));
    let c = controller.clone();
    import.connect_clicked(move |b| import_profile_dialog(b, &c));
    actions.append(&import);
    block.append(&actions);
    block
}

fn provider_field(controller: &Rc<Controller>, picker: &PickerChoices) -> gtk::Box {
    let names: Vec<String> = picker
        .providers
        .iter()
        .map(ProviderInfo::picker_label)
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let providers = widgets::dropdown_or(&refs, "No model providers are available");
    set_accessible_label(&providers, "Provider for the agent");
    set_focus_key(&providers, "agent:provider");
    select(&providers, picker.selected_provider);
    let field = picker_field("Provider", &providers);
    if let Some(provider) = picker.provider() {
        let subtitle = provider.subtitle();
        field.append(&label(&subtitle, &["meta"]));
        widgets::set_accessible_description(&providers, &subtitle);
        if let Some(reason) = &provider.reason {
            field.append(&match provider.status {
                ProviderStatus::Available => label(reason, &["meta"]).upcast::<gtk::Widget>(),
                _ => widgets::inline_message(
                    MessageKind::Warning,
                    reason,
                    provider.credential_help().as_deref(),
                )
                .upcast(),
            });
        }
        if provider.status == ProviderStatus::MissingCredentials {
            let check = widgets::button("Check again", &[]);
            check.set_halign(gtk::Align::Start);
            check.set_tooltip_text(Some("Ask LADE again whether this provider has a key"));
            set_focus_key(&check, "agent:check-credentials");
            let (c, id) = (controller.clone(), provider.id.clone());
            check.connect_clicked(move |_| c.load_models(Some(id.clone())));
            field.append(&check);
        }
    }
    let (c, ids) = (
        controller.clone(),
        picker
            .providers
            .iter()
            .map(|p| p.id.clone())
            .collect::<Vec<_>>(),
    );
    let thinking = picker.choice.thinking;
    providers.connect_selected_notify(move |d| {
        if let Some(id) = ids.get(d.selected() as usize) {
            c.choose_model(ModelChoice {
                provider: Some(id.clone()),
                model: None,
                thinking,
            });
        }
    });
    field
}

fn model_dropdown(
    controller: &Rc<Controller>,
    picker: &PickerChoices,
    selected: Option<usize>,
) -> gtk::DropDown {
    let names: Vec<String> = picker.models.iter().map(ModelInfo::picker_label).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let models = widgets::dropdown_or(&refs, "No models available");
    set_accessible_label(&models, "Model for the agent");
    set_focus_key(&models, "agent:model");
    select(&models, selected);
    let (c, ids) = (
        controller.clone(),
        picker
            .models
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
    );
    let (provider, thinking) = (picker.choice.provider.clone(), picker.choice.thinking);
    models.connect_selected_notify(move |d| {
        if let Some(id) = ids.get(d.selected() as usize) {
            c.choose_model(ModelChoice {
                provider: provider.clone(),
                model: Some(id.clone()),
                thinking,
            });
        }
    });
    models
}

fn thinking_dropdown(
    controller: &Rc<Controller>,
    levels: Vec<ThinkingLevel>,
    selected: Option<usize>,
) -> gtk::DropDown {
    let names: Vec<&str> = levels.iter().map(|l| l.label()).collect();
    let thinking = widgets::dropdown_or(&names, "No thinking levels");
    set_accessible_label(&thinking, "Thinking level");
    set_focus_key(&thinking, "agent:thinking");
    select(&thinking, selected);
    let c = controller.clone();
    thinking.connect_selected_notify(move |d| {
        if let Some(level) = levels.get(d.selected() as usize) {
            let mut choice = c.store.state().agent_session.choice.clone();
            choice.thinking = *level;
            c.choose_model(choice);
        }
    });
    thinking
}

/// Builds the provider, model and thinking controls from the decisions in
/// [`AgentSessionState::model_picker`](crate::presentation::state::agent_session::AgentSessionState::model_picker).
pub fn model_picker(controller: &Rc<Controller>) -> gtk::Box {
    let picker = controller.store.state().agent_session.model_picker();
    let picker = match picker {
        ModelPicker::Loading => {
            let block = vbox(space::GROUP);
            block.append(&label("Loading the available models…", &["meta"]));
            return block;
        }
        ModelPicker::NoProviders { error } => return no_providers(controller, error.as_deref()),
        ModelPicker::Choices(picker) => picker,
    };
    let block = vbox(space::GROUP);
    if let Some(error) = &picker.catalog_error {
        block.append(&widgets::inline_message(
            MessageKind::Warning,
            "Some model providers may be missing.",
            Some(error),
        ));
    }
    block.append(&provider_field(controller, &picker));
    match &picker.model_row {
        ModelRow::Ready {
            selected_model,
            thinking,
            selected_thinking,
        } => {
            let models = model_dropdown(controller, &picker, *selected_model);
            block.append(&picker_field("Model", &models));
            let thinking = thinking_dropdown(controller, thinking.clone(), *selected_thinking);
            block.append(&picker_field("Thinking", &thinking));
        }
        ModelRow::Hidden => {}
        ModelRow::Problem(problem) => {
            block.append(&picker_field("Model", &label(problem, &["secondary"])));
        }
    }
    block
}

pub fn session_section(
    controller: &Rc<Controller>,
    detail: &ObjectiveDetail,
    report: &Rc<dyn Fn(Result<(), String>)>,
) -> gtk::Box {
    let s = section("Agent Session (pi)");
    let objective = detail.summary.objective_id.clone();
    let (session, tree, export) = {
        let state = controller.store.state();
        (
            state.agent_session.sessions.get(&objective).cloned(),
            state.agent_session.trees.get(&objective).cloned(),
            state.agent_session.exports.get(&objective).cloned(),
        )
    };
    session_status(&s, session.as_ref(), export.as_deref());
    s.append(&session_tools(controller, report));
    s.append(&label(
        "Fork, clone and import start a new pi session; feedback addressed to the previous one is \
         refused. Moving in the tree changes only the conversation, never files. Imports keep the \
         conversation and never restore approvals or Git state.",
        &["meta"],
    ));

    if let Some(tree) = tree {
        s.append(&session_tree(controller, &tree, report));
    }
    s
}

/// The session's model, thinking level and pi id, or a note until it reports them, and the last
/// export.
fn session_status(s: &gtk::Box, session: Option<&SessionState>, export: Option<&str>) {
    match session {
        Some(reported) => {
            s.append(&label(
                &format!(
                    "Model {} · thinking {} · this generation: {}",
                    reported.model.label(),
                    reported.thinking.label(),
                    reported.reason
                ),
                &[],
            ));
            if let Some(pi) = &reported.pi_session_id {
                let id =
                    widgets::line_label(pi, &["mono", "meta"], gtk::pango::EllipsizeMode::Middle);
                id.set_tooltip_text(Some(pi));
                s.append(&field("pi session", &id));
            }
        }
        None => s.append(&label(
            "The pi session reports its state once the agent runs.",
            &["meta"],
        )),
    }
    if let Some(path) = export {
        let line = widgets::line_label(path, &["mono", "meta"], gtk::pango::EllipsizeMode::Start);
        line.set_tooltip_text(Some(path));
        s.append(&field("Last export", &line));
    }
}

/// The session commands, each enabled when the session allows it.
fn session_tools(controller: &Rc<Controller>, report: &Rc<dyn Fn(Result<(), String>)>) -> gtk::Box {
    let tools = vbox(space::TIGHT);
    let button = |text: &str, when: SessionUse, run: Rc<dyn Fn() -> Result<(), String>>| {
        let report = Rc::clone(report);
        widgets::list_action_row(
            text,
            &controller.session_availability(when),
            false,
            move |_| {
                report(run());
            },
        )
    };
    let c = controller.clone();
    tools.append(&button(
        "Show session tree",
        SessionUse::Running,
        Rc::new(move || c.load_session_tree()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Apply model and thinking",
        SessionUse::OutsideTurn,
        Rc::new(move || c.configure_session()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Compact context",
        SessionUse::BetweenTurns,
        Rc::new(move || c.compact_session()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Reload session resources",
        SessionUse::BetweenTurns,
        Rc::new(move || c.reload_session()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Export session",
        SessionUse::Running,
        Rc::new(move || c.export_session()),
    ));
    let (c, r) = (controller.clone(), Rc::clone(report));
    let import = widgets::list_action_row(
        "Import session…",
        &controller.session_availability(SessionUse::BetweenTurns),
        false,
        move |b| {
            let Some(window) = b.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let dialog = gtk::FileDialog::builder()
                .title("Import a pi session file (JSONL)")
                .accept_label("Import")
                .modal(true)
                .build();
            let (controller, report) = (c.clone(), r.clone());
            dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
                if let Ok(Some(path)) = result.map(|f| f.path()) {
                    report(controller.import_session(path));
                }
            });
        },
    );
    tools.append(&import);
    tools
}

/// The newest entries of the session tree, each with the moves it allows.
fn session_tree(
    controller: &Rc<Controller>,
    tree: &SessionTree,
    report: &Rc<dyn Fn(Result<(), String>)>,
) -> gtk::Box {
    let between_turns = controller.session_availability(SessionUse::BetweenTurns);
    let list = vbox(space::TIGHT);
    list.append(&widgets::section_title("Session tree"));
    if tree.truncated {
        list.append(&label("Older entries are not shown.", &["meta"]));
    }
    let start = tree.entries.len().saturating_sub(TREE_ROWS);
    for node in &tree.entries[start..] {
        let row = hbox(space::ITEM);
        let current = tree.leaf_id.as_deref() == Some(node.entry_id.as_str());
        let text = format!(
            "{}{}{}: {}{}",
            "  ".repeat(node.depth.min(12)),
            if current { "● " } else { "" },
            node.kind,
            node.label,
            node.bookmark
                .as_ref()
                .map(|b| format!(" [{b}]"))
                .unwrap_or_default()
        );
        let entry = label(&text, if current { &[] } else { &["meta"] });
        entry.set_hexpand(true);
        row.append(&entry);
        let enabled = between_turns == ActionAvailability::Enabled;
        let go = widgets::button("Go here", &[]);
        go.set_sensitive(enabled);
        set_accessible_label(&go, &format!("Go to {}", node.label));
        let (c, id, r) = (controller.clone(), node.entry_id.clone(), Rc::clone(report));
        go.connect_clicked(move |_| r(c.navigate_session(&id)));
        row.append(&go);
        if node.user {
            let fork = widgets::button("Fork", &[]);
            fork.set_sensitive(enabled);
            fork.set_tooltip_text(Some("New session with the history before this message"));
            let (c, id, r) = (controller.clone(), node.entry_id.clone(), Rc::clone(report));
            fork.connect_clicked(move |_| r(c.fork_session(&id, ForkPosition::Before)));
            row.append(&fork);
        }
        let clone = widgets::button("Clone", &[]);
        clone.set_sensitive(enabled);
        clone.set_tooltip_text(Some("New session with the history up to here"));
        let (c, id, r) = (controller.clone(), node.entry_id.clone(), Rc::clone(report));
        clone.connect_clicked(move |_| r(c.fork_session(&id, ForkPosition::At)));
        row.append(&clone);
        list.append(&row);
    }
    list
}

pub const RESOURCES_TITLE: &str = "LADE profile resources";

pub fn resources_section(controller: &Rc<Controller>) -> gtk::Box {
    let s = vbox(space::GROUP);
    let Some(inventory) = controller.store.state().agent_session.inventory.clone() else {
        s.append(&label("Loading the LADE profile…", &["meta"]));
        return s;
    };
    let dir = widgets::line_label(
        &inventory.profile_dir,
        &["mono", "meta"],
        gtk::pango::EllipsizeMode::Start,
    );
    dir.set_tooltip_text(Some(&inventory.profile_dir));
    s.append(&field("Profile", &dir));
    if inventory.entries.is_empty() {
        s.append(&label(
            "No skills, prompt templates, context file or extensions yet.",
            &["meta"],
        ));
    }
    for entry in &inventory.entries {
        let row = vbox(space::TIGHT);
        row.append(&label(
            &format!(
                "{}: {} — {}",
                entry.kind.label(),
                entry.name,
                entry.state.label()
            ),
            if entry.state == ResourceState::Active {
                &[]
            } else {
                &["meta"]
            },
        ));
        if let Some(sha) = &entry.sha256 {
            let hash =
                widgets::line_label(sha, &["mono", "meta"], gtk::pango::EllipsizeMode::Middle);
            hash.set_tooltip_text(Some(sha));
            row.append(&hash);
        }
        if let Some(reason) = &entry.reason {
            row.append(&match entry.state {
                ResourceState::PendingApproval | ResourceState::Refused => {
                    widgets::warning_line(reason).upcast::<gtk::Widget>()
                }
                _ => label(reason, &["meta"]).upcast(),
            });
        }
        if entry.kind == ResourceKind::Extension
            && let Some(sha) = entry.sha256.clone()
            && entry.state != ResourceState::Refused
        {
            let buttons = hbox(space::ITEM);
            if entry.state != ResourceState::Active {
                let approve = widgets::button("Approve this content", &["primary"]);
                let (c, name, sha) = (controller.clone(), entry.name.clone(), sha.clone());
                approve.connect_clicked(move |_| c.decide_resource(&name, &sha, true));
                buttons.append(&approve);
            }
            if entry.state != ResourceState::Rejected {
                let reject = widgets::button("Reject", &[]);
                let (c, name) = (controller.clone(), entry.name.clone());
                reject.connect_clicked(move |_| c.decide_resource(&name, &sha, false));
                buttons.append(&reject);
            }
            row.append(&buttons);
        }
        s.append(&row);
    }
    if !inventory.restrictions.is_empty() {
        let list = vbox(space::TIGHT);
        for restriction in &inventory.restrictions {
            list.append(&label(restriction, &["meta"]));
        }
        s.append(&widgets::disclosure(
            "Trust rules and restrictions",
            false,
            &list,
            |_| {},
        ));
    }
    let actions = hbox(space::ITEM);
    let no_providers = matches!(
        controller.store.state().agent_session.readiness(),
        Readiness::NoProviders { .. }
    );
    if !no_providers {
        let c = controller.clone();
        let import = widgets::button(IMPORT_PROFILE, &[]);
        import.set_tooltip_text(Some(IMPORT_PROFILE_TOOLTIP));
        import.connect_clicked(move |b| import_profile_dialog(b, &c));
        actions.append(&import);
    }
    let c = controller.clone();
    let refresh = widgets::button("Refresh resources", &[]);
    refresh.connect_clicked(move |_| c.load_resources());
    actions.append(&refresh);
    s.append(&actions);
    s
}

#[derive(Default)]
pub struct DialogHost {
    open: RefCell<HashMap<String, gtk::Window>>,
}

impl DialogHost {
    pub fn sync(&self, opener: &impl IsA<gtk::Widget>, controller: &Rc<Controller>) {
        let dialogs = controller.store.state().agent_session.dialogs.clone();
        let stale: Vec<String> = self
            .open
            .borrow()
            .keys()
            .filter(|id| !dialogs.iter().any(|d| &d.ui_id == *id))
            .cloned()
            .collect();
        for id in stale {
            if let Some(window) = self.open.borrow_mut().remove(&id) {
                window.close();
            }
        }
        for dialog in dialogs {
            if self.open.borrow().contains_key(&dialog.ui_id) {
                continue;
            }
            let window = present(opener, controller, &dialog);
            self.open.borrow_mut().insert(dialog.ui_id.clone(), window);
        }
    }
}

fn present(
    opener: &impl IsA<gtk::Widget>,
    controller: &Rc<Controller>,
    dialog: &UiDialog,
) -> gtk::Window {
    let shell = Dialog::new(opener, dialog.prompt.title(), 480);
    shell.body.append(&label(
        &format!(
            "An extension asks (session generation {}):",
            dialog.generation
        ),
        &["meta"],
    ));
    let answered = Rc::new(std::cell::Cell::new(false));
    let send = {
        let controller = controller.clone();
        let ui_id = dialog.ui_id.clone();
        let answered = answered.clone();
        let window = shell.window.clone();
        Rc::new(move |answer: UiAnswer| {
            answered.set(true);
            let _ = controller.answer_dialog(&ui_id, answer);
            window.close();
        })
    };
    let value: Rc<dyn Fn() -> UiAnswer> = match &dialog.prompt {
        UiPrompt::Confirm { message, .. } => {
            shell.body.append(&label(message, &[]));
            Rc::new(|| UiAnswer::Confirm(true))
        }
        UiPrompt::Select { options, .. } => {
            let refs: Vec<&str> = options.iter().map(String::as_str).collect();
            let choice = widgets::dropdown_or(&refs, "The extension offered no choices");
            set_accessible_label(&choice, "Choice");
            shell.body.append(&choice);
            let options = options.clone();
            Rc::new(move || {
                options
                    .get(choice.selected() as usize)
                    .cloned()
                    .map_or(UiAnswer::Cancel, UiAnswer::Value)
            })
        }
        UiPrompt::Input { placeholder, .. } => {
            let entry = gtk::Entry::builder().placeholder_text(placeholder).build();
            set_accessible_label(&entry, "Answer");
            shell.body.append(&entry);
            Rc::new(move || UiAnswer::Value(entry.text().to_string()))
        }
        UiPrompt::Editor { prefill, .. } => {
            let view = gtk::TextView::new();
            view.buffer().set_text(prefill);
            set_accessible_label(&view, "Answer");
            shell.body.append(&widgets::bounded_text(&view, 120, 320));
            Rc::new(move || {
                let buffer = view.buffer();
                UiAnswer::Value(
                    buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string(),
                )
            })
        }
    };
    let confirm = matches!(dialog.prompt, UiPrompt::Confirm { .. });
    let deny = shell.add_button(if confirm { "Deny" } else { "Cancel" }, &[]);
    let s = send.clone();
    deny.connect_clicked(move |_| {
        s(if confirm {
            UiAnswer::Confirm(false)
        } else {
            UiAnswer::Cancel
        });
    });
    let ok = shell.add_button(if confirm { "Allow" } else { "Answer" }, &["primary"]);
    let s = send.clone();
    ok.connect_clicked(move |_| s(value()));
    let (controller, ui_id) = (controller.clone(), dialog.ui_id.clone());
    shell.window.connect_close_request(move |_| {
        if !answered.replace(true) {
            let _ = controller.answer_dialog(&ui_id, UiAnswer::Cancel);
        }
        gtk::glib::Propagation::Proceed
    });
    deny.grab_focus();
    shell.present();
    shell.window
}
