//! Intent → protocol request mapping and reply → store-update mapping. Pure and GTK-free.
//!
//! Every intent maps to exactly one request name; the [`Context`] keeps what the reply needs but
//! does not carry (the requested basis, path and side, the draft being deleted, ...).

use lade_protocol::client::{self as wire, Rejection, ReplyData, Request};
use lade_protocol::types::{Counter, GitId, PageLimit, ReadLimit};

use super::compat as compat_wire;
use super::convert::{self, Invalid, id, to_basis, to_path};
use crate::presentation::compat::CompatUpdate;
use crate::presentation::intents::{Intent, StartPoint, StopReason};
use crate::presentation::model::{
    self as view, Basis, DraftId, NoticeLevel, ObjectiveId, RepoPath, SearchScope, ServiceNotice,
    Side,
};
use crate::presentation::store::StoreUpdate;
use crate::presentation::terminal::TerminalMode;

/// Newest activity page size.
pub const ACTIVITY_PAGE: u32 = 100;
/// Search result page size.
pub const SEARCH_LIMIT: u32 = 100;
/// `file.read` window (the schema maximum).
pub const READ_WINDOW: u32 = 262_144;

/// What a reply needs to be mapped back onto the presentation store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    Snapshot,
    Objective(ObjectiveId),
    Activity(ObjectiveId),
    Changes(ObjectiveId, Basis),
    Diff(ObjectiveId, Basis, RepoPath),
    File(ObjectiveId, Basis, RepoPath, Side),
    Drafts(ObjectiveId),
    Inputs(ObjectiveId),
    Search {
        scope: SearchScope,
        query: String,
        objective_id: Option<ObjectiveId>,
        basis: Option<Basis>,
    },
    /// `models.list` for all providers (`None`) or one provider's models.
    Models(Option<String>),
    Tree(ObjectiveId),
    Resources,
    /// Terminal interaction for an Objective; outcomes are shown in its terminal pane.
    Terminal(ObjectiveId, &'static str),
    /// `service.stop` (Exit / logout).
    Stop,
    /// A mutating request. `objective_id` scopes where its outcome is shown.
    Command {
        label: &'static str,
        objective_id: Option<ObjectiveId>,
        draft_id: Option<DraftId>,
        /// Draft ids of a feedback batch, to name stale items in a `stale_reference` rejection.
        batch: Vec<DraftId>,
    },
}

impl Context {
    fn command(label: &'static str, objective_id: Option<&ObjectiveId>) -> Self {
        Self::Command {
            label,
            objective_id: objective_id.cloned(),
            draft_id: None,
            batch: Vec::new(),
        }
    }

