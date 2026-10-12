//! IMP-011 pi compatibility in the service: the SDK model catalogue with provider access, the
//! LADE-profile resource inventory and its hash-keyed approvals, copy-only profile import, session
//! operations (tree, navigate, fork/clone, import, export, compaction, reload, model/thinking) routed
//! to the live worker, and semantic extension dialogs bound to session generations.
//!
//! Trust rules: approvals and Git state live only in the product store and the repository; nothing
//! a pi session file or the worker reports can create them. A fork or import replaces the Agent
//! Session's destination (association revision advances), so input addressed to the previous
//! session is never delivered to the replacement. Tree navigation is session-only.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lade_core::domain::execution::{ExecutionGeneration, ExecutionState, ModelRef};
use lade_core::domain::ids::{ObjectiveId, SessionId, Timestamp};
use lade_protocol::client::{
    Empty, ErrorCode, Event, ModelCatalog, ModelInfo, ModelsListQuery, NoticeLevel,
    ProfileImportCommand, ProfileImportResult, ProviderAuth, ProviderInfo, ProviderStatus,
    Rejection, ReplyData, ResourceDecideCommand, ResourceEntry, ResourceInventory, ResourceKind,
    ResourceState, SafeAction, SessionCompactCommand, SessionConfigureCommand, SessionExport,
    SessionForkCommand, SessionImportCommand, SessionNavigateCommand, SessionState, SessionTree,
    UiClosed, UiRequest, UiRespondCommand,
};
use lade_protocol::types::{
    Digest, Id, Model, SessionChangeReason, ThinkingLevel, TreeEntry, UiCloseReason, UiPrompt,
    WirePath,
};
use lade_protocol::worker::{
    LoadStatus, LoadedKind, LoadedResource, ResourceSource, SessionCommand, WorkerBody,
    WorkerExtension, WorkerResources,
};
use serde_json::Value;

use crate::adapters::profile::{self, Kind, Materialized};
use crate::storage;
use crate::supervision::credentials::FIXTURE_PROVIDER;
use crate::supervision::{AccessStatus, AuthKind, ProviderFacts};

use super::convert::{
    conflict, counter, domain_rejection, invalid, new_uuid, not_found, now_ms, preview, rejection,
    wire_id,
};
use super::handlers::{Pending, sql};
use super::{Flow, JobOutput, ReqCtx, Service};

/// A session command that has not answered within this bound fails as interrupted.
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
/// Largest session file accepted for import.
pub(crate) const MAX_IMPORT_BYTES: u64 = 16 << 20;
const MAX_PROVIDERS: usize = 128;
const MAX_REPLY_MODELS: usize = 512;
const MAX_CATALOG_MODELS: usize = 4096;

/// Trust-rule restrictions shown with the resource inventory (never silent omissions).
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

/// The SDK catalogue as parsed from the sandboxed catalogue run.
#[derive(Clone, Debug, Default)]
pub(crate) struct CatalogData {
    pub sdk_version: String,
    pub providers: Vec<(ProviderFacts, Vec<ModelInfo>)>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Tree,
    Navigate,
    Fork,
    Import,
    Export,
    Compact,
    Reload,
    Configure,
}

pub(crate) struct PendingCommand {
    pub id: String,
    pub ctx: ReqCtx,
    pub op: Op,
    pub since: Instant,
    pub export: Option<PathBuf>,
}

/// What the worker last reported about its session runtime.
#[derive(Clone, Debug)]
pub(crate) struct PiState {
    pub generation: u64,
    pub reason: SessionChangeReason,
    pub pi_session_id: String,
    pub model: Model,
    pub thinking: ThinkingLevel,
    pub leaf_id: Option<String>,
}

pub(crate) struct OpenUi {
    pub generation: u64,
    pub prompt: UiPrompt,
}

/// Non-durable per-Objective session facts of the live worker.
#[derive(Default)]
pub(crate) struct LiveSession {
    pub command: Option<PendingCommand>,
    pub pi: Option<PiState>,
    pub ui: BTreeMap<String, OpenUi>,
    /// The session generation whose loaded resources were last reported as activity.
    pub reported: Option<u64>,
}

#[derive(Default)]
pub(crate) struct CompatState {
    pub thinking: HashMap<ObjectiveId, ThinkingLevel>,
    pub approvals: HashMap<(String, String), bool>,
    pub catalog: Option<Arc<CatalogData>>,
    pub catalog_running: bool,
    pub catalog_waiting: Vec<(ReqCtx, ModelsListQuery)>,
    pub live: HashMap<ObjectiveId, LiveSession>,
}

fn wire_auth(kind: AuthKind) -> ProviderAuth {
    match kind {
        AuthKind::None => ProviderAuth::None,
        AuthKind::ApiKey => ProviderAuth::ApiKey,
        AuthKind::SubscriptionToken => ProviderAuth::SubscriptionToken,
        AuthKind::OauthLogin => ProviderAuth::OauthLogin,
        AuthKind::CloudCredentials => ProviderAuth::CloudCredentials,
        AuthKind::Custom => ProviderAuth::Custom,
    }
}

fn wire_status(status: AccessStatus) -> ProviderStatus {
    match status {
        AccessStatus::Available => ProviderStatus::Available,
        AccessStatus::MissingCredentials => ProviderStatus::MissingCredentials,
        AccessStatus::Restricted => ProviderStatus::Restricted,
    }
}

