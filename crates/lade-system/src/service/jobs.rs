use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use lade_core::domain::ids::{CaptureId, DraftId, GitId};
use lade_core::domain::inspection::{CurrentContent, Reference, Side};
use lade_core::domain::review::{ChangeCounts, VerificationEnd};
use lade_protocol::client::Rejection;
use lade_protocol::types::WirePath;
use sha2::{Digest as _, Sha256};

use crate::adapters::artifacts::{ArtifactDigest, ArtifactStore};
use crate::adapters::git::{
    self, ChangedFile, CommitSpec, GitError, MergeOutcome, Repository, SquashOutcome, SquashRequest,
};
use crate::adapters::sandbox::{
    Limits, SandboxCgroups, SandboxTools, VERIFICATION_OUTPUT_LIMIT, VERIFICATION_TIMEOUT,
    VerificationLaunch, run_bounded_with, verification_spec,
};
use crate::supervision::sweep::place_sandbox;
use crate::supervision::{
    LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerLauncher,
};

use super::convert::{adapter_git, conflict, core_git, git_rejection, hex, not_found};

pub const MAX_WHOLE_FILE: u64 = 64 << 20;
pub const MAX_CHUNKED_FILE: u64 = 1 << 30;
const LARGE_BLOB_MEMORY: usize = 8;

pub(crate) type Task = Box<dyn FnOnce() + Send>;

