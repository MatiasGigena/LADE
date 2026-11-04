//! Launching a worker: provisioning its worktree and branch, the session directory, the
//! approved profile snapshot, and the launcher call itself.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use lade_core::domain::ids::GitId;

use crate::adapters::git::Repository;
use crate::supervision::{
    LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerLauncher,
};

use crate::service::convert::{adapter_git, core_git};
use crate::support::fs::ensure_private_dir;

pub struct LaunchInput {
    pub repo: Repository,
    pub launcher: Arc<dyn WorkerLauncher>,
    pub events: mpsc::SyncSender<SupervisorEvent>,
    pub request: LaunchRequest,
    /// Set when the Objective has no worktree yet: create one on a fresh branch.
    pub provision: Option<Provision>,
    pub profile: Option<ProfileSnapshot>,
}

pub struct Provision {
    /// The branch name to start from; a numbered suffix is added if it is taken.
    pub base_branch: String,
    pub start: GitId,
}

/// The LADE profile to snapshot into the session, holding only the approved extensions.
pub struct ProfileSnapshot {
    pub source: PathBuf,
    pub approved: Vec<(String, String)>,
}

pub struct LaunchOut {
    pub provisioned_branch: Option<String>,
    pub result: Result<Box<dyn WorkerControl>, LaunchFailure>,
}

#[derive(Debug)]
pub enum LaunchFailure {
    Provision(&'static str),
    WorktreeOccupied,
    WorktreeMissing,
    SessionDir(io::ErrorKind),
    ProfileSnapshot(io::ErrorKind),
    Worker(LaunchError),
}

impl fmt::Display for LaunchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provision(code) => write!(f, "provision: {code}"),
            Self::WorktreeOccupied => f.write_str("worktree_path_occupied"),
            Self::WorktreeMissing => f.write_str("worktree_missing"),
            Self::SessionDir(kind) => write!(f, "session_dir: {kind}"),
            Self::ProfileSnapshot(kind) => write!(f, "profile_snapshot: {kind}"),
            Self::Worker(LaunchError::SandboxUnavailable(_)) => f.write_str("sandbox_unavailable"),
            Self::Worker(LaunchError::ProviderUnavailable(_)) => {
                f.write_str("provider_unavailable")
            }
            Self::Worker(LaunchError::Io(e)) => write!(f, "launch_io: {}", e.kind()),
        }
    }
}

pub fn launch(input: LaunchInput) -> LaunchOut {
    let mut provisioned_branch = None;
    let request = input.request;
    if let Some(wanted) = &input.provision {
        match provision(&input.repo, wanted, &request.worktree) {
            Ok(branch) => {
                crate::fault::point("launch.provisioned");
                provisioned_branch = Some(branch);
            }
            Err(reason) => {
                return LaunchOut {
                    provisioned_branch: None,
                    result: Err(reason),
                };
            }
        }
    }
    if !request.worktree.is_dir() {
        return LaunchOut {
            provisioned_branch,
            result: Err(LaunchFailure::WorktreeMissing),
        };
    }
    if let Err(e) = ensure_private_dir(&request.session_dir) {
        return LaunchOut {
            provisioned_branch,
            result: Err(LaunchFailure::SessionDir(e.kind())),
        };
    }
    let mut request = request;
    if let Some(profile) = &input.profile {
        let dest = request.session_dir.join("profile");
        match crate::service::resources::snapshot(&profile.source, &dest, &profile.approved) {
            Ok(snapshot) => {
                if let lade_protocol::worker::WorkerBody::Start { resources, .. } =
                    &mut request.start
                {
                    *resources = Some(snapshot);
                }
            }
            Err(e) => {
                return LaunchOut {
                    provisioned_branch,
                    result: Err(LaunchFailure::ProfileSnapshot(e.kind())),
                };
            }
        }
    }
    let result = input
        .launcher
        .launch(request, input.events)
        .map_err(LaunchFailure::Worker);
    LaunchOut {
        provisioned_branch,
        result,
    }
}

fn provision(repo: &Repository, wanted: &Provision, path: &Path) -> Result<String, LaunchFailure> {
    let (base, start) = (wanted.base_branch.as_str(), &wanted.start);
    if path.exists() {
        return adopt_provisioned(repo, base, start, path).ok_or(LaunchFailure::WorktreeOccupied);
    }
    let mut branch = base.to_string();
    for attempt in 2..=32 {
        match repo.resolve_ref(&format!("refs/heads/{branch}")) {
            Ok(None) => break,
            Ok(Some(_)) => branch = format!("{base}-{attempt}"),
            Err(e) => return Err(LaunchFailure::Provision(e.code())),
        }
    }
    repo.worktree_add(&branch, &adapter_git(start), path)
        .map_err(|e| LaunchFailure::Provision(e.code()))?;
    Ok(branch)
}

fn adopt_provisioned(repo: &Repository, base: &str, start: &GitId, path: &Path) -> Option<String> {
    let wanted = path.canonicalize().ok()?;
    let entry = repo
        .worktree_entries()
        .ok()?
        .into_iter()
        .find(|e| !e.prunable && e.path.canonicalize().ok().as_ref() == Some(&wanted))?;
    let branch = String::from_utf8(entry.branch?)
        .ok()?
        .strip_prefix("refs/heads/")?
        .to_string();
    let ours = branch == base
        || branch
            .strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|n| n.parse::<u8>().is_ok());
    let at_start = repo
        .resolve_ref(&format!("refs/heads/{branch}"))
        .ok()
        .flatten()
        .is_some_and(|id| core_git(&id) == *start);
    (ours && at_start).then(|| {
        eprintln!("laded: adopted an unrecorded provisioned worktree");
        branch
    })
}