fn text(value: Option<&Value>, max: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|s| preview(s, max))
        .filter(|s| !s.is_empty())
}

/// The catalogue when the SDK catalogue cannot run: only the offline test provider when the
/// service runs with the test switch, otherwise nothing.
fn fallback_catalog(test_fixture_provider: bool, error: String) -> CatalogData {
    let fixture = (
        ProviderFacts {
            id: FIXTURE_PROVIDER.into(),
            name: "LADE fixture provider (offline, scripted; not a real model)".into(),
            api_key: true,
            oauth: None,
            subscription: false,
            base_url: None,
        },
        vec![ModelInfo {
            provider: FIXTURE_PROVIDER.into(),
            id: "scripted".into(),
            name: "LADE fixture: scripted (offline test model)".into(),
            reasoning: false,
            thinking_levels: vec![ThinkingLevel::Off],
        }],
    );
    CatalogData {
        sdk_version: "0.85.1".into(),
        providers: if test_fixture_provider {
            vec![fixture]
        } else {
            Vec::new()
        },
        error: Some(error),
    }
}

/// Parses the catalogue output (`src/worker/catalog.ts`), bounding every list and string.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<CatalogData, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "catalogue output is not JSON".to_string())?;
    let sdk_version = text(value.get("sdk_version"), 64).ok_or("catalogue has no SDK version")?;
    let mut providers = Vec::new();
    let mut total = 0usize;
    for entry in value
        .get("providers")
        .and_then(Value::as_array)
        .ok_or("catalogue has no providers")?
        .iter()
        .take(MAX_PROVIDERS)
    {
        let Some(id) = text(entry.get("id"), 128) else {
            continue;
        };
        let facts = ProviderFacts {
            name: text(entry.get("name"), 256).unwrap_or_else(|| id.clone()),
            api_key: entry.get("api_key").and_then(Value::as_bool) == Some(true),
            oauth: text(entry.get("oauth"), 200),
            subscription: entry.get("subscription").and_then(Value::as_bool) == Some(true),
            base_url: text(entry.get("base_url"), 400),
            id: id.clone(),
        };
        let mut models = Vec::new();
        for model in entry
            .get("models")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if total >= MAX_CATALOG_MODELS {
                break;
            }
            let Some(model_id) = text(model.get("id"), 256) else {
                continue;
            };
            let thinking_levels = model
                .get("thinking")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|l| l.as_str().and_then(ThinkingLevel::parse))
                .take(8)
                .collect();
            models.push(ModelInfo {
                provider: id.clone(),
                name: text(model.get("name"), 256).unwrap_or_else(|| model_id.clone()),
                id: model_id,
                reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
                thinking_levels,
            });
            total += 1;
        }
        providers.push((facts, models));
    }
    Ok(CatalogData {
        sdk_version,
        providers,
        error: text(value.get("error"), 512),
    })
}

