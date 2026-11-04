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
use crate::support::{new_uuid, now_ms, ts_now};

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
            ts_now(),
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
        let detail = self.fresh_detail(&st);
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
                ts_now(),
            )
            .map_err(domain_rejection)?;
        let detail = self.fresh_detail(&st);
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

#[cfg(test)]
mod tests {
    use lade_protocol::client::{Intent, ObjectiveSelector, Request};
    use lade_protocol::types::{Counter, GitId, Id, Model, WirePath};

    use super::*;
    use crate::adapters_support::{commit_all, git};
    use crate::service::test_harness::Harness;

    fn open(harness: &mut Harness, path: &std::path::Path) -> Result<ReplyData, Rejection> {
        harness.call(Request::WorkspaceOpen(WorkspaceOpenCommand {
            path: WirePath::from_os_str(path.as_os_str()).expect("path"),
        }))
    }

    fn create(
        harness: &mut Harness,
        workspace: &Id,
        title: &str,
        target: &str,
        start: Start,
    ) -> Result<ReplyData, Rejection> {
        harness.call(Request::ObjectiveCreate(ObjectiveCreateCommand {
            workspace_id: workspace.clone(),
            title: title.into(),
            intent: Intent {
                outcome: "outcome".into(),
                constraints: String::new(),
                acceptance: String::new(),
            },
            integration_target: target.into(),
            start,
        }))
    }

    #[test]
    fn a_workspace_opens_once_and_only_from_the_top_of_an_absolute_work_tree() {
        let mut harness = Harness::new("objectives-open");
        let repo = harness.repo.clone();
        let Ok(ReplyData::WorkspaceOpen(first)) = open(&mut harness, &repo) else {
            panic!("open");
        };
        assert_eq!(first.default_branch.as_deref(), Some("main"));
        assert_eq!(first.branches, ["main"]);
        std::fs::create_dir(repo.join("sub")).expect("subdir");
        assert!(
            open(&mut harness, &repo.join("sub")).is_err(),
            "only the top of a work tree opens"
        );
        let Ok(ReplyData::WorkspaceOpen(again)) = open(&mut harness, &repo.join(".")) else {
            panic!("reopen");
        };
        assert_eq!(again.workspace_id, first.workspace_id);
        let rows = storage::load_workspaces(harness.service.store.conn()).expect("rows");
        assert_eq!(rows.len(), 1, "reopening records nothing new");

        let relative = open(&mut harness, std::path::Path::new("relative/repo"));
        assert_eq!(
            relative.expect_err("relative").code,
            ErrorCode::InvalidRequest
        );
        let plain = harness.root().join("plain");
        std::fs::create_dir(&plain).expect("plain dir");
        assert!(open(&mut harness, &plain).is_err(), "not a repository");
    }

    #[test]
    fn objectives_start_at_the_target_or_a_named_commit_and_are_recorded() {
        let mut harness = Harness::new("objectives-create");
        let workspace = harness.open_workspace();
        let first = git(&harness.repo, &["rev-parse", "HEAD"]);
        std::fs::write(harness.repo.join("next"), "next\n").expect("write");
        let second = commit_all(&harness.repo, "second");

        let Ok(ReplyData::ObjectiveCreate(at_target)) =
            create(&mut harness, &workspace, "At target", "main", Start::Target)
        else {
            panic!("create at target");
        };
        assert_eq!(at_target.start_commit.hex(), second.trim());
        let named = Start::Commit {
            commit: at_target.start_commit.clone(),
        };
        let Ok(ReplyData::ObjectiveCreate(at_commit)) = create(
            &mut harness,
            &workspace,
            "At commit",
            "main",
            Start::Commit {
                commit: GitId::from_hex(first.trim()).expect("git id"),
            },
        ) else {
            panic!("create at commit");
        };
        assert_eq!(at_commit.start_commit.hex(), first.trim());
        assert!(create(&mut harness, &workspace, "Named", "main", named).is_ok());

        let stored = storage::load_objectives(harness.service.store.conn()).expect("load");
        assert_eq!(stored.len(), 3);
        assert!(
            stored.iter().all(|row| row.objective.lifecycle()
                == lade_core::domain::objectives::Lifecycle::Defined)
        );
    }