pub struct Pool {
    tx: Option<Sender<Task>>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    pub fn new(size: usize) -> Self {
        let (tx, rx) = mpsc::channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));
        let threads = (0..size)
            .map(|index| {
                let rx: Arc<Mutex<Receiver<Task>>> = Arc::clone(&rx);
                thread::Builder::new()
                    .name(format!("laded-pool-{index}"))
                    .spawn(move || {
                        loop {
                            let task = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                            match task {
                                Ok(task) => task(),
                                Err(_) => break,
                            }
                        }
                    })
                    .expect("spawn pool thread")
            })
            .collect();
        Self {
            tx: Some(tx),
            threads,
        }
    }

    pub fn submit(&self, task: impl FnOnce() + Send + 'static) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Box::new(task));
        }
    }

    pub fn shutdown(mut self) {
        self.tx = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn git_err(error: &GitError) -> Rejection {
    git_rejection(error)
}

pub fn open_repository(path: &Path) -> Result<Repository, GitError> {
    Repository::open(path).map(|repo| repo.with_excludes_file(git::user_excludes_file()))
}

pub fn target_commit(repo: &Repository, branch: &str) -> Result<GitId, Rejection> {
    repo.resolve_ref(&format!("refs/heads/{branch}"))
        .map_err(|e| git_err(&e))?
        .map(|id| core_git(&id))
        .ok_or_else(|| not_found("integration target branch"))
}

pub fn counts(files: &[ChangedFile]) -> ChangeCounts {
    ChangeCounts {
        files: u32::try_from(files.len()).unwrap_or(u32::MAX),
        added: files.iter().map(|f| f.added).sum(),
        removed: files.iter().map(|f| f.removed).sum(),
    }
}

pub fn read_whole(
    repo: &Repository,
    tree: &GitId,
    path: &git::RepoPath,
) -> Result<Option<Vec<u8>>, Rejection> {
    let tree = adapter_git(tree);
    let Some(entry) = repo.tree_entry(&tree, path).map_err(|e| git_err(&e))? else {
        return Ok(None);
    };
    if entry.kind != git::ObjectKind::Blob {
        return Err(Rejection::new(
            lade_protocol::client::ErrorCode::UnsupportedContent,
            lade_protocol::client::SafeAction::None,
            "path is not a regular file",
        ));
    }
    match repo
        .read_blob_capped(&entry.id, MAX_WHOLE_FILE)
        .map_err(|e| git_err(&e))?
    {
        Some(bytes) => Ok(Some(bytes)),
        None => Err(Rejection::new(
            lade_protocol::client::ErrorCode::UnsupportedContent,
            lade_protocol::client::SafeAction::None,
            "file exceeds the 64 MiB inspection bound",
        )),
    }
}

fn unsupported(detail: &str) -> Rejection {
    Rejection::new(
        lade_protocol::client::ErrorCode::UnsupportedContent,
        lade_protocol::client::SafeAction::None,
        detail,
    )
}

pub enum FileBytes {
    Whole(Vec<u8>),
    Large { blob: git::GitId },
}

pub fn read_file_bytes(
    repo: &Repository,
    tree: &GitId,
    path: &git::RepoPath,
) -> Result<Option<FileBytes>, Rejection> {
    let Some(entry) = repo
        .tree_entry(&adapter_git(tree), path)
        .map_err(|e| git_err(&e))?
    else {
        return Ok(None);
    };
    match (entry.kind, entry.size) {
        (git::ObjectKind::Blob, Some(size)) if size > MAX_CHUNKED_FILE => {
            Err(unsupported("file exceeds the 1 GiB inspection bound"))
        }
        (git::ObjectKind::Blob, Some(size)) if size > MAX_WHOLE_FILE => {
            Ok(Some(FileBytes::Large { blob: entry.id }))
        }
        _ => read_whole(repo, tree, path).map(|bytes| bytes.map(FileBytes::Whole)),
    }
}

#[derive(Default)]
pub struct LargeBlobs(Mutex<VecDeque<(git::GitId, ArtifactDigest)>>);

impl LargeBlobs {
    pub fn retain(
        &self,
        repo: &Repository,
        artifacts: &ArtifactStore,
        id: &git::GitId,
    ) -> Result<ArtifactDigest, Rejection> {
        let remembered = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(blob, _)| blob == id)
            .map(|(_, digest)| *digest);
        if let Some(digest) = remembered.filter(|d| artifacts.contains(d)) {
            return Ok(digest);
        }
        let digest = repo
            .with_blob_reader(id, |reader| artifacts.put_reader(reader))
            .map_err(|e| git_err(&e))?
            .map_err(|_| super::convert::durability("content could not be retained"))?;
        let mut memory = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        memory.retain(|(blob, _)| blob != id);
        memory.push_back((id.clone(), digest));
        if memory.len() > LARGE_BLOB_MEMORY {
            memory.pop_front();
        }
        Ok(digest)
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn reference_stale(repo: &Repository, old: &GitId, new: &GitId, reference: &Reference) -> bool {
    let Ok(path) = git::RepoPath::new(reference.path().as_bytes().to_vec()) else {
        return true;
    };
    let tree = match reference.side() {
        Side::Old => old,
        Side::New => new,
    };
    match read_file_bytes(repo, tree, &path) {
        Ok(None) => reference.staleness(CurrentContent::Missing).is_stale(),
        Ok(Some(FileBytes::Large { blob })) => {
            let digest = repo.with_blob_reader(&blob, |reader| {
                let mut hasher = Sha256::new();
                let mut buf = vec![0u8; 64 << 10];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break Ok(hex(&hasher.finalize())),
                        Ok(n) => hasher.update(&buf[..n]),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => break Err(e),
                    }
                }
            });
            if matches!(digest, Ok(Ok(ref d)) if *d == reference.artifact_digest().hex()) {
                return false;
            }
            let range = reference.byte_range();
            let Ok(len) = usize::try_from(range.byte_len()) else {
                return true;
            };
            repo.read_blob(&adapter_git(tree), &path, range.start(), len)
                .map_or(true, |page| {
                    page.offset != range.start()
                        || page.data != reference.selected_text().as_bytes()
                })
        }
        Ok(Some(FileBytes::Whole(bytes))) => {
            let Ok(digest) =
                lade_core::domain::inspection::ArtifactDigest::parse(&sha256_hex(&bytes))
            else {
                return true;
            };
            reference
                .staleness(CurrentContent::Present {
                    digest: &digest,
                    bytes: &bytes,
                })
                .is_stale()
        }
        Err(_) => true,
    }
}

pub fn manifest(target: &GitId, tree: &GitId, files: &[ChangedFile]) -> Vec<u8> {
    let mut out = format!(
        "lade-manifest 1\ntarget {}\ntree {}\n",
        target.hex(),
        tree.hex()
    );
    for file in files {
        let id = |id: &Option<git::GitId>| {
            id.as_ref()
                .map_or_else(|| "-".to_string(), |i| i.hex().to_string())
        };
        let path = WirePath::from_bytes(file.path.as_bytes().to_vec())
            .map_or_else(|_| "?".into(), |p| p.b64());
        let old = file
            .old_path
            .as_ref()
            .and_then(|p| WirePath::from_bytes(p.as_bytes().to_vec()).ok())
            .map_or_else(|| "-".to_string(), |p| p.b64());
        out.push_str(&format!(
            "{:?}\t{:o}\t{:o}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            file.status,
            file.old_mode,
            file.new_mode,
            id(&file.old_id),
            id(&file.new_id),
            file.added,
            file.removed,
            old,
            path
        ));
    }
    out.into_bytes()
}

