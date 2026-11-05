//! Maps each [`Intent`] to its wire [`Request`] and the [`Context`] that interprets the reply,
//! including the surface a command was issued from. The match is exhaustive, so a new intent
//! does not compile until it is mapped here.

use std::path::Path;

use lade_protocol::client::{self as wire, Request};
use lade_protocol::types::{Counter, Digest, WirePath};

use super::{
    ACTIVITY_PAGE, Context, SEARCH_LIMIT, candidate, drafts_page, inputs_page, limit, read_limit,
    selector, start_point,
};
use crate::presentation::controller::intents::{Intent, StopReason};
use crate::presentation::state::model::{ActionSurface, ObjectiveId};
use crate::transport::agent_session;
use crate::transport::convert::{self, Invalid, id, to_basis, to_path};

/// Text fields the Agent Session protocol bounds to 1–256 characters.
fn bounded_text(value: &str, what: &str) -> Result<String, Invalid> {
    if value.is_empty() || value.chars().count() > 256 {
        Err(Invalid(format!("{what} must be 1 to 256 characters")))
    } else {
        Ok(value.to_owned())
    }
}

fn wire_path(path: &Path) -> Result<WirePath, Invalid> {
    WirePath::from_os_str(path.as_os_str())
        .map_err(|_| Invalid("the path cannot be sent (empty or over 4096 bytes)".into()))
}

/// The request for `intent`, or `Ok(None)` when the intent is handled without one.
pub fn request(intent: &Intent) -> Result<Option<(Request, Context)>, Invalid> {
    use Intent as I;
    let mapped = match intent {
        I::RetryConnection => return Ok(None),
        I::OpenWorkspace { .. }
        | I::Search { .. }
        | I::CreateObjective { .. }
        | I::UpdateSettings { .. }
        | I::StopService { .. }
        | I::LoadModels { .. }
        | I::LoadResources
        | I::DecideResource { .. }
        | I::ImportProfile { .. } => workspace_request(intent)?,
        I::LoadObjective { .. }
        | I::LoadActivity { .. }
        | I::LoadChanges { .. }
        | I::LoadDiff { .. }
        | I::OpenFile { .. }
        | I::LoadDrafts { .. }
        | I::LoadInputs { .. } => load_request(intent)?,
        I::UpdateObjective { .. }
        | I::StartExecution { .. }
        | I::Cancel { .. }
        | I::Resume { .. }
        | I::SendMessage { .. }
        | I::Complete { .. }
        | I::Discard { .. }
        | I::Cleanup { .. }
        | I::DeleteHistory { .. } => objective_request(intent)?,
        I::SaveDraft { .. } | I::DeleteDraft { .. } | I::SubmitFeedback { .. } => {
            feedback_request(intent)?
        }
        I::Freeze { .. }
        | I::ReturnForRevision { .. }
        | I::Verify { .. }
        | I::Approve { .. }
        | I::Refresh { .. }
        | I::Integrate { .. } => review_request(intent)?,
        I::OpenTerminal { .. }
        | I::TerminalInput { .. }
        | I::ResizeTerminal { .. }
        | I::CloseTerminal { .. } => terminal_request(intent)?,
        I::LoadSessionTree { .. }
        | I::NavigateSession { .. }
        | I::ForkSession { .. }
        | I::ImportSession { .. }
        | I::ExportSession { .. }
        | I::CompactSession { .. }
        | I::ReloadSession { .. }
        | I::ConfigureSession { .. }
        | I::RespondDialog { .. } => session_request(intent)?,
    };
    Ok(Some(mapped))
}

