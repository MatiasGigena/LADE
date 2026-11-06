//! Capturing a worktree as a tree without touching its index, the user excludes file it
//! honours, and the paths a filter driver would rewrite.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use super::command::GitCommand;
use super::{
    Diagnostics, GitError, GitId, MIN_CHECK_ATTR_SOURCE_VERSION, RemoveOnDrop, RepoPath,
    Repository, Result, Worktree, unique_suffix,
};

impl Repository {
    /// Writes a tree of everything in `worktree` (tracked, untracked and modified files, with
    /// the repository's ignore rules) through a scratch index in `scratch_dir`, without touching the
    /// worktree's own index.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Failed`] when `git add` fails for a reason other than a nested repository
    /// without a commit (up to [`MAX_SKIPPED_NESTED`] of those are skipped and reported), or a command
    /// error when another step fails.
    pub fn capture_working_tree(
        &self,
        worktree: &Worktree,
        scratch_dir: &Path,
    ) -> Result<WorkingCapture> {
        let index = scratch_dir.join(format!(
            "lade-capture-{}-{}.index",
            std::process::id(),
            unique_suffix()
        ));
        let mut lock = index.clone().into_os_string();
        lock.push(".lock");
        let _cleanup = RemoveOnDrop(vec![index.clone(), PathBuf::from(lock)]);
        let base = |op| worktree.git(op).env("GIT_INDEX_FILE", &index);
        let head = worktree
            .git("rev-parse")
            .read_only()
            .args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .output()?;
        if head.status.success() {
            let head = self.parse_id("rev-parse", &head.stdout)?;
            base("read-tree").args(["read-tree", head.hex()]).run()?;
        } else {
            base("read-tree").args(["read-tree", "--empty"]).run()?;
        }
        let excludes = self
            .excludes_file
            .as_deref()
            .map_or_else(|| OsStr::new("/dev/null"), Path::as_os_str);
        let mut skipped_nested: Vec<RepoPath> = Vec::new();
        loop {
            let mut add = base("add")
                .config("core.fileMode", "true")
                .config("core.symlinks", "true")
                .config("core.ignoreCase", "false")
                .config("core.excludesFile", excludes)
                .config("advice.addEmbeddedRepo", "false")
                .args(["add", "--all", "--no-ignore-errors"]);
            if !skipped_nested.is_empty() {
                add = add.env("GIT_LITERAL_PATHSPECS", "0").arg("--");
                for path in &skipped_nested {
                    let mut spec = b":(exclude,literal)".to_vec();
                    spec.extend_from_slice(path.as_bytes());
                    add = add.arg(OsStr::from_bytes(&spec));
                }
            }
            let out = add.output()?;
            if out.status.success() {
                break;
            }
            match nested_without_commit(&out.stderr) {
                Some(path)
                    if skipped_nested.len() < MAX_SKIPPED_NESTED
                        && !skipped_nested.contains(&path) =>
                {
                    skipped_nested.push(path);
                }
                _ => {
                    return Err(GitError::Failed {
                        op: "add",
                        exit_code: out.status.code(),
                        stderr: out.stderr,
                    });
                }
            }
        }
        let out = base("write-tree").arg("write-tree").run()?;
        Ok(WorkingCapture {
            tree: self.parse_id("write-tree", &out)?,
            skipped_nested,
        })
    }

    #[must_use]
    pub fn with_excludes_file(mut self, path: Option<PathBuf>) -> Self {
        self.excludes_file = path;
        self
    }

    /// The paths among `paths` that a `filter` attribute (Git LFS, clean/smudge) applies to,
    /// as of `tree` when the installed Git supports it.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidId`] for a `tree` of another object format, a command error when
    /// `git check-attr` fails, or [`GitError::Parse`] when it reports an invalid path.
    pub fn filtered_paths(&self, tree: &GitId, paths: &[&RepoPath]) -> Result<Vec<RepoPath>> {
        self.check_id(tree)?;
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let mut cmd = self
            .git("check-attr")
            .read_only()
            .args(["check-attr", "-z", "--stdin"]);
        if self.version >= MIN_CHECK_ATTR_SOURCE_VERSION {
            cmd = cmd.arg(format!("--source={}", tree.hex()));
        }
        let out = cmd.args(["filter"]).stdin(input).run()?;
        let mut fields = out.split(|b| *b == 0);
        let mut filtered = Vec::new();
        while let (Some(path), Some(_), Some(info)) = (fields.next(), fields.next(), fields.next())
        {
            if !matches!(info, b"unspecified" | b"unset" | b"set") && !path.is_empty() {
                let path = RepoPath::from_git("check-attr", path)?;
                if !filtered.contains(&path) {
                    filtered.push(path);
                }
            }
        }
        Ok(filtered)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingCapture {
    pub tree: GitId,
    pub skipped_nested: Vec<RepoPath>,
}

pub const MAX_SKIPPED_NESTED: usize = 64;

fn nested_without_commit(stderr: &Diagnostics) -> Option<RepoPath> {
    const PREFIX: &[u8] = b"error: '";
    const SUFFIX: &[u8] = b"' does not have a commit checked out";
    stderr.as_bytes().split(|b| *b == b'\n').find_map(|line| {
        let inner = line.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
        RepoPath::new(inner.strip_suffix(b"/").unwrap_or(inner).to_vec()).ok()
    })
}

pub fn user_excludes_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|x| x.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let mut configured = None;
    for file in [xdg.join("git/config"), home.join(".gitconfig")] {
        if !file.is_file() {
            continue;
        }
        let out = GitCommand::new("config-excludes")
            .without_config_scan()
            .read_only()
            .env("HOME", &home)
            .args(["config", "--file"])
            .arg(&file)
            .args(["--includes", "--type=path", "--get", "core.excludesFile"])
            .output()
            .ok()?;
        if out.status.success() {
            let value = out.stdout.strip_suffix(b"\n").unwrap_or(&out.stdout);
            configured = Some(PathBuf::from(OsString::from_vec(value.to_vec())));
        }
    }
    let path = configured.unwrap_or_else(|| xdg.join("git/ignore"));
    (path.is_absolute() && path.is_file()).then_some(path)
}