    pub fn objective_id(&self) -> Option<&ObjectiveId> {
        match self {
            Self::Snapshot
            | Self::Search { .. }
            | Self::Models(_)
            | Self::Resources
            | Self::Stop => None,
            Self::Tree(o) | Self::Terminal(o, _) => Some(o),
            Self::Objective(o)
            | Self::Activity(o)
            | Self::Changes(o, _)
            | Self::Diff(o, ..)
            | Self::File(o, ..)
            | Self::Drafts(o)
            | Self::Inputs(o) => Some(o),
            Self::Command { objective_id, .. } => objective_id.as_ref(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Snapshot => "Load service state",
            Self::Objective(_) => "Load Objective",
            Self::Activity(_) => "Load activity",
            Self::Changes(..) => "Load Change Set",
            Self::Diff(..) => "Load diff",
            Self::File(..) => "Open file",
            Self::Drafts(_) => "Load drafts",
            Self::Inputs(_) => "Load inputs",
            Self::Search { .. } => "Search",
            Self::Models(_) => "Load models",
            Self::Tree(_) => "Load session tree",
            Self::Resources => "Load profile resources",
            Self::Command { label, .. } | Self::Terminal(_, label) => label,
            Self::Stop => "Exit and stop the service",
        }
    }
}

fn selector(objective_id: &ObjectiveId) -> Result<wire::ObjectiveSelector, Invalid> {
    Ok(wire::ObjectiveSelector {
        objective_id: id(objective_id.as_str(), "objective id")?,
    })
}

fn candidate(
    objective_id: &ObjectiveId,
    candidate_id: &view::CandidateId,
) -> Result<wire::CandidateCommand, Invalid> {
    Ok(wire::CandidateCommand {
        objective_id: id(objective_id.as_str(), "objective id")?,
        candidate_id: id(candidate_id.as_str(), "candidate id")?,
    })
}

fn limit(value: u32) -> PageLimit {
    PageLimit::parse(value.to_string()).expect("constant page limit is valid")
}

/// Parses a typed start point: `target`, or a full 40/64-hex commit id.
pub fn start_point(start: &StartPoint) -> Result<wire::Start, Invalid> {
    match start {
        StartPoint::Target => Ok(wire::Start::Target),
        StartPoint::Commit(hex) => {
            let hex = hex.trim().to_ascii_lowercase();
            if hex == "target" {
                return Ok(wire::Start::Target);
            }
            if !matches!(hex.len(), 40 | 64) {
                return Err(Invalid(
                    "start commit must be 40 or 64 hex characters".into(),
                ));
            }
            GitId::from_hex(&hex)
                .map(|commit| wire::Start::Commit { commit })
                .map_err(|_| Invalid("start commit is not hexadecimal".into()))
        }
    }
}

/// Maps one intent to its request. `Ok(None)` for intents handled by the connection itself.
pub fn request(intent: &Intent) -> Result<Option<(Request, Context)>, Invalid> {
    use Intent as I;
    let mapped = match intent {
        I::RetryConnection => return Ok(None),
        I::OpenWorkspace { path } => (
            Request::WorkspaceOpen(wire::WorkspaceOpenCommand {
                path: lade_protocol::types::WirePath::from_os_str(path.as_os_str()).map_err(
                    |_| Invalid("the folder path cannot be sent (empty or over 4096 bytes)".into()),
                )?,
            }),
            Context::command(intent.label(), None),
        ),
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
                limit: limit(ACTIVITY_PAGE),
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
                limit: ReadLimit::parse(READ_WINDOW.to_string()).expect("constant read limit"),
            }),
            Context::File(objective_id.clone(), basis.clone(), path.clone(), *side),
        ),
        I::LoadDrafts { objective_id } => (
            Request::DraftsList(selector(objective_id)?),
            Context::Drafts(objective_id.clone()),
        ),
        I::LoadInputs { objective_id } => (
            Request::InputsList(selector(objective_id)?),
            Context::Inputs(objective_id.clone()),
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
                limit: limit(SEARCH_LIMIT),
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
                thinking: thinking.map(compat_wire::to_thinking),
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
            },
        ),
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
        I::UpdateSettings { default_model } => (
            Request::SettingsUpdate(wire::SettingsUpdateCommand {
                default_model: Some(convert::to_model(default_model)),
                verification_commands: None,
            }),
            Context::command(intent.label(), None),
        ),
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
        I::StopService { reason } => (
            Request::ServiceStop(wire::ServiceStopCommand {
                reason: match reason {
                    StopReason::Exit => wire::StopReason::Exit,
                    StopReason::Logout => wire::StopReason::Logout,
                },
            }),
            Context::Stop,
        ),
        _ => return compat_request(intent).map(Some),
    };
    Ok(Some(mapped))
}

/// IMP-011 intents (model catalogue, session operations, profile resources, dialogs).
fn compat_request(intent: &Intent) -> Result<(Request, Context), Invalid> {
    use Intent as I;
    let command = |objective_id: &ObjectiveId| Context::command(intent.label(), Some(objective_id));
    let text = |value: &str, what: &str| {
        if value.is_empty() || value.chars().count() > 256 {
            Err(Invalid(format!("{what} must be 1 to 256 characters")))
        } else {
            Ok(value.to_owned())
        }
    };
    let path = |path: &std::path::Path| {
        lade_protocol::types::WirePath::from_os_str(path.as_os_str())
            .map_err(|_| Invalid("the path cannot be sent (empty or over 4096 bytes)".into()))
    };
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
                entry_id: text(entry_id, "the entry id")?,
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
                entry_id: text(entry_id, "the entry id")?,
                position: compat_wire::to_fork(*position),
            }),
            command(objective_id),
        ),
        I::ImportSession {
            objective_id,
            path: file,
        } => (
            Request::SessionImport(wire::SessionImportCommand {
                objective_id: id(objective_id.as_str(), "objective id")?,
                path: path(file)?,
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
                thinking: compat_wire::to_thinking(*thinking),
            }),
            command(objective_id),
        ),
        I::LoadResources => (Request::ResourcesList(wire::Empty {}), Context::Resources),
        I::DecideResource {
            name,
            sha256,
            approve,
        } => (
            Request::ResourceDecide(wire::ResourceDecideCommand {
                name: text(name, "the extension name")?,
                sha256: lade_protocol::types::Digest::parse(sha256.clone())
                    .map_err(|_| Invalid("the extension hash is not a sha256 digest".into()))?,
                approve: *approve,
            }),
            Context::command(intent.label(), None),
        ),
        I::ImportProfile { source } => (
            Request::ProfileImport(wire::ProfileImportCommand {
                source: path(source)?,
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
                answer: compat_wire::to_answer(answer),
            }),
            command(objective_id),
        ),
        _ => return Err(Invalid("not an IMP-011 intent".into())),
    })
}

