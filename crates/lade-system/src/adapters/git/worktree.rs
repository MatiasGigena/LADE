//! Objective worktrees. Operations on a worktree always pass its administrative directory
//! (`<common>/worktrees/<name>`) explicitly: the `.git` file inside a worktree is writable by the
//! sandboxed worker and is never used for discovery.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::command::GitCommand;
use super::{GitError, GitId, Repository, Result, validate_branch_name};

/// A worktree and its trusted administrative Git directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub(crate) path: PathBuf,
    pub(crate) admin_dir: PathBuf,
}

impl Worktree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The per-worktree Git directory (the common directory for the main worktree).
    pub fn admin_dir(&self) -> &Path {
        &self.admin_dir
    }

    pub(crate) fn git(&self, op: &'static str) -> GitCommand {
        GitCommand::new(op)
            .git_dir(&self.admin_dir)
            .work_tree(&self.path)
    }
}

/// One record of `git worktree list --porcelain -z`.
#[derive(Debug)]
pub(crate) struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<Vec<u8>>,
    pub bare: bool,
    pub prunable: bool,
}

impl Repository {
    /// Create `branch` at `start` and check it out in a new worktree at `path` (absolute; must not
    /// exist or be an empty directory). Post-checkout hooks and smudge filters never run.
    pub fn worktree_add(&self, branch: &str, start: &GitId, path: &Path) -> Result<Worktree> {
        validate_branch_name(branch)?;
        if start.format() != self.object_format() {
            return Err(GitError::InvalidId);
        }
        if !path.is_absolute() {
            return Err(GitError::InvalidArgument {
                what: "worktree path",
            });
        }
        self.git("worktree-add")
            .args(["worktree", "add", "--quiet", "--no-track", "-b", branch])
            .arg(path)
            .arg(start.hex())
            .run()?;
        self.open_worktree(path)
    }

    /// Identify an existing worktree of this repository by path, resolving its administrative
    /// directory from the common directory (never from the worktree's own `.git` file).
    pub fn open_worktree(&self, path: &Path) -> Result<Worktree> {
        let path = path
            .canonicalize()
            .map_err(|_| GitError::WorktreeNotFound)?;
        let main = self
            .worktree_entries()?
            .into_iter()
            .next()
            .filter(|e| !e.bare)
            .and_then(|e| e.path.canonicalize().ok());
        if main.as_deref() == Some(path.as_path()) {
            return Ok(Worktree {
                path,
                admin_dir: self.common_dir().to_path_buf(),
            });
        }
        let admin_dir = self
            .linked_admin_dir(&path)
            .ok_or(GitError::WorktreeNotFound)?;
        Ok(Worktree { path, admin_dir })
    }

    /// Scan `<common>/worktrees/*/gitdir` for the record pointing at `<path>/.git`.
    pub(crate) fn linked_admin_dir(&self, path: &Path) -> Option<PathBuf> {
        let entries = fs::read_dir(self.common_dir().join("worktrees")).ok()?;
        entries.flatten().map(|e| e.path()).find(|admin| {
            fs::read(admin.join("gitdir")).ok().is_some_and(|content| {
                let target = PathBuf::from(OsString::from_vec(
                    content.strip_suffix(b"\n").unwrap_or(&content).to_vec(),
                ));
                target.file_name().is_some_and(|n| n == ".git")
                    && target
                        .parent()
                        .and_then(|p| p.canonicalize().ok())
                        .is_some_and(|p| p == path)
            })
        })
    }

