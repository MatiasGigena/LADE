//! Verification runs of a Candidate in an isolated, sandboxed copy, and the disk usage of
//! an agent's writable directories.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lade_core::domain::ids::GitId;
use lade_core::domain::review::VerificationEnd;

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::git::{CommitSpec, Repository};
use crate::adapters::sandbox::{
    Limits, SandboxCgroups, SandboxError, SandboxTools, VERIFICATION_OUTPUT_LIMIT,
    VERIFICATION_TIMEOUT, VerificationLaunch, run_bounded_with, verification_spec,
};
use crate::fault;
use crate::supervision::sweep::{PidRecord, place_sandbox};

pub fn disk_usage(roots: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    let mut pending: Vec<PathBuf> = roots.to_vec();
    while let Some(path) = pending.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        total = total.saturating_add(meta.blocks().saturating_mul(512));
        if meta.is_dir()
            && let Ok(entries) = fs::read_dir(&path)
        {
            pending.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    total
}

pub struct VerifyInput {
    pub repo: Repository,
    pub artifacts: Arc<ArtifactStore>,
    pub tools: Option<SandboxTools>,
    pub copy_dir: PathBuf,
    pub branch: String,
    pub tree: GitId,
    pub base_target: GitId,
    pub argv: Vec<String>,
    pub objective: String,
    pub marker: String,
    pub cgroups: Option<Arc<SandboxCgroups>>,
}

#[derive(Debug)]
pub struct VerifyOut {
    pub end: VerificationEnd,
    pub artifact: Option<String>,
    pub truncated: bool,
    pub tail: String,
}

const TAIL_CHARS: usize = 4096;

pub(super) fn tail_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(TAIL_CHARS))
        .collect()
}

pub fn resolve_program(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return path.is_absolute().then_some(path);
    }
    ["/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| candidate.is_file())
}

pub fn verify(input: &VerifyInput) -> VerifyOut {
    let error = |message: &str| VerifyOut {
        end: VerificationEnd::Error,
        artifact: input
            .artifacts
            .put(message.as_bytes())
            .ok()
            .map(|d| d.to_hex()),
        truncated: false,
        tail: message.to_string(),
    };
    let Some(tools) = &input.tools else {
        return error("verification sandbox unavailable");
    };
    let repo = &input.repo;
    let commit = match repo.commit_tree(&CommitSpec {
        tree: input.tree.clone(),
        parents: vec![input.base_target.clone()],
        message: "LADE verification copy".into(),
        timestamp: Some(0),
    }) {
        Ok(commit) => commit,
        Err(e) => return error(&format!("verification copy failed: {}", e.code())),
    };
    let worktree = match repo.worktree_add(&input.branch, &commit, &input.copy_dir) {
        Ok(worktree) => worktree,
        Err(e) => return error(&format!("verification copy failed: {}", e.code())),
    };
    fault::point("verify.copy");
    let result = run_verification(input, tools);
    let _ = repo.worktree_remove(&worktree, true);
    let _ = fs::remove_dir_all(&input.copy_dir);
    let _ = repo.delete_ref_cas(&format!("refs/heads/{}", input.branch), &commit);
    result.unwrap_or_else(|message| error(&message))
}

fn run_verification(input: &VerifyInput, tools: &SandboxTools) -> Result<VerifyOut, String> {
    let program = input
        .argv
        .first()
        .and_then(|p| resolve_program(p))
        .ok_or_else(|| "verification program not found in /usr/bin or /bin".to_string())?;
    let mut argv: Vec<OsString> = vec![program.into_os_string()];
    argv.extend(input.argv.iter().skip(1).map(OsString::from));
    let built = |e: SandboxError| format!("verification sandbox could not be built: {}", e.code());
    let mut spec = verification_spec(&VerificationLaunch {
        work_dir: input.copy_dir.clone(),
        argv,
        extra_ro: vec![input.repo.common_dir().to_path_buf()],
        env: Vec::new(),
        limits: Limits::VERIFICATION,
    })
    .map_err(built)?;
    spec.marker(&input.marker).map_err(built)?;
    let leaf = match &input.cgroups {
        Some(cgroups) => {
            let name = format!(
                "verify-{}",
                input.branch.rsplit('/').next().unwrap_or("run")
            );
            Some(
                cgroups
                    .leaf(&input.objective, &name)
                    .map_err(|e| format!("verification cgroup: {}", e.kind()))?,
            )
        }
        None => None,
    };
    let outcome = run_bounded_with(
        spec.command(tools).map_err(built)?,
        VERIFICATION_TIMEOUT,
        VERIFICATION_OUTPUT_LIMIT,
        |pid| match &leaf {
            Some(leaf) => place_sandbox(pid, leaf, || PidRecord::of(pid).is_none()),
            None => Ok(()),
        },
    )
    .map_err(|e| format!("verification could not start: {}", e.kind()))?;
    drop(leaf);
    let end = match (outcome.timed_out, outcome.exit_code) {
        (false, Some(code)) => VerificationEnd::Exited { code },
        (true, _) | (false, None) => VerificationEnd::Interrupted,
    };
    Ok(VerifyOut {
        end,
        artifact: input
            .artifacts
            .put(&outcome.output_tail)
            .ok()
            .map(|d| d.to_hex()),
        truncated: outcome.truncated,
        tail: tail_text(&outcome.output_tail),
    })
}