/// A request about the workspace, the service and its settings, models or profile resources.
fn workspace_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let label = intent.label();
    Ok(match intent {
        I::OpenWorkspace { path } => (
            Request::WorkspaceOpen(wire::WorkspaceOpenCommand {
                path: WirePath::from_os_str(path.as_os_str()).map_err(|_| {
                    Invalid("the folder path cannot be sent (empty or over 4096 bytes)".into())
                })?,
            }),
            Context::OpenWorkspace,
        ),
        I::Search {
            workspace_id,
            scope,
            objective_id,
            basis,
            query,
        } => (
            Request::Search(wire::SearchQuery {
                workspace_id: id(workspace_id.as_str(), "workspace id")?,
                scope: convert::to_scope(*scope),
                query: query.clone(),
                limit: limit(SEARCH_LIMIT)?,
                objective_id: objective_id
                    .as_ref()
                    .map(|o| id(o.as_str(), "objective id"))
                    .transpose()?,
                basis: basis.as_ref().map(to_basis).transpose()?,
            }),
            Context::Search {
                scope: *scope,
                query: query.clone(),
                objective_id: objective_id.clone(),
                basis: basis.clone(),
            },
        ),
        I::CreateObjective {
            workspace_id,
            title,
            intent: objective_intent,
            integration_target,
            start,
        } => (
            Request::ObjectiveCreate(wire::ObjectiveCreateCommand {
                workspace_id: id(workspace_id.as_str(), "workspace id")?,
                title: title.clone(),
                intent: wire::Intent {
                    outcome: objective_intent.outcome.clone(),
                    constraints: objective_intent.constraints.clone(),
                    acceptance: objective_intent.acceptance.clone(),
                },
                integration_target: integration_target.clone(),
                start: start_point(start)?,
            }),
            Context::command(label, None, None),
        ),
        I::UpdateSettings { default_model } => (
            Request::SettingsUpdate(wire::SettingsUpdateCommand {
                default_model: Some(convert::to_model(default_model)),
                verification_commands: None,
            }),
            Context::command(label, None, None),
        ),
        I::StopService { reason } => (
            Request::ServiceStop(wire::ServiceStopCommand {
                reason: match reason {
                    StopReason::Exit => wire::StopReason::Exit,
                    StopReason::Logout => wire::StopReason::Logout,
                },
            }),
            Context::Stop,
        ),
        I::LoadModels { provider } => (
            Request::ModelsList(wire::ModelsListQuery {
                provider: provider.clone(),
            }),
            Context::Models(provider.clone()),
        ),
        I::LoadResources => (Request::ResourcesList(wire::Empty {}), Context::Resources),
        I::DecideResource {
            name,
            sha256,
            approve,
        } => (
            Request::ResourceDecide(wire::ResourceDecideCommand {
                name: bounded_text(name, "the extension name")?,
                sha256: Digest::parse(sha256.clone())
                    .map_err(|_| Invalid("the extension hash is not a sha256 digest".into()))?,
                approve: *approve,
            }),
            Context::command(label, None, None),
        ),
        I::ImportProfile { source } => (
            Request::ProfileImport(wire::ProfileImportCommand {
                source: wire_path(source)?,
            }),
            Context::command(label, None, None),
        ),
        _ => return Err(misrouted(label)),
    })
}

/// A query that loads one view of an Objective.
fn load_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let label = intent.label();
    Ok(match intent {
        I::LoadObjective { objective_id } => (
            Request::ObjectiveGet(selector(objective_id)?),
            Context::Objective(objective_id.clone()),
        ),
        I::LoadActivity {
            objective_id,
            before,
        } => (
            Request::ActivityPage(wire::ActivityPageQuery {
                objective_id: id(objective_id.as_str(), "objective id")?,
                limit: limit(ACTIVITY_PAGE)?,
                before: before.map(Counter::new),
            }),
            Context::Activity(objective_id.clone()),
        ),
        I::LoadChanges {
            objective_id,
            basis,
        } => (
            Request::ChangesList(wire::ChangesListQuery {
                objective_id: id(objective_id.as_str(), "objective id")?,
                basis: to_basis(basis)?,
            }),
            Context::Changes(objective_id.clone(), basis.clone()),
        ),
        I::LoadDiff {
            objective_id,
            basis,
            path,
        } => (
            Request::DiffGet(wire::DiffGetQuery {
                objective_id: id(objective_id.as_str(), "objective id")?,
                basis: to_basis(basis)?,
                path: to_path(path)?,
            }),
            Context::Diff(objective_id.clone(), basis.clone(), path.clone()),
        ),
        I::OpenFile {
            objective_id,
            basis,
            path,
            side,
            offset,
        } => (
            Request::FileRead(wire::FileReadQuery {
                objective_id: id(objective_id.as_str(), "objective id")?,
                basis: to_basis(basis)?,
                path: to_path(path)?,
                side: convert::to_side(*side),
                offset: Counter::new(*offset),
                limit: read_limit()?,
            }),
            Context::File(objective_id.clone(), basis.clone(), path.clone(), *side),
        ),
        I::LoadDrafts { objective_id } => (
            drafts_page(objective_id, None)?,
            Context::Drafts(objective_id.clone()),
        ),
        I::LoadInputs { objective_id } => (
            inputs_page(objective_id, None)?,
            Context::Inputs(objective_id.clone()),
        ),
        _ => return Err(misrouted(label)),
    })
}