pub struct CaptureInput {
    pub repo: Repository,
    pub worktree: PathBuf,
    pub scratch: PathBuf,
    pub target_branch: String,
    pub capture_id: CaptureId,
    pub latest: Option<(CaptureId, GitId)>,
    pub drafts: Vec<(DraftId, Reference)>,
}

#[derive(Debug)]
pub struct CaptureOut {
    pub capture_id: CaptureId,
    pub reused: bool,
    pub tree: GitId,
    pub target: GitId,
    pub files: Vec<ChangedFile>,
    pub stale: Vec<(DraftId, bool)>,
    pub skipped_nested: Vec<git::RepoPath>,
    pub filtered: Vec<git::RepoPath>,
}

pub fn capture(input: &CaptureInput) -> Result<CaptureOut, Rejection> {
    let repo = &input.repo;
    let worktree = repo
        .open_worktree(&input.worktree)
        .map_err(|e| git_err(&e))?;
    let captured = repo
        .capture_working_tree(&worktree, &input.scratch)
        .map_err(|e| git_err(&e))?;
    let tree = core_git(&captured.tree);
    let target = target_commit(repo, &input.target_branch)?;
    let files = repo
        .diff_numstat(&adapter_git(&target), &captured.tree)
        .map_err(|e| git_err(&e))?;
    let filtered = filtered_paths(repo, &captured.tree, &files)?;
    let (capture_id, reused) = match &input.latest {
        Some((id, latest)) if *latest == tree => (*id, true),
        _ => {
            repo.retain(
                &format!("capture-{}", input.capture_id),
                &adapter_git(&tree),
            )
            .map_err(|e| git_err(&e))?;
            crate::fault::point("capture.retained");
            (input.capture_id, false)
        }
    };
    let stale = input
        .drafts
        .iter()
        .map(|(id, reference)| (*id, reference_stale(repo, &target, &tree, reference)))
        .collect();
    Ok(CaptureOut {
        capture_id,
        reused,
        tree,
        target,
        files,
        stale,
        skipped_nested: captured.skipped_nested,
        filtered,
    })
}

pub fn filtered_paths(
    repo: &Repository,
    tree: &git::GitId,
    files: &[ChangedFile],
) -> Result<Vec<git::RepoPath>, Rejection> {
    let paths: Vec<&git::RepoPath> = files.iter().map(|f| &f.path).collect();
    repo.filtered_paths(tree, &paths).map_err(|e| git_err(&e))
}

pub struct FreezeInput {
    pub repo: Repository,
    pub artifacts: Arc<ArtifactStore>,
    pub worktree: PathBuf,
    pub scratch: PathBuf,
    pub target_branch: String,
    pub start_commit: GitId,
    pub git_branch: String,
    pub candidate_ref: String,
    pub message: String,
}

#[derive(Debug)]
pub struct FreezeOut {
    pub commit: GitId,
    pub tree: GitId,
    pub target: GitId,
    pub counts: ChangeCounts,
    pub manifest_digest: String,
    pub changed: bool,
}

pub fn freeze(input: &FreezeInput) -> Result<FreezeOut, Rejection> {
    let repo = &input.repo;
    let worktree = repo
        .open_worktree(&input.worktree)
        .map_err(|e| git_err(&e))?;
    let captured = repo
        .capture_working_tree(&worktree, &input.scratch)
        .map_err(|e| git_err(&e))?
        .tree;
    let target = target_commit(repo, &input.target_branch)?;
    let commit = repo
        .commit_tree(&CommitSpec {
            tree: captured.clone(),
            parents: vec![adapter_git(&input.start_commit)],
            message: input.message.clone(),
            timestamp: None,
        })
        .map_err(|e| git_err(&e))?;
    let tree = if input.start_commit == target {
        captured.clone()
    } else {
        match repo
            .merge_tree(&adapter_git(&target), &commit)
            .map_err(|e| git_err(&e))?
        {
            MergeOutcome::Clean { tree } => tree,
            MergeOutcome::Conflict { .. } => {
                return Err(conflict(
                    "the work conflicts with the moved integration target; continue the agent work first",
                ));
            }
        }
    };
    let files = repo
        .diff_numstat(&adapter_git(&target), &tree)
        .map_err(|e| git_err(&e))?;
    if files.is_empty() {
        return Err(super::convert::domain_rejection(
            &lade_core::DomainError::InvalidLifecycle(super::project::NO_CHANGES_TO_FREEZE),
        ));
    }
    let digest = input
        .artifacts
        .put(&manifest(&target, &core_git(&tree), &files))
        .map_err(|_| super::convert::durability("manifest artifact could not be stored"))?;
    let branch_ref = format!("refs/heads/{}", input.git_branch);
    let old = repo.resolve_ref(&branch_ref).map_err(|e| git_err(&e))?;
    repo.update_ref_cas(&branch_ref, &commit, old.as_ref())
        .map_err(|e| git_err(&e))?;
    crate::fault::point("freeze.branch_moved");
    repo.retain(&input.candidate_ref, &commit)
        .map_err(|e| git_err(&e))?;
    if let Err(e) = repo.sync_worktree_index(&worktree, &input.git_branch, &commit) {
        eprintln!(
            "laded: worktree index not refreshed after freeze: {}",
            e.code()
        );
    }
    if tree != captured {
        repo.retain(&format!("{}-tree", input.candidate_ref), &tree)
            .map_err(|e| git_err(&e))?;
    }
    let again = repo
        .capture_working_tree(&worktree, &input.scratch)
        .map_err(|e| git_err(&e))?
        .tree;
    Ok(FreezeOut {
        commit: core_git(&commit),
        tree: core_git(&tree),
        target,
        counts: counts(&files),
        manifest_digest: digest.to_hex(),
        changed: again != captured,
    })
}

