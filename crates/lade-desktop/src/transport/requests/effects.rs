//! What a successful reply changes: store updates and follow-up queries per [`Context`].

use lade_protocol::client::{self as wire, ReplyData};
use lade_protocol::types::Counter;

use super::{Context, Effects, refresh_objective};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{
    self as view, Basis, NoticeLevel, NoticeScope, ObjectiveId, ServiceNotice,
};
use crate::presentation::state::store::StoreUpdate;
use crate::presentation::terminal::TerminalMode;
use crate::transport::{agent_session, convert};

fn agent_session_effects(context: &Context, data: &ReplyData, effects: &mut Effects) -> bool {
    let updates = &mut effects.updates;
    match data {
        ReplyData::ModelsList(catalog) => {
            updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Catalog(
                agent_session::catalog(catalog),
            )));
            if let Context::Models(Some(provider)) = context {
                updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Models {
                    provider: provider.clone(),
                    models: agent_session::models(catalog),
                }));
            }
        }
        ReplyData::SessionTree(tree) => {
            if let Context::Tree(objective_id) = context {
                updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Tree {
                    objective_id: objective_id.clone(),
                    tree: agent_session::tree(tree),
                }));
            }
        }
        ReplyData::SessionNavigate(state)
        | ReplyData::SessionFork(state)
        | ReplyData::SessionImport(state)
        | ReplyData::SessionCompact(state)
        | ReplyData::SessionReload(state)
        | ReplyData::SessionConfigure(state) => {
            let (objective_id, session) = agent_session::session(state);
            updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Session {
                objective_id: objective_id.clone(),
                session,
            }));
            effects.follow_ups.push(Intent::LoadSessionTree {
                objective_id: objective_id.clone(),
            });
            effects.follow_ups.push(refresh_objective(&objective_id));
        }
        ReplyData::SessionExport(export) => {
            updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Exported {
                objective_id: ObjectiveId::new(export.objective_id.as_str()),
                path: export.path.display().to_owned(),
            }));
        }
        ReplyData::ResourcesList(inventory) | ReplyData::ResourceDecide(inventory) => {
            updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Inventory(
                agent_session::inventory(inventory),
            )));
        }
        ReplyData::ProfileImport(result) => {
            updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Inventory(
                agent_session::inventory(&result.inventory),
            )));
            updates.push(StoreUpdate::Notice(ServiceNotice {
                level: NoticeLevel::Info,
                code: "profile_import".into(),
                message: format!(
                    "Imported {} item(s) into the LADE profile{}. Extensions still need approval.",
                    result.copied.len(),
                    if result.skipped.is_empty() {
                        String::new()
                    } else {
                        format!("; skipped: {}", result.skipped.join("; "))
                    }
                ),
                scope: NoticeScope::General,
            }));
            effects
                .follow_ups
                .push(Intent::LoadModels { provider: None });
        }
        ReplyData::UiRespond(_) => {}
        _ => return false,
    }
    true
}

pub fn reply_effects(context: &Context, data: &ReplyData) -> Effects {
    effects_for(context, data, Records::Current)
}

