//! What a successful reply changes: store updates and follow-up queries per [`Context`].
//!
//! A command that changes an Objective is followed by its `objective.changed` and
//! `objective.detail` events, so a reply never re-fetches the Objective for that. Only Agent
//! Session replies do, because navigating, compacting or reloading a session sends no such event.

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

/// The effects of a successful reply. Every reply kind has its own arm, so a new one must be
/// classified here; a reply whose [`Context`] does not match its kind is logged and dropped.
pub fn reply_effects(context: &Context, data: &ReplyData) -> Effects {
    let mut effects = command_settled(context);
    let updates = &mut effects.updates;
    match data {
        ReplyData::Snapshot(s) => {
            updates.push(StoreUpdate::Snapshot {
                workspaces: s.workspaces.iter().map(convert::workspace).collect(),
                objectives: s.objectives.iter().map(convert::summary).collect(),
                settings: convert::settings(&s.settings),
            });
            effects
                .follow_ups
                .extend([Intent::LoadModels { provider: None }, Intent::LoadResources]);
        }
        ReplyData::WorkspaceOpen(w) => {
            updates.push(StoreUpdate::WorkspaceOpened(convert::workspace(w)));
        }
        ReplyData::ObjectiveCreate(d) => objective_created(d, &mut effects),
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
        | ReplyData::ObjectiveDeleteHistory(d) => {
            updates.push(StoreUpdate::ObjectiveDetail(Box::new(convert::detail(d))));
        }
        ReplyData::ActivityPage(p) => match context {
            Context::Activity(o) => updates.push(StoreUpdate::ActivityPage {
                objective_id: o.clone(),
                items: p.items.iter().map(convert::activity_item).collect(),
                has_more: p.has_more,
            }),
            _ => mismatched(context, data),
        },
        ReplyData::ChangesList(l) => match context {
            Context::Changes(o, basis) => {
                updates.push(StoreUpdate::Changes(convert::change_list(o, basis, l)));
            }
            _ => mismatched(context, data),
        },
        ReplyData::DiffGet(d) => match context {
            Context::Diff(o, basis, path) => {
                updates.push(StoreUpdate::Diff(convert::file_diff(o, basis, path, d)));
            }
            _ => mismatched(context, data),
        },
        ReplyData::FileRead(c) => match context {
            Context::File(o, basis, path, side) => updates.push(StoreUpdate::FilePage(
                convert::file_page(o, basis, path, *side, c),
            )),
            _ => mismatched(context, data),
        },
        ReplyData::Search(r) => match context {
            Context::Search {
                scope,
                query,
                objective_id,
                basis,
            } => updates.push(StoreUpdate::Search(view::SearchResults {
                scope: *scope,
                query: query.clone(),
                scope_label: r.scope_label.clone(),
                basis: basis.clone(),
                objective_id: objective_id.clone(),
                results: r.results.iter().map(convert::search_result).collect(),
                truncated: r.truncated,
            })),
            _ => mismatched(context, data),
        },
        ReplyData::DraftsList(l) => match context {
            Context::Drafts(o) => updates.push(StoreUpdate::Drafts {
                objective_id: o.clone(),
                drafts: l.drafts.iter().map(convert::draft).collect(),
            }),
            _ => mismatched(context, data),
        },
        ReplyData::InputsList(l) => match context {
            Context::Inputs(o) => updates.push(StoreUpdate::Inputs {
                objective_id: o.clone(),
                inputs: l.inputs.iter().map(convert::input).collect(),
            }),
            _ => mismatched(context, data),
        },
        ReplyData::MessageSend(i) | ReplyData::FeedbackSubmit(i) => {
            let input = convert::input(i);
            consumed_drafts(context, &input.objective_id, &mut effects);
            effects.updates.push(StoreUpdate::InputChanged {
                objective_id: input.objective_id.clone(),
                input,
            });
        }
        ReplyData::DraftSave(d) => updates.push(StoreUpdate::DraftChanged(convert::draft(d))),
        ReplyData::DraftDelete(_) => match context {
            Context::DraftDelete { draft_id, .. } => updates.push(StoreUpdate::DraftDeleted {
                draft_id: draft_id.clone(),
            }),
            _ => mismatched(context, data),
        },
        // The candidate or approval reaches the store with the `objective.detail` that follows.
        ReplyData::ReviewFreeze(_) | ReplyData::ReviewRefresh(_) | ReplyData::ReviewApprove(_) => {}
        ReplyData::ReviewVerify(v) => {
            updates.push(StoreUpdate::VerificationChanged(convert::verification(v)));
        }
        ReplyData::SettingsUpdate(s) => updates.push(StoreUpdate::Settings(convert::settings(s))),
        ReplyData::TerminalOpen(info) => match context {
            Context::Terminal { objective_id, .. } => {
                updates.push(terminal_opened(objective_id, info));
            }
            _ => mismatched(context, data),
        },
        ReplyData::ServiceStop(_) => updates.push(StoreUpdate::ServiceStopped),
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
        ReplyData::SessionTree(tree) => match context {
            Context::Tree(objective_id) => {
                updates.push(StoreUpdate::AgentSession(AgentSessionUpdate::Tree {
                    objective_id: objective_id.clone(),
                    tree: agent_session::tree(tree),
                }));
            }
            _ => mismatched(context, data),
        },
        ReplyData::SessionNavigate(state)
        | ReplyData::SessionFork(state)
        | ReplyData::SessionImport(state)
        | ReplyData::SessionCompact(state)
        | ReplyData::SessionReload(state)
        | ReplyData::SessionConfigure(state) => session_changed(state, &mut effects),
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
        ReplyData::ProfileImport(result) => profile_imported(result, &mut effects),
        // An operation status is read by recovery itself; terminal input, resizes and closes
        // report through terminal events, and a dialog answer through the next extension event.
        ReplyData::OperationGet(_)
        | ReplyData::TerminalInput(_)
        | ReplyData::TerminalResize(_)
        | ReplyData::TerminalClose(_)
        | ReplyData::UiRespond(_) => {}
    }
    effects
}