    #[test]
    fn objective_definitions_are_validated_before_anything_is_recorded() {
        let mut harness = Harness::new("objectives-invalid");
        let workspace = harness.open_workspace();
        let unknown = super::super::wire_id(new_uuid());
        let missing = create(&mut harness, &unknown, "t", "main", Start::Target);
        assert_eq!(
            missing.expect_err("unknown workspace").code,
            ErrorCode::NotFound
        );
        assert!(create(&mut harness, &workspace, "", "main", Start::Target).is_err());
        assert!(
            create(
                &mut harness,
                &workspace,
                "t",
                "no-such-branch",
                Start::Target
            )
            .is_err()
        );
        let not_utf8 = create(&mut harness, &workspace, "t", "bad\\xff", Start::Target);
        assert_eq!(
            not_utf8.expect_err("non-UTF-8 target").code,
            ErrorCode::UnsupportedContent
        );
        assert!(
            storage::load_objectives(harness.service.store.conn())
                .expect("load")
                .is_empty()
        );
    }

    #[test]
    fn intent_updates_need_the_current_revision() {
        let mut harness = Harness::new("objectives-intent");
        let (id, oid) = harness.create_objective("Intent");
        let update = |revision: u64, outcome: &str| {
            Request::ObjectiveUpdateIntent(UpdateIntentCommand {
                objective_id: id.clone(),
                expected_intent_revision: Counter::new(revision),
                intent: Intent {
                    outcome: outcome.into(),
                    constraints: "c".into(),
                    acceptance: "a".into(),
                },
            })
        };
        let Ok(ReplyData::ObjectiveUpdateIntent(detail)) = harness.call(update(1, "better")) else {
            panic!("update");
        };
        assert_eq!(detail.intent_revision.value(), 2);
        assert_eq!(detail.intent.outcome, "better");
        let stale = harness.call(update(1, "stale"));
        assert_eq!(stale.expect_err("stale revision").code, ErrorCode::Conflict);
        assert_eq!(
            harness.service.objectives[&oid]
                .objective()
                .record()
                .intent
                .outcome(),
            "better"
        );
        harness.service.rt(oid).busy = Some("freeze");
        let busy = harness.call(update(2, "busy"));
        assert_eq!(busy.expect_err("busy").code, ErrorCode::Conflict);
        let get = harness.call(Request::ObjectiveGet(ObjectiveSelector {
            objective_id: id,
        }));
        let Ok(ReplyData::ObjectiveGet(detail)) = get else {
            panic!("get");
        };
        assert_eq!(detail.intent.outcome, "better");
    }

    #[test]
    fn settings_reject_commands_without_a_program_and_persist_valid_ones() {
        let mut harness = Harness::new("objectives-settings");
        let update = |commands: Vec<Vec<String>>, model: Option<Model>| {
            Request::SettingsUpdate(SettingsUpdateCommand {
                default_model: model,
                verification_commands: Some(commands),
            })
        };
        for bad in [
            vec![Vec::new()],
            vec![vec![String::new()]],
            vec![vec!["cargo".into(), "te\0st".into()]],
        ] {
            let rejected = harness.call(update(bad, None));
            assert_eq!(
                rejected.expect_err("invalid").code,
                ErrorCode::InvalidRequest
            );
        }
        let model = Model {
            provider: "anthropic".into(),
            id: "claude".into(),
        };
        let good = vec![vec!["cargo".to_string(), "test".to_string()]];
        let Ok(ReplyData::SettingsUpdate(saved)) = harness.call(update(good.clone(), Some(model)))
        else {
            panic!("settings");
        };
        assert_eq!(saved.verification_commands, good);
        assert_eq!(harness.service.settings, saved);
        assert_eq!(
            storage::load_settings(harness.service.store.conn()).expect("load"),
            Some(saved)
        );
        let bad_model = Model {
            provider: String::new(),
            id: "x".into(),
        };
        assert!(harness.call(update(Vec::new(), Some(bad_model))).is_err());
        assert_eq!(
            harness.service.settings.verification_commands, good,
            "unchanged"
        );
    }
}
