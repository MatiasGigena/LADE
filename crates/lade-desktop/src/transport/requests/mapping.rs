//! Maps each request-backed [`Intent`] to its wire [`Request`] and the [`Context`] that
//! interprets the reply. Each mapper handles one family of intents and hands any other intent
//! to the next family; the last one rejects intents that have no request.

use std::path::Path;

use lade_protocol::client::{self as wire, Request};
use lade_protocol::types::{Counter, Digest, WirePath};

use super::{
    ACTIVITY_PAGE, Context, SEARCH_LIMIT, candidate, drafts_page, inputs_page, limit, read_limit,
    selector, start_point,
};
use crate::presentation::controller::intents::{Intent, StopReason};
use crate::presentation::state::model::ObjectiveId;
use crate::transport::agent_session;
use crate::transport::convert::{self, Invalid, id, to_basis, to_path};

/// The first mapper: workspace, search, objective creation, settings and service lifetime.
/// `Ok(None)` means the intent is handled without a request.
pub(super) fn request_for(intent: &Intent) -> Result<Option<(Request, Context)>, Invalid> {
    use Intent as I;
    let mapped = match intent {
        I::RetryConnection => return Ok(None),
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
            Context::command(intent.label(), None),
        ),
        I::UpdateSettings { default_model } => (
            Request::SettingsUpdate(wire::SettingsUpdateCommand {
                default_model: Some(convert::to_model(default_model)),
                verification_commands: None,
            }),
            Context::command(intent.label(), None),
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
        _ => return objective_query(intent).map(Some),
    };
    Ok(Some(mapped))
}

/// Reads scoped to one objective.
fn objective_query(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let mapped = match intent {
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
        _ => return objective_command(intent),
    };
    Ok(mapped)
}

/// Intent, execution, messaging and closing commands on one objective.
fn objective_command(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let mapped = match intent {
        I::UpdateObjective {
            objective_id,
            expected_intent_revision,
            intent: objective_intent,
        } => (
            Request::ObjectiveUpdateIntent(wire::UpdateIntentCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                expected_intent_revision: Counter::new(*expected_intent_revision),
                intent: wire::Intent {
                    outcome: objective_intent.outcome.clone(),
                    constraints: objective_intent.constraints.clone(),
                    acceptance: objective_intent.acceptance.clone(),
                },
            }),
            Context::command(intent.label(), Some(objective_id)),
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
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Cancel { objective_id } => (
            Request::ExecutionCancel(selector(objective_id)?),
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Resume { objective_id } => (
            Request::ExecutionResume(selector(objective_id)?),
            Context::command(intent.label(), Some(objective_id)),
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
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Complete { objective_id } => (
            Request::ObjectiveComplete(selector(objective_id)?),
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Discard { objective_id } => (
            Request::ObjectiveDiscard(selector(objective_id)?),
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Cleanup {
            objective_id,
            confirm_data_loss,
        } => (
            Request::ObjectiveCleanup(wire::ObjectiveCleanupCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                confirm_data_loss: *confirm_data_loss,
            }),
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::DeleteHistory { objective_id } => (
            Request::ObjectiveDeleteHistory(wire::ObjectiveHistoryDeleteCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                confirm_history_loss: true,
            }),
            Context::command(intent.label(), Some(objective_id)),
        ),
        _ => return draft_command(intent),
    };
    Ok(mapped)
}

/// Review drafts and their submission as feedback.
fn draft_command(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let mapped = match intent {
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
                label: intent.label(),
                objective_id: Some(objective_id.clone()),
                draft_id: Some(draft_id.clone()),
                batch: Vec::new(),
                surface: None,
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
            Context::Command {
                label: intent.label(),
                objective_id: None,
                draft_id: Some(draft_id.clone()),
                batch: Vec::new(),
                surface: None,
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
                label: intent.label(),
                objective_id: Some(objective_id.clone()),
                draft_id: None,
                batch: drafts.iter().map(|d| d.draft_id.clone()).collect(),
                surface: None,
            },
        ),
        _ => return review_command(intent),
    };
    Ok(mapped)
}

/// Candidate review: freeze, return, verify, approve, refresh and integrate.
fn review_command(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let mapped = match intent {
        I::Freeze { objective_id } => (
            Request::ReviewFreeze(selector(objective_id)?),
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::ReturnForRevision {
            objective_id,
            candidate_id,
        } => (
            Request::ReviewReturn(candidate(objective_id, candidate_id)?),
            Context::command(intent.label(), Some(objective_id)),
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
            Context::command(intent.label(), Some(objective_id)),
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
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Refresh {
            objective_id,
            candidate_id,
        } => (
            Request::ReviewRefresh(candidate(objective_id, candidate_id)?),
            Context::command(intent.label(), Some(objective_id)),
        ),
        I::Integrate {
            objective_id,
            approval_id,
        } => (
            Request::ReviewIntegrate(wire::ReviewIntegrateCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                approval_id: id(approval_id.as_str(), "approval id")?,
            }),
            Context::command(intent.label(), Some(objective_id)),
        ),
        _ => return terminal_request(intent),
    };
    Ok(mapped)
}

/// The objective terminal.
fn terminal_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let mapped = match intent {
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
            Context::Terminal(objective_id.clone(), intent.label()),
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
            Context::Terminal(objective_id.clone(), intent.label()),
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
            Context::Terminal(objective_id.clone(), intent.label()),
        ),
        I::CloseTerminal {
            objective_id,
            terminal_id,
        } => (
            Request::TerminalClose(wire::TerminalSelector {
                terminal_id: id(terminal_id, "terminal id")?,
            }),
            Context::Terminal(objective_id.clone(), intent.label()),
        ),
        _ => return agent_session_request(intent),
    };
    Ok(mapped)
}

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

/// Agent Session (pi) models, tree and session commands.
fn agent_session_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let command = |objective_id: &ObjectiveId| Context::command(intent.label(), Some(objective_id));
    Ok(match intent {
        I::LoadModels { provider } => (
            Request::ModelsList(wire::ModelsListQuery {
                provider: provider.clone(),
            }),
            Context::Models(provider.clone()),
        ),
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
            command(objective_id),
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
            command(objective_id),
        ),
        I::ImportSession {
            objective_id,
            path: file,
        } => (
            Request::SessionImport(wire::SessionImportCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                path: wire_path(file)?,
            }),
            command(objective_id),
        ),
        I::ExportSession { objective_id } => (
            Request::SessionExport(selector(objective_id)?),
            command(objective_id),
        ),
        I::CompactSession { objective_id } => (
            Request::SessionCompact(wire::SessionCompactCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                instructions: None,
            }),
            command(objective_id),
        ),
        I::ReloadSession { objective_id } => (
            Request::SessionReload(selector(objective_id)?),
            command(objective_id),
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
            command(objective_id),
        ),
        _ => return profile_request(intent),
    })
}

/// The LADE profile's resources and extension approvals, and Agent Session dialogs; any other
/// intent has no request.
fn profile_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    Ok(match intent {
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
            Context::command(intent.label(), None),
        ),
        I::ImportProfile { source } => (
            Request::ProfileImport(wire::ProfileImportCommand {
                source: wire_path(source)?,
            }),
            Context::command(intent.label(), None),
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
            Context::command(intent.label(), Some(objective_id)),
        ),
        _ => return Err(Invalid("not an Agent Session intent".into())),
    })
}

#[cfg(test)]
mod tests;