fn wire_reason(reason: SessionChangeReason) -> &'static str {
    match reason {
        SessionChangeReason::Start => "start",
        SessionChangeReason::Navigate => "navigate",
        SessionChangeReason::Fork => "fork",
        SessionChangeReason::Import => "import",
        SessionChangeReason::Reload => "reload",
        SessionChangeReason::Compact => "compact",
        SessionChangeReason::Configure => "configure",
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

impl Service {
    pub(crate) fn profile_dir(&self) -> PathBuf {
        self.paths.state.join("profile")
    }

    pub(crate) fn load_compat(&mut self) -> Result<(), String> {
        private_dir(&self.profile_dir()).map_err(|e| format!("profile: {}", e.kind()))?;
        let conn = self.store.conn();
        for (oid, level) in storage::load_thinking(conn).map_err(|e| e.to_string())? {
            if let Some(level) = ThinkingLevel::parse(&level) {
                self.compat.thinking.insert(oid, level);
            }
        }
        for (name, sha, approved) in storage::load_approvals(conn).map_err(|e| e.to_string())? {
            self.compat.approvals.insert((name, sha), approved);
        }
        Ok(())
    }

    fn live(&mut self, oid: ObjectiveId) -> &mut LiveSession {
        self.compat.live.entry(oid).or_default()
    }

    /// The developer's thinking selection for `oid` (shown in the Objective detail).
    pub(crate) fn thinking_of(&self, oid: ObjectiveId) -> Option<ThinkingLevel> {
        self.compat.thinking.get(&oid).copied()
    }

    /// Approved profile extensions, as `(name, sha256)`.
    pub(crate) fn approved_extensions(&self) -> Vec<(String, String)> {
        self.compat
            .approvals
            .iter()
            .filter(|(_, approved)| **approved)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Whether a session command is outstanding (input dispatch waits for it).
    pub(crate) fn command_pending(&self, oid: ObjectiveId) -> bool {
        self.compat
            .live
            .get(&oid)
            .is_some_and(|l| l.command.is_some())
    }

    // -----------------------------------------------------------------------------------------
    // Models.

    pub(crate) fn models_list(&mut self, ctx: ReqCtx, query: ModelsListQuery) -> Flow {
        if let Some(data) = self.compat.catalog.clone() {
            let reply = self.catalog_reply(&data, &query);
            self.finish(ctx, Ok(reply));
            return Flow::Done;
        }
        self.compat.catalog_waiting.push((ctx, query));
        if !self.compat.catalog_running {
            self.compat.catalog_running = true;
            let launcher = self.launcher.clone();
            let scratch = self.paths.tmp.join(format!("catalog-{}", new_uuid()));
            let profile = self.profile_dir();
            self.spawn(Pending::Catalog, move || {
                let models = profile.join("models.json");
                let models = profile::profile_providers(&profile)
                    .is_ok_and(|p| !p.is_empty())
                    .then_some(models);
                let result = private_dir(&scratch)
                    .map_err(|e| format!("catalogue scratch: {}", e.kind()))
                    .and_then(|()| {
                        launcher
                            .catalog(&scratch, models.as_deref())
                            .map_err(|e| e.detail())
                    })
                    .and_then(|bytes| parse_catalog(&bytes));
                let _ = fs::remove_dir_all(&scratch);
                JobOutput::Catalog(result)
            });
        }
        Flow::Deferred
    }

    pub(crate) fn catalog_done(&mut self, result: Result<CatalogData, String>) {
        self.compat.catalog_running = false;
        let data = match result {
            Ok(data) => {
                let data = Arc::new(data);
                self.compat.catalog = Some(data.clone());
                data
            }
            Err(why) => {
                eprintln!("laded: model catalogue unavailable");
                Arc::new(fallback_catalog(
                    self.test_fixture_provider,
                    preview(
                        &format!("the SDK model catalogue is unavailable ({why})"),
                        512,
                    ),
                ))
            }
        };
        for (ctx, query) in std::mem::take(&mut self.compat.catalog_waiting) {
            let reply = self.catalog_reply(&data, &query);
            self.finish(ctx, Ok(reply));
        }
    }

    fn catalog_reply(&self, data: &CatalogData, query: &ModelsListQuery) -> ReplyData {
        // The test-only fixture provider is never offered without the service's test switch.
        let offered =
            |facts: &ProviderFacts| self.test_fixture_provider || facts.id != FIXTURE_PROVIDER;
        let mut providers: Vec<ProviderInfo> = data
            .providers
            .iter()
            .filter(|(facts, _)| offered(facts))
            .map(|(facts, models)| {
                let access = self.launcher.provider_access(facts);
                ProviderInfo {
                    provider: facts.id.clone(),
                    name: preview(&facts.name, 256),
                    auth: access.auth.iter().map(|a| wire_auth(*a)).take(8).collect(),
                    status: wire_status(access.status),
                    models: counter(models.len() as u64),
                    reason: access.reason.map(|r| preview(&r, 512)),
                    variables: (!access.variables.is_empty()).then(|| {
                        access
                            .variables
                            .iter()
                            .take(8)
                            .map(|v| preview(v, 128))
                            .collect()
                    }),
                }
            })
            .collect();
        providers.sort_by_key(|p| match p.status {
            ProviderStatus::Available => 0,
            ProviderStatus::MissingCredentials => 1,
            ProviderStatus::Restricted => 2,
        });
        let matching: Vec<&ModelInfo> = data
            .providers
            .iter()
            .filter(|(facts, _)| {
                offered(facts) && query.provider.as_deref() == Some(facts.id.as_str())
            })
            .flat_map(|(_, models)| models)
            .collect();
        ReplyData::ModelsList(ModelCatalog {
            sdk_version: data.sdk_version.clone(),
            providers,
            truncated: matching.len() > MAX_REPLY_MODELS,
            models: matching
                .into_iter()
                .take(MAX_REPLY_MODELS)
                .cloned()
                .collect(),
            error: data.error.clone(),
        })
    }

    // -----------------------------------------------------------------------------------------
    // Resources.

    pub(crate) fn inventory(&self) -> ResourceInventory {
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
                        match self.compat.approvals.get(&(entry.name.clone(), sha.clone())) {
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
            return Err(rejection(
                ErrorCode::StaleReference,
                SafeAction::Refresh,
                "no profile extension with this name currently has this content; list resources again",
            ));
        }
        let key = (cmd.name.clone(), sha);
        let mut approvals = self.compat.approvals.clone();
        approvals.insert(key.clone(), cmd.approve);
        let inventory = {
            let previous = std::mem::replace(&mut self.compat.approvals, approvals);
            let inventory = self.inventory();
            self.compat.approvals = previous;
            inventory
        };
        let data = ReplyData::ResourceDecide(inventory.clone());
        let now = now_ms();
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_approval(tx, &key.0, &key.1, cmd.approve, now)
        })?;
        self.compat.approvals.insert(key, cmd.approve);
        self.emit(Event::ResourcesChanged(inventory));
        Ok(data)
    }

    pub(crate) fn profile_import(&mut self, ctx: ReqCtx, cmd: &ProfileImportCommand) -> Flow {
        let source = PathBuf::from(cmd.source.to_os_string());
        if !source.is_absolute() {
            self.finish(
                ctx,
                Err(invalid("the import source must be an absolute path")),
            );
            return Flow::Done;
        }
        let profile = self.profile_dir();
        if source.starts_with(&profile) || profile.starts_with(&source) {
            self.finish(
                ctx,
                Err(invalid(
                    "the import source must be outside the LADE profile",
                )),
            );
            return Flow::Done;
        }
        self.spawn(Pending::ProfileImport { ctx }, move || {
            JobOutput::Profile(
                profile::import(&source, &profile)
                    .map_err(|e| format!("profile import failed: {}", e.kind())),
            )
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
                // Custom providers may have changed: the catalogue is rebuilt on the next request.
                self.compat.catalog = None;
                let inventory = self.inventory();
                self.emit(Event::ResourcesChanged(inventory.clone()));
                Ok(ReplyData::ProfileImport(ProfileImportResult {
                    copied: report.copied.iter().map(|c| preview(c, 256)).collect(),
                    skipped: report.skipped.iter().map(|s| preview(s, 512)).collect(),
                    inventory,
                }))
            }
            Err(why) => Err(rejection(
                ErrorCode::PermissionDenied,
                SafeAction::None,
                &why,
            )),
        };
        self.finish(ctx, reply);
    }

    // -----------------------------------------------------------------------------------------
    // Session commands.

    /// The live, idle (or, for `tree`, running) worker of `oid`, or why it cannot take a command.
    fn command_target(&mut self, oid: ObjectiveId, op: Op) -> Result<(), Rejection> {
        self.check_idle(oid)?;
        if self.command_pending(oid) {
            return Err(conflict("another session command is in progress"));
        }
        let st = self.obj(oid)?;
        let state = st.objective().execution().state();
        let ready = match op {
            Op::Tree | Op::Export => {
                matches!(state, ExecutionState::Idle | ExecutionState::Running)
            }
            _ => *state == ExecutionState::Idle,
        };
        if !ready || self.runtime.get(&oid).is_none_or(|rt| rt.control.is_none()) {
            return Err(rejection(
                ErrorCode::ExecutionUnavailable,
                SafeAction::Refresh,
                if state.is_live() {
                    "wait until the current turn has finished"
                } else {
                    "start or resume the execution first"
                },
            ));
        }
        Ok(())
    }

    fn send_command(
        &mut self,
        ctx: ReqCtx,
        oid: ObjectiveId,
        op: Op,
        command: SessionCommand,
        export: Option<PathBuf>,
    ) -> Flow {
        let id = new_uuid();
        let body = WorkerBody::Command {
            command_id: wire_id(id),
            command,
        };
        let sent = self
            .rt(oid)
            .control
            .as_mut()
            .map(|control| control.send(body));
        if !matches!(sent, Some(Ok(()))) {
            self.finish(
                ctx,
                Err(rejection(
                    ErrorCode::ExecutionUnavailable,
                    SafeAction::Refresh,
                    "the worker link is gone",
                )),
            );
            return Flow::Done;
        }
        self.live(oid).command = Some(PendingCommand {
            id: id.to_string(),
            ctx,
            op,
            since: Instant::now(),
            export,
        });
        Flow::Deferred
    }

    fn command(&mut self, ctx: ReqCtx, id: &Id, op: Op, command: SessionCommand) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, op).map(|()| oid));
        match prepared {
            Ok(oid) => self.send_command(ctx, oid, op, command, None),
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn session_tree(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        self.command(ctx, id, Op::Tree, SessionCommand::Tree)
    }

    pub(crate) fn session_navigate(&mut self, ctx: ReqCtx, cmd: SessionNavigateCommand) -> Flow {
        let command = SessionCommand::Navigate {
            entry_id: cmd.entry_id,
        };
        self.command(ctx, &cmd.objective_id, Op::Navigate, command)
    }

    pub(crate) fn session_fork(&mut self, ctx: ReqCtx, cmd: SessionForkCommand) -> Flow {
        let command = SessionCommand::Fork {
            entry_id: cmd.entry_id,
            position: cmd.position,
        };
        self.command(ctx, &cmd.objective_id, Op::Fork, command)
    }

    pub(crate) fn session_compact(&mut self, ctx: ReqCtx, cmd: SessionCompactCommand) -> Flow {
        let command = SessionCommand::Compact {
            instructions: cmd.instructions,
        };
        self.command(ctx, &cmd.objective_id, Op::Compact, command)
    }

    fn session_dir(&self, oid: ObjectiveId) -> PathBuf {
        self.paths.sessions.join(oid.to_string())
    }

    pub(crate) fn session_export(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, Op::Export).map(|()| oid));
        match prepared {
            Ok(oid) => {
                let path = self
                    .session_dir(oid)
                    .join("exports")
                    .join(format!("session-{}.jsonl", now_ms()));
                let command = SessionCommand::Export {
                    path: path.to_string_lossy().into_owned(),
                };
                self.send_command(ctx, oid, Op::Export, command, Some(path))
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    /// Copies the chosen JSONL file into the Objective's import directory (the source is only
    /// read), then asks the worker to import that copy with the worktree as working directory.
    pub(crate) fn session_import(&mut self, ctx: ReqCtx, cmd: &SessionImportCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            self.command_target(oid, Op::Import)?;
            let source = PathBuf::from(cmd.path.to_os_string());
            let copy = self
                .session_dir(oid)
                .join("imports")
                .join(format!("{}.jsonl", new_uuid()));
            copy_import(&source, &copy)?;
            Ok((oid, copy))
        })();
        match prepared {
            Ok((oid, copy)) => {
                let command = SessionCommand::Import {
                    path: copy.to_string_lossy().into_owned(),
                };
                self.send_command(ctx, oid, Op::Import, command, None)
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    /// Materialises the current profile with the current approvals, then reloads the session.
    pub(crate) fn session_reload(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, Op::Reload).map(|()| oid));
        let oid = match prepared {
            Ok(oid) => oid,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let source = self.profile_dir();
        let dest = self.session_dir(oid).join("profile");
        let approved = self.approved_extensions();
        self.rt(oid).busy = Some("session reload");
        self.spawn(Pending::Reload { oid, ctx }, move || {
            JobOutput::Snapshot(
                snapshot(&source, &dest, &approved)
                    .map(|resources| (resources, dest))
                    .map_err(|e| format!("profile snapshot failed: {}", e.kind())),
            )
        });
        Flow::Deferred
    }

    pub(crate) fn reload_snapshot_done(
        &mut self,
        oid: ObjectiveId,
        ctx: ReqCtx,
        result: Result<(WorkerResources, PathBuf), String>,
    ) {
        self.rt(oid).busy = None;
        match result
            .map_err(|why| rejection(ErrorCode::DurabilityFailure, SafeAction::Refresh, &why))
        {
            Ok((resources, _)) => {
                if let Err(rejection) = self.command_target(oid, Op::Reload) {
                    self.finish(ctx, Err(rejection));
                    return;
                }
                let _ = self.send_command(
                    ctx,
                    oid,
                    Op::Reload,
                    SessionCommand::Reload { resources },
                    None,
                );
            }
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
    }

    /// Model and thinking selection, persisted per Objective. With a live idle worker the change
    /// applies to the running session (same provider only: a provider's egress policy and
    /// credentials are fixed per launch); otherwise it applies on the next start or resume.
    pub(crate) fn session_configure(
        &mut self,
        mut ctx: ReqCtx,
        cmd: SessionConfigureCommand,
    ) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let model = ModelRef::new(&cmd.model.provider, &cmd.model.id)
                .map_err(|e| domain_rejection(&e))?;
            let st = self.obj(oid)?;
            let session = st
                .objective()
                .execution()
                .session()
                .cloned()
                .ok_or_else(|| domain_rejection(&lade_core::DomainError::ExecutionUnavailable))?;
            let live = st.objective().execution().state().is_live();
            if live && session.model.provider() != model.provider() {
                return Err(invalid(
                    "changing the provider needs a new launch: stop the execution, configure, then resume",
                ));
            }
            Ok((oid, model, live))
        })();
        let (oid, model, live) = match prepared {
            Ok(found) => found,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        if live {
            if let Err(rejection) = self.command_target(oid, Op::Configure) {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
            let command = SessionCommand::Configure {
                model: cmd.model,
                thinking: cmd.thinking,
            };
            return self.send_command(ctx, oid, Op::Configure, command, None);
        }
        if let Err(rejection) = self.preflight(model.provider()) {
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        let result = self.persist_selection(Some(&mut ctx), oid, model, cmd.thinking, |service| {
            service.session_state(oid, SessionChangeReason::Configure)
        });
        self.finish(ctx, result);
        Flow::Done
    }

    /// Records model (Objective row) and thinking (preference) in one transaction.
    fn persist_selection(
        &mut self,
        ctx: Option<&mut ReqCtx>,
        oid: ObjectiveId,
        model: ModelRef,
        thinking: ThinkingLevel,
        reply: impl FnOnce(&Self) -> SessionState,
    ) -> Result<ReplyData, Rejection> {
        let mut st = self.obj(oid)?.clone();
        st.objective_mut()
            .select_model(model, Timestamp::from_unix_ms(now_ms()))
            .map_err(|e| domain_rejection(&e))?;
        let previous_model = self.objectives.insert(oid, st.clone());
        let previous = self.compat.thinking.insert(oid, thinking);
        let data = ReplyData::SessionConfigure(reply(self));
        let now = now_ms();
        let committed = self.commit(ctx, Some(&data), |tx| {
            Self::save_tx(tx, &st)?;
            storage::save_thinking(tx, oid, thinking.as_str(), now)
        });
        if let Err(rejection) = committed {
            match previous {
                Some(level) => self.compat.thinking.insert(oid, level),
                None => self.compat.thinking.remove(&oid),
            };
            if let Some(old) = previous_model {
                self.objectives.insert(oid, old);
            }
            return Err(rejection);
        }
        self.emit_objective(oid);
        Ok(data)
    }

    /// The Objective's current session state for replies and `session.changed`.
    pub(crate) fn session_state(
        &self,
        oid: ObjectiveId,
        reason: SessionChangeReason,
    ) -> SessionState {
        let st = self.objectives.get(&oid);
        let session = st.and_then(|st| st.objective().execution().session().cloned());
        let pi = self.compat.live.get(&oid).and_then(|l| l.pi.clone());
        // Session commands need a bound session; without one the live worker's model (if any).
        let model = session
            .as_ref()
            .map(|s| Model {
                provider: preview(s.model.provider(), 128),
                id: preview(s.model.id(), 256),
            })
            .or_else(|| pi.as_ref().map(|p| p.model.clone()))
            .unwrap_or_else(|| Model {
                provider: String::new(),
                id: String::new(),
            });
        SessionState {
            objective_id: wire_id(oid.uuid()),
            generation: counter(pi.as_ref().map_or(0, |p| p.generation)),
            reason,
            model,
            thinking: pi
                .as_ref()
                .map(|p| p.thinking)
                .or_else(|| self.thinking_of(oid))
                .unwrap_or(ThinkingLevel::Off),
            session_id: session.as_ref().map(|s| wire_id(s.session_id.uuid())),
            association_revision: session
                .as_ref()
                .map(|s| counter(s.association_revision.value())),
            pi_session_id: pi
                .as_ref()
                .map(|p| preview(&p.pi_session_id, 256))
                .filter(|p| !p.is_empty()),
            leaf_id: pi.and_then(|p| p.leaf_id),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Extension dialogs.

    pub(crate) fn ui_respond(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: UiRespondCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        let ui_id = cmd.ui_id.as_str().to_string();
        let live = self.live(oid);
        let current = live.pi.as_ref().map(|p| p.generation);
        let Some(open) = live.ui.get(&ui_id) else {
            return Err(not_found(
                "open dialog (it was answered, closed or replaced)",
            ));
        };
        if open.generation != cmd.generation.value() || current != Some(open.generation) {
            return Err(rejection(
                ErrorCode::StaleExecution,
                SafeAction::Refresh,
                "the dialog belongs to a replaced session generation",
            ));
        }
        let body = WorkerBody::UiRespond {
            ui_id: cmd.ui_id.clone(),
            generation: cmd.generation.clone(),
            answer: cmd.answer,
        };
        let sent = self
            .rt(oid)
            .control
            .as_mut()
            .map(|control| control.send(body));
        if !matches!(sent, Some(Ok(()))) {
            return Err(rejection(
                ErrorCode::ExecutionUnavailable,
                SafeAction::Refresh,
                "the worker link is gone",
            ));
        }
        let data = ReplyData::UiRespond(Empty {});
        self.commit(Some(ctx), Some(&data), |_| Ok(()))?;
        self.live(oid).ui.remove(&ui_id);
        self.emit(Event::UiClosed(UiClosed {
            objective_id: wire_id(oid.uuid()),
            ui_id: cmd.ui_id,
            reason: UiCloseReason::Answered,
        }));
        Ok(data)
    }

    /// Re-announces open dialogs to a newly attached controlling client.
    pub(crate) fn announce_dialogs(&mut self) {
        let mut open = Vec::new();
        for (oid, live) in &self.compat.live {
            for (ui_id, dialog) in &live.ui {
                if let Ok(id) = Id::parse(ui_id.clone()) {
                    open.push(UiRequest {
                        objective_id: wire_id(oid.uuid()),
                        ui_id: id,
                        generation: counter(dialog.generation),
                        prompt: dialog.prompt.clone(),
                    });
                }
            }
        }
        for request in open {
            self.emit(Event::UiRequest(request));
        }
    }

    fn close_dialogs(
        &mut self,
        oid: ObjectiveId,
        keep: impl Fn(u64) -> bool,
        reason: UiCloseReason,
    ) {
        let live = self.live(oid);
        let closed: Vec<String> = live
            .ui
            .iter()
            .filter(|(_, d)| !keep(d.generation))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &closed {
            live.ui.remove(id);
        }
        for id in closed {
            if let Ok(ui_id) = Id::parse(id) {
                self.emit(Event::UiClosed(UiClosed {
                    objective_id: wire_id(oid.uuid()),
                    ui_id,
                    reason,
                }));
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Worker reports.

    pub(crate) fn on_compat_body(
        &mut self,
        oid: ObjectiveId,
        generation: ExecutionGeneration,
        body: WorkerBody,
    ) {
        match body {
            WorkerBody::SessionChanged {
                generation: pi_generation,
                reason,
                pi_session_id,
                session_file,
                model,
                thinking,
                leaf_id,
            } => self.session_changed(
                oid,
                generation,
                PiState {
                    generation: pi_generation.value(),
                    reason,
                    pi_session_id,
                    model,
                    thinking,
                    leaf_id,
                },
                &session_file,
            ),
            WorkerBody::CommandResult {
                command_id,
                ok,
                message,
                entries,
                leaf_id,
                truncated,
            } => self.command_result(
                oid,
                command_id.as_str(),
                ok,
                &message,
                entries,
                leaf_id,
                truncated,
            ),
            WorkerBody::ResourcesLoaded {
                generation: pi_generation,
                resources,
            } => self.resources_loaded(oid, pi_generation.value(), resources),
            WorkerBody::UiRequest {
                ui_id,
                generation: pi_generation,
                prompt,
            } => {
                let current = self.live(oid).pi.as_ref().map(|p| p.generation);
                if current.is_some_and(|g| g != pi_generation.value()) {
                    return; // A late request of a replaced generation is never shown.
                }
                self.live(oid).ui.insert(
                    ui_id.as_str().to_string(),
                    OpenUi {
                        generation: pi_generation.value(),
                        prompt: prompt.clone(),
                    },
                );
                self.emit(Event::UiRequest(UiRequest {
                    objective_id: wire_id(oid.uuid()),
                    ui_id,
                    generation: pi_generation,
                    prompt,
                }));
            }
            WorkerBody::UiClosed { ui_id, reason } => self.ui_closed(oid, ui_id, reason),
            _ => {}
        }
    }

    /// The worker closed a dialog (timeout, abort, replacement); unknown ids are ignored.
    fn ui_closed(&mut self, oid: ObjectiveId, ui_id: Id, reason: UiCloseReason) {
        if self.live(oid).ui.remove(ui_id.as_str()).is_some() {
            self.emit(Event::UiClosed(UiClosed {
                objective_id: wire_id(oid.uuid()),
                ui_id,
                reason,
            }));
        }
    }

    fn session_changed(
        &mut self,
        oid: ObjectiveId,
        generation: ExecutionGeneration,
        pi: PiState,
        session_file: &str,
    ) {
        let reason = pi.reason;
        let pi_generation = pi.generation;
        self.live(oid).pi = Some(pi.clone());
        self.close_dialogs(oid, |g| g >= pi_generation, UiCloseReason::Replaced);
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let now = now_ms();
        if matches!(
            reason,
            SessionChangeReason::Fork | SessionChangeReason::Import
        ) {
            let session = SessionId::new(new_uuid());
            if st
                .objective_mut()
                .rebind_live_session(generation, session, Timestamp::from_unix_ms(now))
                .is_err()
            {
                eprintln!("laded: session replacement refused objective={oid}");
                return;
            }
            let association = st.objective().execution().session().cloned();
            let committed = self.commit(None, None, |tx| {
                Self::save_tx(tx, &st)?;
                if let Some(association) = &association {
                    storage::save_session(tx, oid, association, generation.value(), now)?;
                    storage::record_pi_session(
                        tx,
                        association.session_id,
                        &pi.pi_session_id,
                        session_file,
                    )?;
                }
                Ok(())
            });
            if committed.is_err() {
                eprintln!("laded: could not record session replacement objective={oid}");
                return;
            }
            self.objectives.insert(oid, st);
            self.system_item(
                oid,
                &format!("Session replaced ({})", wire_reason(reason)),
                "The Agent Session now continues in a new pi session. Input and feedback addressed to the previous \
                 session are not delivered to it; no approval, verification or Git state was carried over.",
                false,
            );
            self.emit_objective(oid);
        } else if let Some(session) = st.objective().execution().session().map(|s| s.session_id) {
            let recorded = self.commit(None, None, |tx| {
                storage::record_pi_session(tx, session, &pi.pi_session_id, session_file)
            });
            if recorded.is_err() {
                eprintln!("laded: could not record pi session objective={oid}");
            }
        }
        if reason == SessionChangeReason::Configure
            && let Ok(model) = ModelRef::new(&pi.model.provider, &pi.model.id)
        {
            let _ = self.persist_selection(None, oid, model, pi.thinking, |service| {
                service.session_state(oid, SessionChangeReason::Configure)
            });
        }
        let state = self.session_state(oid, reason);
        self.emit(Event::SessionChanged(state));
    }

    #[allow(clippy::too_many_arguments)]
    fn command_result(
        &mut self,
        oid: ObjectiveId,
        command_id: &str,
        ok: bool,
        message: &str,
        entries: Option<Vec<TreeEntry>>,
        leaf_id: Option<String>,
        truncated: Option<bool>,
    ) {
        let live = self.live(oid);
        if live.command.as_ref().is_none_or(|c| c.id != command_id) {
            return;
        }
        let Some(pending) = live.command.take() else {
            return;
        };
        let result = if ok {
            Ok(match pending.op {
                Op::Tree => ReplyData::SessionTree(SessionTree {
                    objective_id: wire_id(oid.uuid()),
                    entries: entries.unwrap_or_default().into_iter().take(1024).collect(),
                    truncated: truncated.unwrap_or(false),
                    leaf_id,
                }),
                Op::Export => ReplyData::SessionExport(SessionExport {
                    objective_id: wire_id(oid.uuid()),
                    path: pending
                        .export
                        .as_deref()
                        .and_then(|p| WirePath::from_os_str(p.as_os_str()).ok())
                        .unwrap_or_else(|| super::convert::wire_path(b"export")),
                }),
                Op::Navigate => ReplyData::SessionNavigate(
                    self.session_state(oid, SessionChangeReason::Navigate),
                ),
                Op::Fork => {
                    ReplyData::SessionFork(self.session_state(oid, SessionChangeReason::Fork))
                }
                Op::Import => {
                    ReplyData::SessionImport(self.session_state(oid, SessionChangeReason::Import))
                }
                Op::Compact => {
                    ReplyData::SessionCompact(self.session_state(oid, SessionChangeReason::Compact))
                }
                Op::Reload => {
                    ReplyData::SessionReload(self.session_state(oid, SessionChangeReason::Reload))
                }
                Op::Configure => ReplyData::SessionConfigure(
                    self.session_state(oid, SessionChangeReason::Configure),
                ),
            })
        } else {
            Err(rejection(
                ErrorCode::Conflict,
                SafeAction::Refresh,
                &format!("pi refused the session operation: {message}"),
            ))
        };
        self.finish(pending.ctx, result);
        self.dispatch_queued(oid, None);
    }

    fn resources_loaded(
        &mut self,
        oid: ObjectiveId,
        generation: u64,
        resources: Vec<LoadedResource>,
    ) {
        if self.live(oid).reported == Some(generation) {
            return;
        }
        self.live(oid).reported = Some(generation);
        let describe = |kind: LoadedKind| {
            let names: Vec<String> = resources
                .iter()
                .filter(|r| r.kind == kind && r.status == LoadStatus::Loaded)
                .map(|r| match r.source {
                    ResourceSource::LadeProfile => r.name.clone(),
                    ResourceSource::Repository => format!("{} (repository)", r.name),
                })
                .collect();
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        };
        let mut text = format!(
            "Generation {generation}. Context: {}. Skills: {}. Prompt templates: {}. Extensions: {}.",
            describe(LoadedKind::Context),
            describe(LoadedKind::Skill),
            describe(LoadedKind::Prompt),
            describe(LoadedKind::Extension)
        );
        let mut problem = false;
        for refused in resources.iter().filter(|r| r.status != LoadStatus::Loaded) {
            problem = true;
            text.push_str(&format!(
                "\n{} {} was {}: {}",
                match refused.kind {
                    LoadedKind::Extension => "Extension",
                    LoadedKind::Skill => "Skill",
                    LoadedKind::Prompt => "Prompt template",
                    LoadedKind::Context => "Context file",
                },
                refused.name,
                if refused.status == LoadStatus::Refused {
                    "refused"
                } else {
                    "not loaded"
                },
                refused.message.as_deref().unwrap_or("no detail")
            ));
        }
        self.system_item(oid, "Session resources", &text, problem);
    }

    /// The worker of `oid` is gone: its pending command fails and its dialogs close.
    pub(crate) fn compat_worker_gone(&mut self, oid: ObjectiveId) {
        let pending = self.live(oid).command.take();
        if let Some(pending) = pending {
            self.finish(
                pending.ctx,
                Err(rejection(
                    ErrorCode::Interrupted,
                    SafeAction::Refresh,
                    "the worker stopped before the session operation finished",
                )),
            );
        }
        self.close_dialogs(oid, |_| false, UiCloseReason::Aborted);
        self.live(oid).pi = None;
        self.live(oid).reported = None;
    }

    pub(crate) fn compat_timeouts(&mut self) {
        let expired: Vec<ObjectiveId> = self
            .compat
            .live
            .iter()
            .filter(|(_, l)| {
                l.command
                    .as_ref()
                    .is_some_and(|c| c.since.elapsed() > COMMAND_TIMEOUT)
            })
            .map(|(oid, _)| *oid)
            .collect();
        for oid in expired {
            if let Some(pending) = self.live(oid).command.take() {
                self.finish(
                    pending.ctx,
                    Err(rejection(
                        ErrorCode::Interrupted,
                        SafeAction::Refresh,
                        "the session operation did not finish in time",
                    )),
                );
                self.notice(
                    NoticeLevel::Warning,
                    "session_command_timeout",
                    "a session operation timed out",
                );
                self.dispatch_queued(oid, None);
            }
        }
    }

    /// The `worker.start` extras: thinking selection and the session file of the association.
    pub(crate) fn start_extras(&self, oid: ObjectiveId) -> (Option<ThinkingLevel>, Option<String>) {
        let session = self
            .objectives
            .get(&oid)
            .and_then(|st| st.objective().execution().session().map(|s| s.session_id));
        let file = session
            .and_then(|s| {
                storage::pi_session_file(self.store.conn(), s)
                    .ok()
                    .flatten()
            })
            .filter(|f| !f.is_empty());
        (self.thinking_of(oid), file)
    }

    pub(crate) fn record_start_thinking(
        &mut self,
        oid: ObjectiveId,
        thinking: ThinkingLevel,
    ) -> Result<(), Rejection> {
        let now = now_ms();
        sql(storage::save_thinking(
            self.store.conn(),
            oid,
            thinking.as_str(),
            now,
        ))?;
        self.compat.thinking.insert(oid, thinking);
        Ok(())
    }
}

/// The worker's view of a materialised profile snapshot.
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

/// Bounded copy of an import candidate: a regular pi session JSONL whose first line is a session
/// header. The source is opened read-only and never modified.
fn copy_import(source: &Path, copy: &Path) -> Result<(), Rejection> {
    if !source.is_absolute() {
        return Err(invalid("the session file path must be absolute"));
    }
    let meta = fs::metadata(source).map_err(|_| not_found("session file"))?;
    if !meta.is_file() || meta.len() > MAX_IMPORT_BYTES {
        return Err(invalid(
            "the session file must be a regular file of at most 16 MiB",
        ));
    }
    let bytes = fs::read(source).map_err(|_| not_found("readable session file"))?;
    let header = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let is_session = serde_json::from_slice::<Value>(header)
        .ok()
        .is_some_and(|v| v.get("type").and_then(Value::as_str) == Some("session"));
    if !is_session {
        return Err(invalid(
            "not a pi session file (the first line must be a session header)",
        ));
    }
    let dir = copy.parent().ok_or_else(|| invalid("import directory"))?;
    private_dir(dir).map_err(|_| super::convert::durability("import directory unavailable"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(copy)
        .map_err(|_| super::convert::durability("import copy failed"))?;
    file.write_all(&bytes)
        .map_err(|_| super::convert::durability("import copy failed"))
}