/// IMP-011 reply data; `true` when `data` was one of them.
fn compat_effects(context: &Context, data: &ReplyData, effects: &mut Effects) -> bool {
    let updates = &mut effects.updates;
    match data {
        ReplyData::ModelsList(catalog) => {
            updates.push(StoreUpdate::Compat(CompatUpdate::Catalog(
                compat_wire::catalog(catalog),
            )));
            if let Context::Models(Some(provider)) = context {
                updates.push(StoreUpdate::Compat(CompatUpdate::Models {
                    provider: provider.clone(),
                    models: compat_wire::models(catalog),
                }));
            }
        }
        ReplyData::SessionTree(tree) => {
            if let Context::Tree(objective_id) = context {
                updates.push(StoreUpdate::Compat(CompatUpdate::Tree {
                    objective_id: objective_id.clone(),
                    tree: compat_wire::tree(tree),
                }));
            }
        }
        ReplyData::SessionNavigate(state)
        | ReplyData::SessionFork(state)
        | ReplyData::SessionImport(state)
        | ReplyData::SessionCompact(state)
        | ReplyData::SessionReload(state)
        | ReplyData::SessionConfigure(state) => {
            let (objective_id, session) = compat_wire::session(state);
            updates.push(StoreUpdate::Compat(CompatUpdate::Session {
                objective_id: objective_id.clone(),
                session,
            }));
            // A navigation or fork changes the tree; the detail shows a rebound session.
            effects.follow_ups.push(Intent::LoadSessionTree {
                objective_id: objective_id.clone(),
            });
            effects.follow_ups.push(refresh_objective(&objective_id));
        }
        ReplyData::SessionExport(export) => {
            updates.push(StoreUpdate::Compat(CompatUpdate::Exported {
                objective_id: ObjectiveId::new(export.objective_id.as_str()),
                path: export.path.display().to_owned(),
            }));
        }
        ReplyData::ResourcesList(inventory) | ReplyData::ResourceDecide(inventory) => {
            updates.push(StoreUpdate::Compat(CompatUpdate::Inventory(
                compat_wire::inventory(inventory),
            )));
        }
        ReplyData::ProfileImport(result) => {
            updates.push(StoreUpdate::Compat(CompatUpdate::Inventory(
                compat_wire::inventory(&result.inventory),
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
            }));
            // Custom providers may have changed.
            effects
                .follow_ups
                .push(Intent::LoadModels { provider: None });
        }
        ReplyData::UiRespond(_) => {}
        _ => return false,
    }
    true
}

/// What a successful reply does: store updates plus follow-up queries to issue.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub updates: Vec<StoreUpdate>,
    pub follow_ups: Vec<Intent>,
}

fn refresh_objective(objective_id: &ObjectiveId) -> Intent {
    Intent::LoadObjective {
        objective_id: objective_id.clone(),
    }
}