pub struct RefreshInput {
    pub repo: Repository,
    pub artifacts: Arc<ArtifactStore>,
    pub target_branch: String,
    pub candidate_commit: GitId,
    pub retain_ref: String,
}

#[derive(Debug)]
pub enum RefreshOut {
    Rebuilt {
        tree: GitId,
        target: GitId,
        counts: ChangeCounts,
        manifest_digest: String,
    },
    Conflict {
        target: GitId,
    },
}

pub fn refresh(input: &RefreshInput) -> Result<RefreshOut, Rejection> {
    let repo = &input.repo;
    let target = target_commit(repo, &input.target_branch)?;
    match repo
        .merge_tree(&adapter_git(&target), &adapter_git(&input.candidate_commit))
        .map_err(|e| git_err(&e))?
    {
        MergeOutcome::Conflict { .. } => Ok(RefreshOut::Conflict { target }),
        MergeOutcome::Clean { tree } => {
            let files = repo
                .diff_numstat(&adapter_git(&target), &tree)
                .map_err(|e| git_err(&e))?;
            if files.is_empty() {
                return Err(conflict(
                    "the moved target already contains this change set; complete the objective instead",
                ));
            }
            let digest = input
                .artifacts
                .put(&manifest(&target, &core_git(&tree), &files))
                .map_err(|_| super::convert::durability("manifest artifact could not be stored"))?;
            repo.retain(&input.retain_ref, &tree)
                .map_err(|e| git_err(&e))?;
            Ok(RefreshOut::Rebuilt {
                tree: core_git(&tree),
                target,
                counts: counts(&files),
                manifest_digest: digest.to_hex(),
            })
        }
    }
}

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