/// A command on an Objective's intent, its agent's execution or its closing.
fn objective_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use ActionSurface as S;
    use Intent as I;
    let label = intent.label();
    let command = |objective_id: &ObjectiveId, surface| {
        Context::command(label, Some(objective_id), Some(surface))
    };
    Ok(match intent {
        I::UpdateObjective {
            objective_id,
            expected_intent_revision,
            intent: objective_intent,
        } => (
            Request::ObjectiveUpdateIntent(wire::ObjectiveUpdateIntentCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                expected_intent_revision: Counter::new(*expected_intent_revision),
                intent: wire::Intent {
                    outcome: objective_intent.outcome.clone(),
                    constraints: objective_intent.constraints.clone(),
                    acceptance: objective_intent.acceptance.clone(),
                },
            }),
            command(objective_id, S::Intent),
        ),
        I::StartExecution {
            objective_id,
            model,
            thinking,
        } => (
            Request::ExecutionStart(wire::ExecutionStartCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                model: convert::to_model(model),
                thinking: thinking.map(agent_session::to_thinking),
            }),
            command(objective_id, S::Agent),
        ),
        I::Cancel { objective_id } => (
            Request::ExecutionCancel(selector(objective_id)?),
            command(objective_id, S::Agent),
        ),
        I::Resume { objective_id } => (
            Request::ExecutionResume(selector(objective_id)?),
            command(objective_id, S::Agent),
        ),
        I::SendMessage {
            objective_id,
            text,
            mode,
        } => (
            Request::MessageSend(wire::MessageSendCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                text: text.clone(),
                mode: convert::to_delivery_mode(*mode),
            }),
            command(objective_id, S::Send),
        ),
        I::Complete { objective_id } => (
            Request::ObjectiveComplete(selector(objective_id)?),
            command(objective_id, S::Closing),
        ),
        I::Discard { objective_id } => (
            Request::ObjectiveDiscard(selector(objective_id)?),
            command(objective_id, S::Closing),
        ),
        I::Cleanup {
            objective_id,
            confirm_data_loss,
        } => (
            Request::ObjectiveCleanup(wire::ObjectiveCleanupCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                confirm_data_loss: *confirm_data_loss,
            }),
            command(objective_id, S::Closing),
        ),
        I::DeleteHistory { objective_id } => (
            Request::ObjectiveDeleteHistory(wire::ObjectiveDeleteHistoryCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                confirm_history_loss: true,
            }),
            command(objective_id, S::Closing),
        ),
        _ => return Err(misrouted(label)),
    })
}

