//! IMP-011 views: the model and thinking picker over the service's catalogue (provider status
//! and restriction reasons are always shown), the Agent Session section (pi generation, tree with
//! go-to/fork/clone, compaction, reload, export/import), the LADE-profile resources with explicit
//! approval of executable extensions, and extension dialogs answered only by the developer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{
    self, Dialog, MessageKind, field, hbox, label, set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::compat::{
    ForkPosition, ModelChoice, ProviderStatus, Readiness, ResourceKind, ResourceState,
    ThinkingLevel, UiAnswerView, UiDialog, UiPromptView,
};
use crate::presentation::controller::Controller;
use crate::presentation::model::{ActionAvailability, ExecutionState, ObjectiveDetail};

/// Tree entries shown inline (newest last); the full tree stays in pi.
const TREE_ROWS: usize = 60;

fn section(title: &str) -> gtk::Box {
    let section = vbox(space::GROUP);
    section.append(&widgets::section_title(title));
    section
}

/// A picker field: its metadata label above the control, grouped tightly.
fn picker_field(title: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let field = vbox(space::TIGHT);
    field.append(&label(title, &["meta"]));
    field.append(control);
    field
}

/// Opens the folder chooser for a pi agent directory and imports a copy into the LADE profile.
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

/// Provider, model and thinking drop-downs bound to the controller's picker choice (which the
/// controller keeps defaulted; this view only reads it). Each missing piece is stated in its own
/// words: the catalogue loading or unavailable, no providers, the provider's models loading, a
/// provider without models. Thinking is offered once a model is chosen.
pub fn model_picker(controller: &Rc<Controller>) -> gtk::Box {
    let block = vbox(space::GROUP);
    let state = controller.store.state();
    let compat = &state.compat;
    let Some(catalog) = compat.catalog.clone() else {
        block.append(&label("Loading the available models…", &["meta"]));
        return block;
    };
    let readiness = compat.readiness();
    let choice = compat.choice.clone();
    let models = compat.chosen_models().to_vec();
    drop(state);

    if catalog.providers.is_empty() {
        // Nothing to pick: one calm message (the service's text in its Details) and the ways on.
        let sentence = match &catalog.error {
            Some(_) => "LADE can't load models right now.",
            None => "No model providers are available.",
        };
        block.append(&widgets::inline_message(
            MessageKind::Warning,
            sentence,
            catalog.error.as_deref(),
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
        return block;
    }
    if let Some(error) = &catalog.error {
        block.append(&widgets::inline_message(
            MessageKind::Warning,
            "Some model providers may be missing.",
            Some(error),
        ));
    }

    let names: Vec<String> = catalog.providers.iter().map(|p| p.picker_label()).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let providers = widgets::dropdown_or(&refs, "No model providers are available");
    set_accessible_label(&providers, "Provider for the agent");
    set_focus_key(&providers, "agent:provider");
    if let Some(index) = choice
        .provider
        .as_ref()
        .and_then(|p| catalog.providers.iter().position(|v| &v.id == p))
        .and_then(|i| u32::try_from(i).ok())
    {
        providers.set_selected(index);
    }
    let provider_field = picker_field("Provider", &providers);
    if let Some(provider) = choice.provider.as_ref().and_then(|p| catalog.provider(p)) {
        let subtitle = provider.subtitle();
        provider_field.append(&label(&subtitle, &["meta"]));
        widgets::set_accessible_description(&providers, &subtitle);
        // The service's reason is one sentence; where a missing key goes is the collapsed Details.
        if let Some(reason) = &provider.reason {
            provider_field.append(&match provider.status {
                ProviderStatus::Available => label(reason, &["meta"]).upcast::<gtk::Widget>(),
                _ => widgets::inline_message(
                    MessageKind::Warning,
                    reason,
                    provider.credential_help().as_deref(),
                )
                .upcast(),
            });
        }
        // After adding a key, the service classifies the provider again on the next request.
        if provider.status == ProviderStatus::MissingCredentials {
            let check = widgets::button("Check again", &[]);
            check.set_halign(gtk::Align::Start);
            check.set_tooltip_text(Some("Ask LADE again whether this provider has a key"));
            set_focus_key(&check, "agent:check-credentials");
            let (c, id) = (controller.clone(), provider.id.clone());
            check.connect_clicked(move |_| c.load_models(Some(id.clone())));
            provider_field.append(&check);
        }
    }
    block.append(&provider_field);
    let catalog_for_providers = catalog.clone();
    let controller_for_providers = controller.clone();
    let thinking_now = choice.thinking;
    providers.connect_selected_notify(move |d| {
        if let Some(provider) = catalog_for_providers.providers.get(d.selected() as usize) {
            controller_for_providers.choose_model(ModelChoice {
                provider: Some(provider.id.clone()),
                model: None,
                thinking: thinking_now,
            });
        }
    });

    let model_names: Vec<String> = models
        .iter()
        .map(|m| {
            if m.reasoning {
                format!("{} (reasoning)", m.name)
            } else {
                m.name.clone()
            }
        })
        .collect();
    let model_refs: Vec<&str> = model_names.iter().map(String::as_str).collect();
    let model_choice = widgets::dropdown_or(&model_refs, "No models available");
    set_accessible_label(&model_choice, "Model for the agent");
    set_focus_key(&model_choice, "agent:model");
    if let Some(index) = choice
        .model
        .as_ref()
        .and_then(|id| models.iter().position(|m| &m.id == id))
        .and_then(|i| u32::try_from(i).ok())
    {
        model_choice.set_selected(index);
    }
    let ready_model = match &readiness {
        Readiness::Ready { model, .. } => Some(model.clone()),
        _ => None,
    };
    // A provider that needs credentials or is restricted is stated under the provider already.
    let provider_problem = matches!(
        readiness,
        Readiness::NeedsCredentials { .. } | Readiness::Restricted { .. }
    );
    match (&ready_model, readiness.problem()) {
        (Some(_), _) => block.append(&picker_field("Model", &model_choice)),
        (None, Some(_)) if provider_problem => {}
        (None, Some(problem)) => {
            block.append(&picker_field("Model", &label(&problem, &["secondary"])));
        }
        (None, None) => {}
    }
    // Thinking applies to a chosen model only.
    let thinking_levels: Vec<ThinkingLevel> = ready_model
        .as_ref()
        .map(|m| m.thinking.clone())
        .filter(|levels| !levels.is_empty())
        .unwrap_or_else(|| ThinkingLevel::ALL.to_vec());
    let thinking_names: Vec<&str> = thinking_levels.iter().map(|l| l.label()).collect();
    let thinking = widgets::dropdown_or(&thinking_names, "No thinking levels");
    set_accessible_label(&thinking, "Thinking level");
    set_focus_key(&thinking, "agent:thinking");
    if let Some(index) = thinking_levels
        .iter()
        .position(|l| *l == choice.thinking)
        .and_then(|i| u32::try_from(i).ok())
    {
        thinking.set_selected(index);
    }
    if ready_model.is_some() {
        block.append(&picker_field("Thinking", &thinking));
    }

    let models_for_choice = models.clone();
    let controller_for_models = controller.clone();
    let provider_now = choice.provider.clone();
    model_choice.connect_selected_notify(move |d| {
        if let Some(model) = models_for_choice.get(d.selected() as usize) {
            controller_for_models.choose_model(ModelChoice {
                provider: provider_now.clone(),
                model: Some(model.id.clone()),
                thinking: thinking_now,
            });
        }
    });
    let controller_for_thinking = controller.clone();
    thinking.connect_selected_notify(move |d| {
        if let Some(level) = thinking_levels.get(d.selected() as usize) {
            let mut choice = controller_for_thinking.store.state().compat.choice.clone();
            choice.thinking = *level;
            controller_for_thinking.choose_model(choice);
        }
    });
    block
}

fn availability(detail: &ObjectiveDetail, allow_running: bool) -> ActionAvailability {
    match detail.summary.execution.state {
        _ if detail.session.is_none() => ActionAvailability::NotOffered,
        ExecutionState::Idle => ActionAvailability::Enabled,
        ExecutionState::Running if allow_running => ActionAvailability::Enabled,
        ExecutionState::Running => ActionAvailability::Disabled {
            reason: "Wait until the current turn has finished".into(),
        },
        _ => ActionAvailability::Disabled {
            reason: "Start or resume the agent first".into(),
        },
    }
}

/// The Agent Session section: pi session facts and operations. `report` shows a local error.
pub fn session_section(
    controller: &Rc<Controller>,
    detail: &ObjectiveDetail,
    report: Rc<dyn Fn(Result<(), String>)>,
) -> gtk::Box {
    let s = section("Agent Session (pi)");
    let objective = detail.summary.objective_id.clone();
    let (session, tree, export) = {
        let state = controller.store.state();
        (
            state.compat.sessions.get(&objective).cloned(),
            state.compat.trees.get(&objective).cloned(),
            state.compat.exports.get(&objective).cloned(),
        )
    };
    match &session {
        Some(view) => {
            // Session ids and the execution generation are the state line's tooltip.
            s.append(&label(
                &format!(
                    "Model {} · thinking {} · this generation: {}",
                    view.model.label(),
                    view.thinking.label(),
                    view.reason
                ),
                &[],
            ));
            if let Some(pi) = &view.pi_session_id {
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
    if let Some(path) = &export {
        let line = widgets::line_label(path, &["mono", "meta"], gtk::pango::EllipsizeMode::Start);
        line.set_tooltip_text(Some(path));
        s.append(&field("Last export", &line));
    }

    let tools = vbox(space::TIGHT);
    let button = |text: &str, allow_running: bool, run: Rc<dyn Fn() -> Result<(), String>>| {
        let report = report.clone();
        widgets::list_action_row(
            text,
            &availability(detail, allow_running),
            false,
            move |_| {
                report(run());
            },
        )
    };
    let c = controller.clone();
    tools.append(&button(
        "Show session tree",
        true,
        Rc::new(move || c.load_session_tree()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Apply model and thinking",
        false,
        Rc::new(move || c.configure_session()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Compact context",
        false,
        Rc::new(move || c.compact_session()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Reload session resources",
        false,
        Rc::new(move || c.reload_session()),
    ));
    let c = controller.clone();
    tools.append(&button(
        "Export session",
        true,
        Rc::new(move || c.export_session()),
    ));
    let c = controller.clone();
    let import = widgets::list_action_row(
        "Import session…",
        &availability(detail, false),
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
            let controller = c.clone();
            dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
                if let Ok(Some(path)) = result.map(|f| f.path()) {
                    let _ = controller.import_session(path);
                }
            });
        },
    );
    tools.append(&import);
    s.append(&tools);
    s.append(&label(
        "Fork, clone and import start a new pi session; feedback addressed to the previous one is \
         refused. Moving in the tree changes only the conversation, never files. Imports keep the \
         conversation and never restore approvals or Git state.",
        &["meta"],
    ));

    if let Some(tree) = tree {
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
            let enabled = availability(detail, false) == ActionAvailability::Enabled;
            let go = widgets::button("Go here", &[]);
            go.set_sensitive(enabled);
            set_accessible_label(&go, &format!("Go to {}", node.label));
            let (c, id, r) = (controller.clone(), node.entry_id.clone(), report.clone());
            go.connect_clicked(move |_| r(c.navigate_session(&id)));
            row.append(&go);
            if node.user {
                let fork = widgets::button("Fork", &[]);
                fork.set_sensitive(enabled);
                fork.set_tooltip_text(Some("New session with the history before this message"));
                let (c, id, r) = (controller.clone(), node.entry_id.clone(), report.clone());
                fork.connect_clicked(move |_| r(c.fork_session(&id, ForkPosition::Before)));
                row.append(&fork);
            }
            let clone = widgets::button("Clone", &[]);
            clone.set_sensitive(enabled);
            clone.set_tooltip_text(Some("New session with the history up to here"));
            let (c, id, r) = (controller.clone(), node.entry_id.clone(), report.clone());
            clone.connect_clicked(move |_| r(c.fork_session(&id, ForkPosition::At)));
            row.append(&clone);
            list.append(&row);
        }
        s.append(&list);
    }
    s
}

/// Title (and accessible name) of the LADE-profile resources, wherever Activity places them.
pub const RESOURCES_TITLE: &str = "LADE profile resources";

/// The LADE-profile resources (workspace-wide) with explicit decisions for extensions, without a
/// title: Activity places it under a section title or a disclosure named [`RESOURCES_TITLE`].
pub fn resources_section(controller: &Rc<Controller>) -> gtk::Box {
    let s = vbox(space::GROUP);
    let Some(inventory) = controller.store.state().compat.inventory.clone() else {
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
    // With no model providers the model picker beside this already offers the import.
    let no_providers = matches!(
        controller.store.state().compat.readiness(),
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

/// Open extension-dialog windows by dialog id. A window closed by the developer answers
/// `Cancel`; a dialog closed by the service closes its window without answering.
#[derive(Default)]
pub struct DialogHost {
    open: RefCell<HashMap<String, gtk::Window>>,
}

impl DialogHost {
    pub fn sync(&self, opener: &impl IsA<gtk::Widget>, controller: &Rc<Controller>) {
        let dialogs = controller.store.state().compat.dialogs.clone();
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
        Rc::new(move |answer: UiAnswerView| {
            answered.set(true);
            let _ = controller.answer_dialog(&ui_id, answer);
            window.close();
        })
    };
    let value: Rc<dyn Fn() -> UiAnswerView> = match &dialog.prompt {
        UiPromptView::Confirm { message, .. } => {
            shell.body.append(&label(message, &[]));
            Rc::new(|| UiAnswerView::Confirm(true))
        }
        UiPromptView::Select { options, .. } => {
            let refs: Vec<&str> = options.iter().map(String::as_str).collect();
            let choice = widgets::dropdown_or(&refs, "The extension offered no choices");
            set_accessible_label(&choice, "Choice");
            shell.body.append(&choice);
            let options = options.clone();
            Rc::new(move || {
                options
                    .get(choice.selected() as usize)
                    .cloned()
                    .map_or(UiAnswerView::Cancel, UiAnswerView::Value)
            })
        }
        UiPromptView::Input { placeholder, .. } => {
            let entry = gtk::Entry::builder().placeholder_text(placeholder).build();
            set_accessible_label(&entry, "Answer");
            shell.body.append(&entry);
            Rc::new(move || UiAnswerView::Value(entry.text().to_string()))
        }
        UiPromptView::Editor { prefill, .. } => {
            let view = gtk::TextView::new();
            view.buffer().set_text(prefill);
            set_accessible_label(&view, "Answer");
            shell.body.append(&widgets::bounded_text(&view, 120, 320));
            Rc::new(move || {
                let buffer = view.buffer();
                UiAnswerView::Value(
                    buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string(),
                )
            })
        }
    };
    let confirm = matches!(dialog.prompt, UiPromptView::Confirm { .. });
    let deny = shell.add_button(if confirm { "Deny" } else { "Cancel" }, &[]);
    let s = send.clone();
    deny.connect_clicked(move |_| {
        s(if confirm {
            UiAnswerView::Confirm(false)
        } else {
            UiAnswerView::Cancel
        });
    });
    let ok = shell.add_button(if confirm { "Allow" } else { "Answer" }, &["primary"]);
    let s = send.clone();
    ok.connect_clicked(move |_| s(value()));
    let (controller, ui_id) = (controller.clone(), dialog.ui_id.clone());
    shell.window.connect_close_request(move |_| {
        if !answered.replace(true) {
            // Closing the window is the developer's explicit dismissal: answered as cancelled.
            let _ = controller.answer_dialog(&ui_id, UiAnswerView::Cancel);
        }
        gtk::glib::Propagation::Proceed
    });
    deny.grab_focus();
    shell.present();
    shell.window
}