/// Maps successful reply data for `context`.
pub fn reply_effects(context: &Context, data: ReplyData) -> Effects {
    let mut effects = Effects::default();
    let updates = &mut effects.updates;
    if let Context::Command {
        objective_id: Some(objective_id),
        ..
    } = context
    {
        updates.push(StoreUpdate::CommandStatus {
            objective_id: objective_id.clone(),
            status: None,
        });
    }
    if compat_effects(context, &data, &mut effects) {
        return effects;
    }
    let updates = &mut effects.updates;
    match (context, data) {
        (_, ReplyData::Snapshot(s)) => {
            updates.push(StoreUpdate::Snapshot {
                workspaces: s.workspaces.iter().map(convert::workspace).collect(),
                objectives: s.objectives.iter().map(convert::summary).collect(),
                settings: convert::settings(&s.settings),
            });
            // The catalogue and the profile inventory are service authority, loaded on connect.
            effects
                .follow_ups
                .extend([Intent::LoadModels { provider: None }, Intent::LoadResources]);
        }
        (_, ReplyData::WorkspaceOpen(w)) => {
            updates.push(StoreUpdate::WorkspaceOpened(convert::workspace(&w)));
        }
        (_, ReplyData::ObjectiveCreate(d)) => {
            let detail = convert::detail(&d);
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
            | ReplyData::ObjectiveCleanup(d),
        ) => updates.push(StoreUpdate::ObjectiveDetail(Box::new(convert::detail(&d)))),
        (Context::Activity(o), ReplyData::ActivityPage(p)) => {
            updates.push(StoreUpdate::ActivityPage {
                objective_id: o.clone(),
                items: p.items.iter().map(convert::activity_item).collect(),
                has_more: p.has_more,
            });
        }
        (Context::Changes(o, basis), ReplyData::ChangesList(l)) => {
            updates.push(StoreUpdate::Changes(convert::change_list(o, basis, &l)));
        }
        (Context::Diff(o, basis, path), ReplyData::DiffGet(d)) => {
            updates.push(StoreUpdate::Diff(convert::file_diff(o, basis, path, &d)));
        }
        (Context::File(o, basis, path, side), ReplyData::FileRead(c)) => {
            updates.push(StoreUpdate::FilePage(convert::file_page(
                o, basis, path, *side, &c,
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
        (_, ReplyData::MessageSend(i) | ReplyData::FeedbackSubmit(i)) => {
            let input = convert::input(&i);
            let objective_id = input.objective_id.clone();
            updates.push(StoreUpdate::InputChanged {
                objective_id: objective_id.clone(),
                input,
            });
            if matches!(context, Context::Command { batch, .. } if !batch.is_empty()) {
                effects.follow_ups.push(Intent::LoadDrafts {
                    objective_id: objective_id.clone(),
                });
            }
            effects.follow_ups.push(refresh_objective(&objective_id));
        }
        (_, ReplyData::DraftSave(d)) => updates.push(StoreUpdate::DraftChanged(convert::draft(&d))),
        (Context::Command { draft_id, .. }, ReplyData::DraftDelete(_)) => {
            if let Some(draft_id) = draft_id {
                updates.push(StoreUpdate::DraftDeleted {
                    draft_id: draft_id.clone(),
                });
            }
        }
        (
            _,
            ReplyData::ReviewFreeze(_) | ReplyData::ReviewRefresh(_) | ReplyData::ReviewApprove(_),
        ) => {
            // The detail (candidates, approval, available actions) is the authority to show.
            if let Some(o) = context.objective_id() {
                effects.follow_ups.push(refresh_objective(o));
            }
        }
        (_, ReplyData::ReviewVerify(v)) => {
            updates.push(StoreUpdate::VerificationChanged(convert::verification(&v)));
        }
        (_, ReplyData::SettingsUpdate(s)) => {
            updates.push(StoreUpdate::Settings(convert::settings(&s)));
        }
        (_, ReplyData::OperationGet(_)) => {}
        (Context::Terminal(o, _), ReplyData::TerminalOpen(info)) => {
            let size = |c: &Counter| u16::try_from(c.value()).unwrap_or(u16::MAX);
            updates.push(StoreUpdate::TerminalOpened {
                objective_id: o.clone(),
                terminal_id: info.terminal_id.as_str().to_owned(),
                mode: match info.mode {
                    wire::TerminalMode::ReadWrite => TerminalMode::ReadWrite,
                    wire::TerminalMode::ReadOnly => TerminalMode::ReadOnly,
                },
                cols: size(&info.cols),
                rows: size(&info.rows),
            });
        }
        (_, ReplyData::ServiceStop(_)) => updates.push(StoreUpdate::ServiceStopped),
        (
            _,
            ReplyData::TerminalInput(_)
            | ReplyData::TerminalResize(_)
            | ReplyData::TerminalClose(_)
            | ReplyData::TerminalOpen(_),
        ) => {}
        // Data that does not match its context: the reply is ignored rather than misfiled.
        (
            _,
            ReplyData::ActivityPage(_)
            | ReplyData::ChangesList(_)
            | ReplyData::DiffGet(_)
            | ReplyData::FileRead(_)
            | ReplyData::Search(_)
            | ReplyData::DraftsList(_)
            | ReplyData::InputsList(_)
            | ReplyData::DraftDelete(_),
        ) => {}
        // Handled by `compat_effects` above.
        (
            _,
            ReplyData::ModelsList(_)
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
    effects
}

/// The developer-facing sentence for a rejection: label, code, service detail and safe action.
pub fn rejection_message(label: &str, rejection: &Rejection) -> String {
    let code = convert::error_code(rejection.code);
    let mut message = format!("{label} was refused ({code})");
    if !rejection.detail.is_empty() {
        message.push_str(" — ");
        message.push_str(&rejection.detail);
    }
    if !message.ends_with('.') {
        message.push('.');
    }
    if let Some(action) = convert::safe_action(rejection.action) {
        message.push(' ');
        message.push_str(action);
    }
    message
}

/// Maps a rejection for `context` onto the surface that asked.
pub fn rejection_effects(context: &Context, rejection: &Rejection) -> Effects {
    let mut effects = Effects::default();
    let code = convert::error_code(rejection.code).to_owned();
    let mut message = rejection_message(context.label(), rejection);
    let updates = &mut effects.updates;
    match context {
        Context::Objective(o) => updates.push(StoreUpdate::ObjectiveLoadFailed {
            objective_id: o.clone(),
            message,
        }),
        Context::Changes(o, basis) => updates.push(StoreUpdate::ChangesFailed {
            objective_id: o.clone(),
            basis: basis.clone(),
            message,
        }),
        Context::Diff(o, _, path) | Context::File(o, _, path, _) => {
            updates.push(StoreUpdate::ViewerFailed {
                objective_id: o.clone(),
                path: path.clone(),
                message,
            });
        }
        Context::Search { .. } => updates.push(StoreUpdate::SearchFailed { message }),
        Context::Terminal(o, _) => updates.push(StoreUpdate::TerminalProblem {
            objective_id: o.clone(),
            message,
        }),
        Context::Stop => updates.push(StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Warning,
            code,
            message,
        })),
        Context::Snapshot
        | Context::Activity(_)
        | Context::Drafts(_)
        | Context::Inputs(_)
        | Context::Models(_)
        | Context::Tree(_)
        | Context::Resources => {
            updates.push(StoreUpdate::Notice(ServiceNotice {
                level: NoticeLevel::Warning,
                code,
                message,
            }));
        }
        Context::Command {
            objective_id,
            draft_id,
            batch,
            ..
        } => {
            if rejection.code == wire::ErrorCode::StaleReference && !batch.is_empty() {
                let stale: Vec<&str> = batch
                    .iter()
                    .filter(|d| rejection.detail.contains(d.as_str()))
                    .map(DraftId::short)
                    .collect();
                if !stale.is_empty() {
                    message.push_str(&format!(
                        " Stale draft{}: {}. Re-anchor or remove {} and submit again.",
                        if stale.len() == 1 { "" } else { "s" },
                        stale.join(", "),
                        if stale.len() == 1 { "it" } else { "them" },
                    ));
                }
            }
            if let Some(draft_id) = draft_id
                && objective_id.is_some()
            {
                // A refused save keeps the text locally and offers Retry beside the draft.
                updates.push(StoreUpdate::DraftSaveFailed {
                    draft_id: draft_id.clone(),
                    message: format!("{code}: {}", rejection.detail),
                });
            }
            let notice = ServiceNotice {
                level: NoticeLevel::Warning,
                code,
                message,
            };
            if let Some(o) = objective_id {
                updates.push(StoreUpdate::CommandStatus {
                    objective_id: o.clone(),
                    status: Some(notice.clone()),
                });
                // Re-read the authority projection so actions show their current reasons.
                effects.follow_ups.push(refresh_objective(o));
                if !batch.is_empty() || draft_id.is_some() {
                    effects.follow_ups.push(Intent::LoadDrafts {
                        objective_id: o.clone(),
                    });
                }
            }
            updates.push(StoreUpdate::Notice(notice));
        }
    }
    effects
}

#[cfg(test)]
pub(crate) mod tests;
