//! The LADE profile's pi resources: the inventory shown to the developer, approve/reject
//! decisions for executable extensions, copy-only profile import, and the per-session snapshot
//! that holds only approved extensions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lade_protocol::client::{
    ErrorCode, Event, ProfileImportCommand, ProfileImportResult, Rejection, ReplyData,
    ResourceDecideCommand, ResourceEntry, ResourceInventory, ResourceKind, ResourceState,
    SafeAction,
};
use lade_protocol::types::{Digest, WirePath};
use lade_protocol::worker::{WorkerExtension, WorkerResources};

use crate::adapters::profile::{self, Kind, Materialized};
use crate::storage;
use crate::support::fs::ensure_private_dir;

use super::convert::{invalid, preview};
use super::{Completed, Flow, ReqCtx, Service, StartError};
use crate::support::now_ms;

pub(crate) const RESTRICTIONS: &[&str] = &[
    "Repository .pi/ resources, .agents/skills, project settings and packages are never loaded (project trust is \"never\").",
    "Your own ~/.pi profile is never read or changed; only an explicit, copy-only import reads it.",
    "SYSTEM.md / APPEND_SYSTEM.md replacements, themes and pi packages are not loaded; the LADE system note is appended.",
    "Executable extensions load only by approved content hash; a new or changed extension needs approval and a reload.",
    "Extensions cannot start, fork or switch sessions; use the LADE session controls.",
    "Terminal-only extension UI (custom components, editor control) fails explicitly; dialogs are never answered automatically.",
    "Providers registered by extensions and interactive OAuth logins have no egress policy or credential custody and are restricted.",
    "Custom providers (models.json) may not run shell commands and reference credentials only as $LADE_PROVIDER_* variables.",
];

impl Service {
    pub(crate) fn profile_dir(&self) -> PathBuf {
        self.paths.state.join("profile")
    }

    pub(crate) fn load_resources(&mut self) -> Result<(), StartError> {
        let profile = self.profile_dir();
        ensure_private_dir(&profile).map_err(|e| StartError::PrivateDir(profile, e))?;
        for row in storage::load_approvals(self.store.conn())? {
            self.extension_approvals
                .insert((row.name, row.sha256), row.approved);
        }
        Ok(())
    }

    pub(crate) fn approved_extensions(&self) -> Vec<(String, String)> {
        self.extension_approvals
            .iter()
            .filter(|(_, approved)| **approved)
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) fn inventory(&self) -> ResourceInventory {
        self.inventory_with(&self.extension_approvals)
    }

    /// The inventory as it reads under `approvals`, which may differ from the recorded decisions.
    fn inventory_with(&self, approvals: &HashMap<(String, String), bool>) -> ResourceInventory {
        let profile = self.profile_dir();
        let entries = profile::scan(&profile)
            .into_iter()
            .map(|entry| {
                let kind = match entry.kind {
                    Kind::Context => ResourceKind::Context,
                    Kind::Skill => ResourceKind::Skill,
                    Kind::Prompt => ResourceKind::Prompt,
                    Kind::Extension => ResourceKind::Extension,
                    Kind::Models => ResourceKind::Models,
                };
                let sha256 = entry
                    .sha256
                    .as_deref()
                    .and_then(|s| Digest::parse(s.to_string()).ok());
                let (state, reason) = match (&entry.problem, entry.kind, &entry.sha256) {
                    (Some(problem), _, _) => (ResourceState::Refused, Some(problem.clone())),
                    (None, Kind::Extension, Some(sha)) => {
                        match approvals.get(&(entry.name.clone(), sha.clone())) {
                            Some(true) => (ResourceState::Active, None),
                            Some(false) => (
                                ResourceState::Rejected,
                                Some("rejected by the developer; it is not loaded".to_string()),
                            ),
                            None => (
                                ResourceState::PendingApproval,
                                Some(
                                    "new or changed executable extension: approve this exact content, then reload the session"
                                        .to_string(),
                                ),
                            ),
                        }
                    }
                    _ => (ResourceState::Active, None),
                };
                ResourceEntry {
                    kind,
                    name: preview(&entry.name, 256),
                    state,
                    sha256,
                    reason: reason.map(|r| preview(&r, 512)),
                }
            })
            .collect();
        ResourceInventory {
            profile_dir: WirePath::from_os_str(profile.as_os_str())
                .unwrap_or_else(|_| super::convert::wire_path(b"profile")),
            entries,
            restrictions: RESTRICTIONS.iter().map(|r| preview(r, 512)).collect(),
        }
    }