    /// Remove a linked worktree. Without `force`, modified or untracked content refuses with
    /// [`GitError::WorktreeDirty`]. The worktree's `.git` file is first restored to point at its
    /// trusted administrative directory so Git's internal status check cannot be redirected to
    /// worker-controlled configuration.
    pub fn worktree_remove(&self, worktree: &Worktree, force: bool) -> Result<()> {
        if worktree.admin_dir() == self.common_dir() {
            return Err(GitError::InvalidArgument {
                what: "main worktree",
            });
        }
        // A forced removal deletes the directory directly instead of through `git worktree remove`:
        // that command runs `status` inside any nested repository the directory holds, and a
        // sandboxed worker can plant one whose configuration names a filter/hook program, which
        // Git would then execute on the host (the per-repository hardening does not reach a nested
        // repository's own config). The administrative entry is then pruned.
        if force {
            match fs::remove_dir_all(worktree.path()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(GitError::Io {
                        op: "worktree-remove",
                        kind: e.kind(),
                    });
                }
            }
            self.git("worktree-prune")
                .args(["worktree", "prune"])
                .run()?;
            return Ok(());
        }
        if !worktree.path().exists() {
            self.git("worktree-prune")
                .args(["worktree", "prune"])
                .run()?;
            return Ok(());
        }
        // The non-force path keeps Git's own dirty detection (callers that want it accept that it
        // inspects worktree content; production removals are forced and never reach here).
        restore_gitfile(worktree)?;
        let out = self
            .git("worktree-remove")
            .args(["worktree", "remove"])
            .arg(worktree.path())
            .output()?;
        if out.status.success() {
            Ok(())
        } else if out.stderr.contains("modified or untracked files") {
            Err(GitError::WorktreeDirty)
        } else {
            Err(GitError::Failed {
                op: "worktree-remove",
                exit_code: out.status.code(),
                stderr: out.stderr,
            })
        }
    }

    /// Make a linked worktree's own Git view agree with `commit`, whose tree equals the files
    /// already on disk (a frozen candidate): `HEAD` is re-pointed at `refs/heads/<branch>` (which
    /// must already hold `commit`), the worktree's index is replaced with `commit`'s tree without
    /// touching files (`read-tree`, no `-u`), and stat information is refreshed
    /// (`update-index -q --refresh`), so `git status` in the worktree reports the candidate as
    /// committed. Filters and merge drivers from the worktree's configuration are disabled.
    pub fn sync_worktree_index(
        &self,
        worktree: &Worktree,
        branch: &str,
        commit: &GitId,
    ) -> Result<()> {
        validate_branch_name(branch)?;
        if worktree.admin_dir() == self.common_dir() {
            return Err(GitError::InvalidArgument {
                what: "main worktree",
            });
        }
        let refname = format!("refs/heads/{branch}");
        let current = self.resolve_ref(&refname)?;
        if current.as_ref() != Some(commit) {
            return Err(GitError::StaleRef { refname, current });
        }
        restore_gitfile(worktree)?;
        worktree
            .git("symbolic-ref")
            .args(["symbolic-ref", "HEAD"])
            .arg(&refname)
            .run()?;
        worktree
            .git("read-tree")
            .args(["read-tree", commit.hex()])
            .run()?;
        // Exit status 1 only reports paths that still differ; the index itself is updated.
        let out = worktree
            .git("update-index")
            .args(["update-index", "-q", "--refresh"])
            .output()?;
        match out.status.code() {
            Some(0 | 1) => Ok(()),
            code => Err(GitError::Failed {
                op: "update-index",
                exit_code: code,
                stderr: out.stderr,
            }),
        }
    }

    pub(crate) fn worktree_entries(&self) -> Result<Vec<WorktreeEntry>> {
        let out = self
            .git("worktree-list")
            .read_only()
            .args(["worktree", "list", "--porcelain", "-z"])
            .run()?;
        let mut entries = Vec::new();
        let mut current: Option<WorktreeEntry> = None;
        for field in out.split(|b| *b == 0) {
            if field.is_empty() {
                entries.extend(current.take());
                continue;
            }
            if let Some(path) = field.strip_prefix(b"worktree ") {
                entries.extend(current.take());
                current = Some(WorktreeEntry {
                    path: PathBuf::from(OsString::from_vec(path.to_vec())),
                    branch: None,
                    bare: false,
                    prunable: false,
                });
                continue;
            }
            let Some(entry) = current.as_mut() else {
                return Err(GitError::Parse {
                    op: "worktree-list",
                    what: "record",
                });
            };
            if let Some(branch) = field.strip_prefix(b"branch ") {
                entry.branch = Some(branch.to_vec());
            } else if field == b"bare" {
                entry.bare = true;
            } else if field == b"prunable" || field.starts_with(b"prunable ") {
                entry.prunable = true;
            }
        }
        entries.extend(current);
        Ok(entries)
    }
}

fn restore_gitfile(worktree: &Worktree) -> Result<()> {
    let gitfile = worktree.path().join(".git");
    let mut expected = b"gitdir: ".to_vec();
    expected.extend_from_slice(worktree.admin_dir().as_os_str().as_encoded_bytes());
    expected.push(b'\n');
    let io = |e: std::io::Error| GitError::Io {
        op: "restore-gitfile",
        kind: e.kind(),
    };
    match fs::symlink_metadata(&gitfile) {
        Ok(meta) if meta.is_file() && fs::read(&gitfile).ok().as_ref() == Some(&expected) => {
            return Ok(());
        }
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(&gitfile).map_err(io)?,
        Ok(_) => fs::remove_file(&gitfile).map_err(io)?,
        Err(_) => {}
    }
    fs::write(&gitfile, expected).map_err(io)
}