/// A command on review drafts and the feedback they make up.
fn feedback_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use ActionSurface as S;
    use Intent as I;
    let label = intent.label();
    Ok(match intent {
        I::SaveDraft {
            objective_id,
            draft_id,
            expected_revision,
            reference,
            body,
        } => (
            Request::DraftSave(wire::DraftSaveCommand {
                draft_id: id(draft_id.as_str(), "draft id")?,
                objective_id: id(objective_id.as_str(), "objective id")?,
                expected_revision: Counter::new(*expected_revision),
                reference: convert::to_reference(reference)?,
                body: body.clone(),
            }),
            Context::Command {
                label,
                objective_id: Some(objective_id.clone()),
                saved_draft: Some(draft_id.clone()),
                batch: Vec::new(),
                surface: Some(S::Drafts),
            },
        ),
        I::DeleteDraft {
            draft_id,
            expected_revision,
        } => (
            Request::DraftDelete(wire::DraftDeleteCommand {
                draft_id: id(draft_id.as_str(), "draft id")?,
                expected_revision: Counter::new(*expected_revision),
            }),
            Context::DraftDelete {
                label,
                draft_id: draft_id.clone(),
            },
        ),
        I::SubmitFeedback {
            objective_id,
            destination_session_id,
            association_revision,
            drafts,
            mode,
            note,
        } => (
            Request::FeedbackSubmit(wire::FeedbackSubmitCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                destination_session_id: id(destination_session_id.as_str(), "session id")?,
                association_revision: Counter::new(*association_revision),
                drafts: drafts
                    .iter()
                    .map(|d| {
                        Ok(wire::DraftRevision {
                            draft_id: id(d.draft_id.as_str(), "draft id")?,
                            revision: Counter::new(d.revision),
                        })
                    })
                    .collect::<Result<_, Invalid>>()?,
                mode: convert::to_delivery_mode(*mode),
                note: note.clone(),
            }),
            Context::Command {
                label,
                objective_id: Some(objective_id.clone()),
                saved_draft: None,
                batch: drafts.iter().map(|d| d.draft_id.clone()).collect(),
                surface: Some(S::Feedback),
            },
        ),
        _ => return Err(misrouted(label)),
    })
}

/// A command on an Objective's candidate: freezing, returning, checking, approving and integrating it.
fn review_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use ActionSurface as S;
    use Intent as I;
    let label = intent.label();
    let command = |objective_id: &ObjectiveId, surface| {
        Context::command(label, Some(objective_id), Some(surface))
    };
    Ok(match intent {
        I::Freeze { objective_id } => (
            Request::ReviewFreeze(selector(objective_id)?),
            command(objective_id, S::Review),
        ),
        I::ReturnForRevision {
            objective_id,
            candidate_id,
        } => (
            Request::ReviewReturn(candidate(objective_id, candidate_id)?),
            command(objective_id, S::Review),
        ),
        I::Verify {
            objective_id,
            candidate_id,
            argv,
        } => (
            Request::ReviewVerify(wire::ReviewVerifyCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                candidate_id: id(candidate_id.as_str(), "candidate id")?,
                command: argv.clone(),
            }),
            command(objective_id, S::Checks),
        ),
        I::Approve {
            objective_id,
            candidate_id,
            target_commit,
            override_reason,
        } => (
            Request::ReviewApprove(wire::ReviewApproveCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                candidate_id: id(candidate_id.as_str(), "candidate id")?,
                target_commit: convert::to_git_id(target_commit)?,
                override_reason: override_reason.clone(),
            }),
            command(objective_id, S::Review),
        ),
        I::Refresh {
            objective_id,
            candidate_id,
        } => (
            Request::ReviewRefresh(candidate(objective_id, candidate_id)?),
            command(objective_id, S::Review),
        ),
        I::Integrate {
            objective_id,
            approval_id,
        } => (
            Request::ReviewIntegrate(wire::ReviewIntegrateCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                approval_id: id(approval_id.as_str(), "approval id")?,
            }),
            command(objective_id, S::Review),
        ),
        _ => return Err(misrouted(label)),
    })
}