    pub(crate) fn resource_decide(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &ResourceDecideCommand,
    ) -> Result<ReplyData, Rejection> {
        let sha = cmd.sha256.as_str().to_string();
        let current = profile::scan(&self.profile_dir()).into_iter().any(|e| {
            e.kind == Kind::Extension && e.name == cmd.name && e.sha256.as_deref() == Some(&sha)
        });
        if !current {
            return Err(Rejection::new(
                ErrorCode::StaleReference,
                SafeAction::Refresh,
                "This extension changed since it was listed. Review it again.",
            ));
        }
        let key = (cmd.name.clone(), sha);
        let mut approvals = self.extension_approvals.clone();
        approvals.insert(key.clone(), cmd.approve);
        let inventory = self.inventory_with(&approvals);
        let data = ReplyData::ResourceDecide(inventory.clone());
        let now = now_ms();
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_approval(
                tx,
                &storage::ApprovalRow {
                    name: key.0.clone(),
                    sha256: key.1.clone(),
                    approved: cmd.approve,
                },
                now,
            )
        })?;
        self.extension_approvals.insert(key, cmd.approve);
        self.emit(Event::ResourcesChanged(inventory));
        Ok(data)
    }

    pub(crate) fn profile_import(&mut self, ctx: ReqCtx, cmd: &ProfileImportCommand) -> Flow {
        let source = PathBuf::from(cmd.source.to_os_string());
        if !source.is_absolute() {
            self.finish(
                ctx,
                Err(invalid("Choose the folder to import by its full path.")),
            );
            return Flow::Done;
        }
        let profile = self.profile_dir();
        if source.starts_with(&profile) || profile.starts_with(&source) {
            self.finish(
                ctx,
                Err(invalid(
                    "Choose a folder outside the LADE profile to import from.",
                )),
            );
            return Flow::Done;
        }
        self.spawn(move || Completed::ProfileImport {
            ctx,
            result: profile::import(&source, &profile)
                .map_err(|e| format!("The profile could not be imported ({}).", e.kind())),
        });
        Flow::Deferred
    }

    pub(crate) fn profile_import_done(
        &mut self,
        ctx: ReqCtx,
        result: Result<profile::ImportReport, String>,
    ) {
        let reply = match result {
            Ok(report) => {
                self.catalog.data = None;
                let inventory = self.inventory();
                self.emit(Event::ResourcesChanged(inventory.clone()));
                Ok(ReplyData::ProfileImport(ProfileImportResult {
                    copied: report.copied.iter().map(|c| preview(c, 256)).collect(),
                    skipped: report.skipped.iter().map(|s| preview(s, 512)).collect(),
                    inventory,
                }))
            }
            Err(why) => Err(Rejection::new(
                ErrorCode::PermissionDenied,
                SafeAction::None,
                &why,
            )),
        };
        self.finish(ctx, reply);
    }
}

pub(crate) fn snapshot(
    source: &Path,
    dest: &Path,
    approved: &[(String, String)],
) -> std::io::Result<WorkerResources> {
    let extensions = profile::materialize(source, dest, |name, sha| {
        approved.iter().any(|(n, s)| n == name && s == sha)
    })?;
    Ok(WorkerResources {
        profile_dir: dest.to_string_lossy().into_owned(),
        extensions: extensions
            .into_iter()
            .take(64)
            .map(|Materialized { name, path, sha256 }| WorkerExtension {
                name,
                path: path.to_string_lossy().into_owned(),
                sha256,
            })
            .collect(),
    })
}