fn tail_text(bytes: &[u8]) -> String {
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

pub fn verify(input: VerifyInput) -> VerifyOut {
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
        tree: adapter_git(&input.tree),
        parents: vec![adapter_git(&input.base_target)],
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
    crate::fault::point("verify.copy");
    let result = run_verification(&input, tools);
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
    let built = |e: crate::adapters::sandbox::SandboxError| {
        format!("verification sandbox could not be built: {}", e.code())
    };
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
            Some(leaf) => place_sandbox(pid, leaf, || {
                crate::supervision::sweep::PidRecord::of(pid).is_none()
            }),
            None => Ok(()),
        },
    )
    .map_err(|e| format!("verification could not start: {}", e.kind()))?;
    drop(leaf);
    let end = match (outcome.timed_out, outcome.exit_code) {
        (true, _) => VerificationEnd::Interrupted,
        (false, Some(code)) => VerificationEnd::Exited { code },
        (false, None) => VerificationEnd::Interrupted,
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

pub fn integrate(repo: &Repository, request: &SquashRequest) -> Result<SquashOutcome, GitError> {
    repo.integrate_squash(request)
}

pub fn change_set_empty(
    repo: &Repository,
    worktree: Option<&Path>,
    scratch: &Path,
    target_branch: &str,
    start_commit: &GitId,
) -> Result<bool, Rejection> {
    let target = target_commit(repo, target_branch)?;
    let tree = match worktree {
        Some(path) => {
            let wt = repo.open_worktree(path).map_err(|e| git_err(&e))?;
            repo.capture_working_tree(&wt, scratch)
                .map_err(|e| git_err(&e))?
                .tree
        }
        None => adapter_git(start_commit),
    };
    let files = repo
        .diff_numstat(&adapter_git(&target), &tree)
        .map_err(|e| git_err(&e))?;
    Ok(files.is_empty())
}

pub fn cleanup(
    repo: &Repository,
    worktree: Option<&Path>,
    git_branch: Option<&str>,
    scratch: &Path,
    force: bool,
) -> Result<(), Rejection> {
    if let Some(path) = worktree {
        match repo.open_worktree(path) {
            Ok(wt) => {
                let mut force = force;
                if !force && let Some(branch) = git_branch {
                    let head = repo
                        .resolve_ref(&format!("refs/heads/{branch}"))
                        .map_err(|e| git_err(&e))?;
                    let tree = repo
                        .capture_working_tree(&wt, scratch)
                        .map_err(|e| git_err(&e))?
                        .tree;
                    if let Some(head) = head {
                        force = repo
                            .diff_numstat(&head, &tree)
                            .map_err(|e| git_err(&e))?
                            .is_empty();
                    }
                }
                match repo.worktree_remove(&wt, force) {
                    Ok(()) => {}
                    Err(GitError::WorktreeDirty) => {
                        return Err(conflict(
                            "worktree has changes not in any candidate; confirm data loss to remove it",
                        ));
                    }
                    Err(e) => return Err(git_err(&e)),
                }
            }
            Err(GitError::WorktreeNotFound) => {
                let _ = fs::remove_dir_all(path);
            }
            Err(e) => return Err(git_err(&e)),
        }
    }
    if let Some(branch) = git_branch {
        let refname = format!("refs/heads/{branch}");
        if let Some(current) = repo.resolve_ref(&refname).map_err(|e| git_err(&e))? {
            repo.delete_ref_cas(&refname, &current)
                .map_err(|e| git_err(&e))?;
        }
    }
    Ok(())
}

pub fn delete_refs(repo: &Repository, refs: &[String]) -> Result<(), Rejection> {
    for refname in refs {
        if let Some(current) = repo.resolve_ref(refname).map_err(|e| git_err(&e))? {
            repo.delete_ref_cas(refname, &current)
                .map_err(|e| git_err(&e))?;
        }
    }
    Ok(())
}

pub struct LaunchInput {
    pub repo: Repository,
    pub launcher: Arc<dyn WorkerLauncher>,
    pub events: mpsc::SyncSender<SupervisorEvent>,
    pub request: LaunchRequest,
    pub provision: Option<(String, GitId)>,
    pub profile: Option<(PathBuf, Vec<(String, String)>)>,
}

pub struct LaunchOut {
    pub provisioned_branch: Option<String>,
    pub result: Result<Box<dyn WorkerControl>, String>,
}

fn launch_reason(error: &LaunchError) -> String {
    match error {
        LaunchError::SandboxUnavailable(_) => "sandbox_unavailable".into(),
        LaunchError::ProviderUnavailable(_) => "provider_unavailable".into(),
        LaunchError::Io(e) => format!("launch_io: {}", e.kind()),
    }
}

pub fn launch(input: LaunchInput) -> LaunchOut {
    let mut provisioned_branch = None;
    let request = input.request;
    if let Some((base, start)) = &input.provision {
        match provision(&input.repo, base, start, &request.worktree) {
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
            result: Err("worktree_missing".into()),
        };
    }
    if let Err(e) = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&request.session_dir)
    {
        return LaunchOut {
            provisioned_branch,
            result: Err(format!("session_dir: {}", e.kind())),
        };
    }
    let mut request = request;
    if let Some((profile, approved)) = &input.profile {
        let dest = request.session_dir.join("profile");
        match super::compat::snapshot(profile, &dest, approved) {
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
                    result: Err(format!("profile_snapshot: {}", e.kind())),
                };
            }
        }
    }
    let result = input
        .launcher
        .launch(request, input.events)
        .map_err(|e| launch_reason(&e));
    LaunchOut {
        provisioned_branch,
        result,
    }
}

fn provision(repo: &Repository, base: &str, start: &GitId, path: &Path) -> Result<String, String> {
    if path.exists() {
        return adopt_provisioned(repo, base, start, path)
            .ok_or_else(|| "worktree_path_occupied".into());
    }
    let mut branch = base.to_string();
    for attempt in 2..=32 {
        match repo.resolve_ref(&format!("refs/heads/{branch}")) {
            Ok(None) => break,
            Ok(Some(_)) => branch = format!("{base}-{attempt}"),
            Err(e) => return Err(format!("provision: {}", e.code())),
        }
    }
    repo.worktree_add(&branch, &adapter_git(start), path)
        .map_err(|e| format!("provision: {}", e.code()))?;
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