/// A command on an Objective's terminal.
fn terminal_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let label = intent.label();
    Ok(match intent {
        I::OpenTerminal {
            objective_id,
            cols,
            rows,
        } => (
            Request::TerminalOpen(wire::TerminalOpenCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                cols: Counter::new(u64::from(*cols)),
                rows: Counter::new(u64::from(*rows)),
            }),
            Context::Terminal {
                label,
                objective_id: objective_id.clone(),
            },
        ),
        I::TerminalInput {
            objective_id,
            terminal_id,
            data,
        } => (
            Request::TerminalInput(wire::TerminalInputCommand {
                terminal_id: id(terminal_id, "terminal id")?,
                data: data.clone(),
            }),
            Context::Terminal {
                label,
                objective_id: objective_id.clone(),
            },
        ),
        I::ResizeTerminal {
            objective_id,
            terminal_id,
            cols,
            rows,
        } => (
            Request::TerminalResize(wire::TerminalResizeCommand {
                terminal_id: id(terminal_id, "terminal id")?,
                cols: Counter::new(u64::from(*cols)),
                rows: Counter::new(u64::from(*rows)),
            }),
            Context::Terminal {
                label,
                objective_id: objective_id.clone(),
            },
        ),
        I::CloseTerminal {
            objective_id,
            terminal_id,
        } => (
            Request::TerminalClose(wire::TerminalSelector {
                terminal_id: id(terminal_id, "terminal id")?,
            }),
            Context::Terminal {
                label,
                objective_id: objective_id.clone(),
            },
        ),
        _ => return Err(misrouted(label)),
    })
}

/// A command on an Objective's pi session or one of its dialogs.
fn session_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use ActionSurface as S;
    use Intent as I;
    let label = intent.label();
    let command = |objective_id: &ObjectiveId, surface| {
        Context::command(label, Some(objective_id), Some(surface))
    };
    Ok(match intent {
        I::LoadSessionTree { objective_id } => (
            Request::SessionTree(selector(objective_id)?),
            Context::Tree(objective_id.clone()),
        ),
        I::NavigateSession {
            objective_id,
            entry_id,
        } => (
            Request::SessionNavigate(wire::SessionNavigateCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                entry_id: bounded_text(entry_id, "the entry id")?,
            }),
            command(objective_id, S::Session),
        ),
        I::ForkSession {
            objective_id,
            entry_id,
            position,
        } => (
            Request::SessionFork(wire::SessionForkCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                entry_id: bounded_text(entry_id, "the entry id")?,
                position: agent_session::to_fork(*position),
            }),
            command(objective_id, S::Session),
        ),
        I::ImportSession {
            objective_id,
            path: file,
        } => (
            Request::SessionImport(wire::SessionImportCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                path: wire_path(file)?,
            }),
            command(objective_id, S::Session),
        ),
        I::ExportSession { objective_id } => (
            Request::SessionExport(selector(objective_id)?),
            command(objective_id, S::Session),
        ),
        I::CompactSession { objective_id } => (
            Request::SessionCompact(wire::SessionCompactCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                instructions: None,
            }),
            command(objective_id, S::Session),
        ),
        I::ReloadSession { objective_id } => (
            Request::SessionReload(selector(objective_id)?),
            command(objective_id, S::Session),
        ),
        I::ConfigureSession {
            objective_id,
            model,
            thinking,
        } => (
            Request::SessionConfigure(wire::SessionConfigureCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                model: convert::to_model(model),
                thinking: agent_session::to_thinking(*thinking),
            }),
            command(objective_id, S::Session),
        ),
        I::RespondDialog {
            objective_id,
            ui_id,
            generation,
            answer,
        } => (
            Request::UiRespond(wire::UiRespondCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                ui_id: id(ui_id, "dialog id")?,
                generation: Counter::new(*generation),
                answer: agent_session::to_answer(answer),
            }),
            command(objective_id, S::Dialog),
        ),

        _ => return Err(misrouted(label)),
    })
}

/// The error for an intent routed to the wrong family above, which `request`'s match rules out.
fn misrouted(label: &str) -> Invalid {
    Invalid(format!("{label} has no request in this family"))
}

#[cfg(test)]
mod tests;