/// The effects of a command outcome recovered after the connection dropped, when they differ
/// from [`reply_effects`]; this is the one place that decides which recovered replies are stale.
///
/// A draft save or an accepted input carries its record as it was when the command completed,
/// while pages and events loaded since may already hold a later state of it, even at the same
/// draft revision. That record is left out and reloaded from current service state instead.
/// Every other outcome (`None`) is applied like a current reply: a deletion is final, and the
/// store drops an Objective older than the one it already holds.
pub fn recovered_effects(context: &Context, data: &ReplyData) -> Option<Effects> {
    let mut effects = command_settled(context);
    match data {
        ReplyData::DraftSave(draft) => effects.follow_ups.push(Intent::LoadDrafts {
            objective_id: ObjectiveId::new(draft.objective_id.as_str()),
        }),
        ReplyData::MessageSend(input) | ReplyData::FeedbackSubmit(input) => {
            let objective_id = ObjectiveId::new(input.objective_id.as_str());
            consumed_drafts(context, &objective_id, &mut effects);
            effects.follow_ups.push(Intent::LoadInputs { objective_id });
        }
        _ => return None,
    }
    Some(effects)
}

/// A finished command clears its pending status and the refusal shown on its surface.
fn command_settled(context: &Context) -> Effects {
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
    effects
}

fn mismatched(context: &Context, data: &ReplyData) {
    eprintln!(
        "lade-desktop: ignored a {} reply to “{}”",
        data.name().as_str(),
        context.label()
    );
}

fn objective_created(detail: &wire::ObjectiveDetail, effects: &mut Effects) {
    let detail = convert::detail(detail);
    let objective_id = detail.summary.objective_id.clone();
    effects
        .updates
        .push(StoreUpdate::ObjectiveDetail(Box::new(detail)));
    effects
        .updates
        .push(StoreUpdate::Select(objective_id.clone()));
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

/// An accepted feedback submission reloads the drafts it consumed.
fn consumed_drafts(context: &Context, objective_id: &ObjectiveId, effects: &mut Effects) {
    if matches!(context, Context::Command { batch, .. } if !batch.is_empty()) {
        effects.follow_ups.push(Intent::LoadDrafts {
            objective_id: objective_id.clone(),
        });
    }
}

/// A session command's new state, and a reload of the tree and Objective it changed.
fn session_changed(state: &wire::SessionState, effects: &mut Effects) {
    let (objective_id, session) = agent_session::session(state);
    effects
        .updates
        .push(StoreUpdate::AgentSession(AgentSessionUpdate::Session {
            objective_id: objective_id.clone(),
            session,
        }));
    effects.follow_ups.push(Intent::LoadSessionTree {
        objective_id: objective_id.clone(),
    });
    effects.follow_ups.push(refresh_objective(&objective_id));
}

fn profile_imported(result: &wire::ProfileImportResult, effects: &mut Effects) {
    effects
        .updates
        .push(StoreUpdate::AgentSession(AgentSessionUpdate::Inventory(
            agent_session::inventory(&result.inventory),
        )));
    effects.updates.push(StoreUpdate::Notice(ServiceNotice {
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

#[cfg(test)]
mod tests;
