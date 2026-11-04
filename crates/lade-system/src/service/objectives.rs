//! Workspace, Objective and settings requests: opening a workspace, defining an Objective,
//! updating its intent, and changing service settings.

use std::collections::HashMap;
use std::path::PathBuf;

use lade_core::Revision;
use lade_core::domain::ids::{ObjectiveId, WorkspaceId};
use lade_core::domain::objectives::{BranchName, Intent, NewObjective, Objective, Title};
use lade_core::domain::review::Review;
use lade_protocol::client::{
    ErrorCode, ObjectiveCreateCommand, Rejection, ReplyData, SafeAction, SettingsUpdateCommand,
    Start, UpdateIntentCommand, WorkspaceOpenCommand,
};

use crate::adapters::git;
use crate::storage::{self, ObjectiveRow, WorkspaceRow};

use super::convert::{
    domain_rejection, from_wire_git, git_rejection, invalid, not_found, parse_uuid,
};
use super::handlers::sql;
use super::jobs::{self};
use super::state::{ObjState, WsState};
use super::{ReqCtx, Service};
use crate::support::{new_uuid, now_ms};

const REPLY_BRANCHES: usize = 256;

impl Service {
    pub(super) fn workspace_open(
        &mut self,
        cmd: &WorkspaceOpenCommand,
    ) -> Result<ReplyData, Rejection> {
        let path = PathBuf::from(cmd.path.to_os_string());
        if !path.is_absolute() {
            return Err(invalid("workspace path must be absolute"));
        }
        let repo = jobs::open_repository(&path).map_err(git_rejection)?;
        let top = repo.work_tree().to_path_buf();
        if let Some(existing) = self.workspaces.values().find(|ws| ws.path == top) {
            return Ok(ReplyData::WorkspaceOpen(Self::wire_workspace(
                existing,
                REPLY_BRANCHES,
            )));
        }
        let (row, fresh) =
            if let Some(row) = sql(storage::workspace_by_path(self.store.conn(), &top))? {
                (row, false)
            } else {
                let row = WorkspaceRow {
                    id: WorkspaceId::new(new_uuid()),
                    path: top.clone(),
                    name: top.file_name().map_or_else(
                        || "workspace".to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                    default_branch: repo.head_branch().ok().flatten(),
                    opened_at: now_ms(),
                };
                (row, true)
            };
        let ws = WsState {
            id: row.id,
            repo,
            path: top,
            name: row.name.clone(),
            default_branch: row.default_branch.clone(),
        };
        self.check_snapshot_representation(&self.settings, None, Some(&ws))?;
        if fresh {
            sql(storage::insert_workspace(self.store.conn(), &row))?;
        }
        let reply = Self::wire_workspace(&ws, REPLY_BRANCHES);
        self.workspaces.insert(ws.id, ws);
        Ok(ReplyData::WorkspaceOpen(reply))
    }

    pub(super) fn objective_create(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &ObjectiveCreateCommand,
    ) -> Result<ReplyData, Rejection> {
        let workspace = WorkspaceId::new(parse_uuid(&cmd.workspace_id)?);
        let ws = self
            .workspaces
            .get(&workspace)
            .ok_or_else(|| not_found("workspace"))?;
        let title = Title::new(&cmd.title).map_err(domain_rejection)?;
        let intent = Intent::new(
            &cmd.intent.outcome,
            &cmd.intent.constraints,
            &cmd.intent.acceptance,
        )
        .map_err(domain_rejection)?;
        if cmd.integration_target.contains("\\x")
            && git::unescape_branch_name(&cmd.integration_target)
                .is_some_and(|raw| std::str::from_utf8(&raw).is_err())
        {
            return Err(Rejection::new(
                ErrorCode::UnsupportedContent,
                SafeAction::None,
                "branch names that are not UTF-8 are listed but cannot be an integration target",
            ));
        }
        let target = BranchName::new(&cmd.integration_target).map_err(domain_rejection)?;
        let target_commit = jobs::target_commit(&ws.repo, target.as_str())?;
        let start_commit = match &cmd.start {
            Start::Target => target_commit,
            Start::Commit { commit } => {
                let requested = from_wire_git(commit)?;
                let resolved = ws
                    .repo
                    .resolve_commit(requested.hex())
                    .map_err(git_rejection)?;
                super::convert::core_git(&resolved)
            }
        };
        if start_commit.algorithm() != super::convert::core_algorithm(ws.repo.object_format()) {
            return Err(invalid(
                "start commit object format differs from the repository",
            ));
        }
        let oid = ObjectiveId::new(new_uuid());
        let objective = Objective::define(
            NewObjective {
                id: oid,
                workspace_id: workspace,
                title,
                intent,
                integration_target: target,
                start_commit,
            },
            lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
        );
        let st = ObjState {
            row: ObjectiveRow {
                objective,
                git_branch: None,
                worktree: None,
                integration_conflict: false,
                history_deleted: false,
            },
            review: Review::new(oid),
            drafts: Vec::new(),
            draft_stale: HashMap::new(),
            inputs: Vec::new(),
            capture: None,
            tails: HashMap::new(),
        };
        let detail = self.detail_of(&st);
        self.check_intent_representation(&detail)?;
        self.check_snapshot_representation(&self.settings, Some(&st), None)?;
        let data = ReplyData::ObjectiveCreate(Box::new(detail));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    pub(super) fn objective_update_intent(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &UpdateIntentCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let intent = Intent::new(
            &cmd.intent.outcome,
            &cmd.intent.constraints,
            &cmd.intent.acceptance,
        )
        .map_err(domain_rejection)?;
        let mut st = self.obj(oid)?.clone();
        st.objective_mut()
            .update_intent(
                Revision::new(cmd.expected_intent_revision.value()),
                intent,
                lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
            )
            .map_err(domain_rejection)?;
        let detail = self.detail_of(&st);
        self.check_intent_representation(&detail)?;
        let data = ReplyData::ObjectiveUpdateIntent(Box::new(detail));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    pub(super) fn settings_update(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: SettingsUpdateCommand,
    ) -> Result<ReplyData, Rejection> {
        let mut settings = self.settings.clone();
        if let Some(model) = cmd.default_model {
            lade_core::domain::execution::ModelRef::new(&model.provider, &model.id)
                .map_err(domain_rejection)?;
            settings.default_model = Some(model);
        }
        if let Some(commands) = cmd.verification_commands {
            let valid = commands.iter().all(|argv| {
                argv.first().is_some_and(|p| !p.is_empty())
                    && argv.iter().all(|a| !a.contains('\0'))
            });
            if !valid {
                return Err(invalid(
                    "verification commands need a program and no NUL bytes",
                ));
            }
            settings.verification_commands = commands;
        }
        let data = ReplyData::SettingsUpdate(settings.clone());
        self.check_snapshot_representation(&settings, None, None)?;
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_settings(tx, &settings)
        })?;
        self.settings = settings;
        Ok(data)
    }
}