/// Effects of a command outcome recovered after the connection dropped. The
/// outcome holds the record as it was when the command completed, while current
/// pages and events may already hold newer metadata, even at the same draft
/// revision, so the changed draft or input record is left out and reconciled
/// from current service state instead.
pub fn recovered_effects(context: &Context, data: &ReplyData) -> Effects {
    effects_for(context, data, Records::AsCompleted)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Records {
    Current,
    AsCompleted,
}

fn effects_for(context: &Context, data: &ReplyData, records: Records) -> Effects {
    let mut effects = Effects::default();
    if let Context::Command {
        objective_id: Some(objective_id),
        surface,
        ..
    } = context
    {
        effects.updates.push(StoreUpdate::CommandStatus {
            objective_id: objective_id.clone(),
            status: None,
        });
        if let Some(surface) = surface {
            effects.updates.push(StoreUpdate::RefusalCleared {
                objective_id: objective_id.clone(),
                surface: *surface,
            });
        }
    }
    if !agent_session_effects(context, data, &mut effects)
        && !read_effects(context, data, &mut effects)
    {
        command_effects(context, data, records, &mut effects);
    }
    effects
}

/// Replies that load state: the snapshot, workspaces, objectives and their review views.
fn read_effects(context: &Context, data: &ReplyData, effects: &mut Effects) -> bool {
    let updates = &mut effects.updates;
    match (context, data) {
        (_, ReplyData::Snapshot(s)) => {
            updates.push(StoreUpdate::Snapshot {
                workspaces: s.workspaces.iter().map(convert::workspace).collect(),
                objectives: s.objectives.iter().map(convert::summary).collect(),
                settings: convert::settings(&s.settings),
            });
            effects
                .follow_ups
                .extend([Intent::LoadModels { provider: None }, Intent::LoadResources]);
        }
        (_, ReplyData::WorkspaceOpen(w)) => {
            updates.push(StoreUpdate::WorkspaceOpened(convert::workspace(w)));
        }
        (_, ReplyData::ObjectiveCreate(d)) => {
            let detail = convert::detail(d);
            let objective_id = detail.summary.objective_id.clone();
            updates.push(StoreUpdate::ObjectiveDetail(Box::new(detail)));
            updates.push(StoreUpdate::Select(objective_id.clone()));
            effects.follow_ups.extend([
                Intent::LoadChanges {
                    objective_id: objective_id.clone(),
                    basis: Basis::Working,
                },
                Intent::LoadDrafts {
                    objective_id: objective_id.clone(),
                },
                Intent::LoadInputs { objective_id },
            ]);
        }
        (
            _,
            ReplyData::ObjectiveGet(d)
            | ReplyData::ObjectiveUpdateIntent(d)
            | ReplyData::ExecutionStart(d)
            | ReplyData::ExecutionCancel(d)
            | ReplyData::ExecutionResume(d)
            | ReplyData::ReviewReturn(d)
            | ReplyData::ReviewIntegrate(d)
            | ReplyData::ObjectiveComplete(d)
            | ReplyData::ObjectiveDiscard(d)
            | ReplyData::ObjectiveCleanup(d)
            | ReplyData::ObjectiveDeleteHistory(d),
        ) => updates.push(StoreUpdate::ObjectiveDetail(Box::new(convert::detail(d)))),
        (Context::Activity(o), ReplyData::ActivityPage(p)) => {
            updates.push(StoreUpdate::ActivityPage {
                objective_id: o.clone(),
                items: p.items.iter().map(convert::activity_item).collect(),
                has_more: p.has_more,
            });
        }
        (Context::Changes(o, basis), ReplyData::ChangesList(l)) => {
            updates.push(StoreUpdate::Changes(convert::change_list(o, basis, l)));
        }
        (Context::Diff(o, basis, path), ReplyData::DiffGet(d)) => {
            updates.push(StoreUpdate::Diff(convert::file_diff(o, basis, path, d)));
        }
        (Context::File(o, basis, path, side), ReplyData::FileRead(c)) => {
            updates.push(StoreUpdate::FilePage(convert::file_page(
                o, basis, path, *side, c,
            )));
        }
        (
            Context::Search {
                scope,
                query,
                objective_id,
                basis,
            },
            ReplyData::Search(r),
        ) => updates.push(StoreUpdate::Search(view::SearchResults {
            scope: *scope,
            query: query.clone(),
            scope_label: r.scope_label.clone(),
            basis: basis.clone(),
            objective_id: objective_id.clone(),
            results: r.results.iter().map(convert::search_result).collect(),
            truncated: r.truncated,
        })),
        (Context::Drafts(o), ReplyData::DraftsList(l)) => updates.push(StoreUpdate::Drafts {
            objective_id: o.clone(),
            drafts: l.drafts.iter().map(convert::draft).collect(),
        }),
        (Context::Inputs(o), ReplyData::InputsList(l)) => updates.push(StoreUpdate::Inputs {
            objective_id: o.clone(),
            inputs: l.inputs.iter().map(convert::input).collect(),
        }),
        _ => return false,
    }
    true
}

/// Replies to commands. Every reply kind is listed so a new one must be classified here.
fn command_effects(context: &Context, data: &ReplyData, records: Records, effects: &mut Effects) {
    let current = records == Records::Current;
    let updates = &mut effects.updates;
    match (context, data) {
        (_, ReplyData::MessageSend(i) | ReplyData::FeedbackSubmit(i)) => {
            input_accepted(context, i, current, effects);
        }
        (_, ReplyData::DraftSave(d)) => {
            if current {
                updates.push(StoreUpdate::DraftChanged(convert::draft(d)));
            }
        }
        (Context::Command { draft_id, .. }, ReplyData::DraftDelete(_)) => {
            if let Some(draft_id) = draft_id.as_ref().filter(|_| current) {
                updates.push(StoreUpdate::DraftDeleted {
                    draft_id: draft_id.clone(),
                });
            }
        }
        (
            _,
            ReplyData::ReviewFreeze(_) | ReplyData::ReviewRefresh(_) | ReplyData::ReviewApprove(_),
        ) => {
            if let Some(o) = context.objective_id() {
                effects.follow_ups.push(refresh_objective(o));
            }
        }
        (_, ReplyData::ReviewVerify(v)) => {
            updates.push(StoreUpdate::VerificationChanged(convert::verification(v)));
        }
        (_, ReplyData::SettingsUpdate(s)) => {
            updates.push(StoreUpdate::Settings(convert::settings(s)));
        }
        (Context::Terminal(o, _), ReplyData::TerminalOpen(info)) => {
            updates.push(terminal_opened(o, info));
        }
        (_, ReplyData::ServiceStop(_)) => updates.push(StoreUpdate::ServiceStopped),
        // Handled by `read_effects` or `agent_session_effects`, or nothing to apply.
        (
            _,
            ReplyData::OperationGet(_)
            | ReplyData::TerminalInput(_)
            | ReplyData::TerminalResize(_)
            | ReplyData::TerminalClose(_)
            | ReplyData::TerminalOpen(_)
            | ReplyData::Snapshot(_)
            | ReplyData::WorkspaceOpen(_)
            | ReplyData::ObjectiveCreate(_)
            | ReplyData::ObjectiveGet(_)
            | ReplyData::ObjectiveUpdateIntent(_)
            | ReplyData::ExecutionStart(_)
            | ReplyData::ExecutionCancel(_)
            | ReplyData::ExecutionResume(_)
            | ReplyData::ReviewReturn(_)
            | ReplyData::ReviewIntegrate(_)
            | ReplyData::ObjectiveComplete(_)
            | ReplyData::ObjectiveDiscard(_)
            | ReplyData::ObjectiveCleanup(_)
            | ReplyData::ObjectiveDeleteHistory(_)
            | ReplyData::ActivityPage(_)
            | ReplyData::ChangesList(_)
            | ReplyData::DiffGet(_)
            | ReplyData::FileRead(_)
            | ReplyData::Search(_)
            | ReplyData::DraftsList(_)
            | ReplyData::InputsList(_)
            | ReplyData::DraftDelete(_)
            | ReplyData::ModelsList(_)
            | ReplyData::SessionTree(_)
            | ReplyData::ResourcesList(_)
            | ReplyData::SessionNavigate(_)
            | ReplyData::SessionFork(_)
            | ReplyData::SessionImport(_)
            | ReplyData::SessionExport(_)
            | ReplyData::SessionCompact(_)
            | ReplyData::SessionReload(_)
            | ReplyData::SessionConfigure(_)
            | ReplyData::ResourceDecide(_)
            | ReplyData::ProfileImport(_)
            | ReplyData::UiRespond(_),
        ) => {}
    }
}

/// An accepted message or feedback submission: the input record, and a reload of the drafts
/// it consumed and of the objective it changed.
fn input_accepted(
    context: &Context,
    input: &wire::InputSummary,
    current: bool,
    effects: &mut Effects,
) {
    let input = convert::input(input);
    let objective_id = input.objective_id.clone();
    if current {
        effects.updates.push(StoreUpdate::InputChanged {
            objective_id: objective_id.clone(),
            input,
        });
    }
    if matches!(context, Context::Command { batch, .. } if !batch.is_empty()) {
        effects.follow_ups.push(Intent::LoadDrafts {
            objective_id: objective_id.clone(),
        });
    }
    effects.follow_ups.push(refresh_objective(&objective_id));
}

fn terminal_opened(objective_id: &ObjectiveId, info: &wire::TerminalInfo) -> StoreUpdate {
    let size = |c: &Counter| u16::try_from(c.value()).unwrap_or(u16::MAX);
    StoreUpdate::TerminalOpened {
        objective_id: objective_id.clone(),
        terminal_id: info.terminal_id.as_str().to_owned(),
        mode: match info.mode {
            wire::TerminalMode::ReadWrite => TerminalMode::ReadWrite,
            wire::TerminalMode::ReadOnly => TerminalMode::ReadOnly,
        },
        cols: size(&info.cols),
        rows: size(&info.rows),
    }
}
