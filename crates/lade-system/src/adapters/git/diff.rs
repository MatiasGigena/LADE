use super::tree::ObjectKind;
use super::{GitError, GitId, RepoPath, Repository, Result};

pub const MAX_DIFF_LINES: usize = 20_000;
pub const MAX_DIFF_BLOB_BYTES: u64 = 16 << 20;
const BINARY_PROBE: usize = 8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed { similarity: u8 },
    Copied { similarity: u8 },
    TypeChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: RepoPath,
    pub old_path: Option<RepoPath>,
    pub status: ChangeStatus,
    pub old_mode: u32,
    pub new_mode: u32,
    pub old_id: Option<GitId>,
    pub new_id: Option<GitId>,
    pub added: u64,
    pub removed: u64,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u64>,
    pub new_no: Option<u64>,
    pub old_byte_start: Option<u64>,
    pub new_byte_start: Option<u64>,
    pub text: Vec<u8>,
    pub no_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub old_blob: Option<GitId>,
    pub new_blob: Option<GitId>,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub binary: bool,
    pub too_large: bool,
    pub hunks: Vec<Hunk>,
    pub truncated: bool,
}

impl Repository {
    /// The files that differ between trees `old` and `new`, with renames detected and line
    /// counts.
    ///
    /// # Errors
    ///
    /// Returns a command error when `git diff-tree` fails, or [`GitError::Parse`] when its output is
    /// malformed.
    pub fn diff_numstat(&self, old: &GitId, new: &GitId) -> Result<Vec<ChangedFile>> {
        let out = self
            .git("diff-tree")
            .read_only()
            .args([
                "diff-tree",
                "-r",
                "-z",
                "-M",
                "--raw",
                "--numstat",
                "--no-abbrev",
                "--no-ext-diff",
                "--no-textconv",
                old.hex(),
                new.hex(),
            ])
            .run()?;
        self.parse_raw_numstat(&out)
    }

    fn parse_raw_numstat(&self, out: &[u8]) -> Result<Vec<ChangedFile>> {
        let bad = |what| GitError::Parse {
            op: "diff-tree",
            what,
        };
        let mut tokens = out.split(|b| *b == 0).peekable();
        let mut files = Vec::new();
        while let Some(meta) = tokens.next_if(|t| t.starts_with(b":")) {
            let meta = std::str::from_utf8(&meta[1..]).map_err(|_| bad("raw"))?;
            let fields: Vec<&str> = meta.split(' ').collect();
            let [old_mode, new_mode, old_id, new_id, status] = fields[..] else {
                return Err(bad("raw"));
            };
            let mode = |m: &str| u32::from_str_radix(m, 8).map_err(|_| bad("mode"));
            let id = |h: &str| -> Result<Option<GitId>> {
                let id = self.parse_id("diff-tree", h.as_bytes())?;
                Ok((!id.is_zero()).then_some(id))
            };
            let score = || status[1..].parse::<u8>().map_err(|_| bad("score"));
            let status = match status.as_bytes().first() {
                Some(b'A') => ChangeStatus::Added,
                Some(b'M') => ChangeStatus::Modified,
                Some(b'D') => ChangeStatus::Deleted,
                Some(b'T') => ChangeStatus::TypeChanged,
                Some(b'R') => ChangeStatus::Renamed {
                    similarity: score()?,
                },
                Some(b'C') => ChangeStatus::Copied {
                    similarity: score()?,
                },
                _ => return Err(bad("status")),
            };
            let first = tokens.next().ok_or(bad("path"))?;
            let (path, old_path) = match status {
                ChangeStatus::Renamed { .. } | ChangeStatus::Copied { .. } => {
                    let second = tokens.next().ok_or(bad("path"))?;
                    (
                        RepoPath::from_git("diff-tree", second)?,
                        Some(RepoPath::from_git("diff-tree", first)?),
                    )
                }
                _ => (RepoPath::from_git("diff-tree", first)?, None),
            };
            files.push(ChangedFile {
                path,
                old_path,
                status,
                old_mode: mode(old_mode)?,
                new_mode: mode(new_mode)?,
                old_id: id(old_id)?,
                new_id: id(new_id)?,
                added: 0,
                removed: 0,
                binary: false,
            });
        }
        for file in &mut files {
            let record = tokens.next().ok_or(bad("numstat"))?;
            let mut parts = record.splitn(3, |b| *b == b'\t');
            let (Some(added), Some(removed), Some(path)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(bad("numstat"));
            };
            if path.is_empty() {
                let (src, dst) = (tokens.next(), tokens.next());
                let expected_src = file.old_path.as_ref().map(RepoPath::as_bytes);
                if src != expected_src || dst != Some(file.path.as_bytes()) {
                    return Err(bad("numstat order"));
                }
            } else if path != file.path.as_bytes() {
                return Err(bad("numstat order"));
            }
            if added == b"-" && removed == b"-" {
                file.binary = true;
            } else {
                let count = |b: &[u8]| {
                    std::str::from_utf8(b)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or(bad("numstat"))
                };
                file.added = count(added)?;
                file.removed = count(removed)?;
            }
        }
        if tokens.any(|t| !t.is_empty()) {
            return Err(bad("trailing"));
        }
        Ok(files)
    }

    /// The bounded line diff of `path` between trees `old` and `new`; `old_path` names the
    /// file in `old` when it was renamed.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::NotFound`] when the path is in neither tree, or the errors of reading the
    /// tree entries and blobs.
    pub fn diff_hunks(
        &self,
        old: &GitId,
        new: &GitId,
        path: &RepoPath,
        old_path: Option<&RepoPath>,
    ) -> Result<FileDiff> {
        let old_entry = self.tree_entry(old, old_path.unwrap_or(path))?;
        let new_entry = self.tree_entry(new, path)?;
        if old_entry.is_none() && new_entry.is_none() {
            return Err(GitError::NotFound { what: "path" });
        }
        let mut diff = FileDiff {
            old_blob: old_entry.as_ref().map(|e| e.id.clone()),
            new_blob: new_entry.as_ref().map(|e| e.id.clone()),
            old_mode: old_entry.as_ref().map(|e| e.mode),
            new_mode: new_entry.as_ref().map(|e| e.mode),
            binary: false,
            too_large: false,
            hunks: Vec::new(),
            truncated: false,
        };
        let non_blob = [&old_entry, &new_entry]
            .iter()
            .any(|e| e.as_ref().is_some_and(|e| e.kind != ObjectKind::Blob));
        if non_blob {
            diff.binary = true;
            return Ok(diff);
        }
        let load = |id: &Option<GitId>| -> Result<Option<Option<Vec<u8>>>> {
            id.as_ref()
                .map(|id| self.read_blob_capped(id, MAX_DIFF_BLOB_BYTES))
                .transpose()
        };
        let (old_bytes, new_bytes) = match (load(&diff.old_blob)?, load(&diff.new_blob)?) {
            (Some(None), _) | (_, Some(None)) => {
                diff.too_large = true;
                diff.truncated = true;
                return Ok(diff);
            }
            (o, n) => (o.flatten(), n.flatten()),
        };
        let is_binary = |b: &Option<Vec<u8>>| {
            b.as_ref()
                .is_some_and(|b| b[..b.len().min(BINARY_PROBE)].contains(&0))
        };
        if is_binary(&old_bytes) || is_binary(&new_bytes) {
            diff.binary = true;
            return Ok(diff);
        }
        let sides = (&old_bytes, &new_bytes, &diff.old_blob, &diff.new_blob);
        let (hunks, truncated) = match sides {
            (None, Some(new), ..) => whole_file(new, LineKind::Add),
            (Some(old), None, ..) => whole_file(old, LineKind::Del),
            (Some(_), Some(_), Some(old_id), Some(new_id)) if old_id == new_id => {
                (Vec::new(), false)
            }
            (Some(old), Some(new), Some(old_id), Some(new_id)) => {
                let patch = self
                    .git("diff")
                    .read_only()
                    .args([
                        "diff",
                        "--no-color",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--diff-algorithm=myers",
                        "--indent-heuristic",
                        "-U3",
                        "--inter-hunk-context=0",
                        "--src-prefix=a/",
                        "--dst-prefix=b/",
                        old_id.hex(),
                        new_id.hex(),
                    ])
                    .run()?;
                if let Some(parsed) = parse_patch(&patch, old, new)? {
                    parsed
                } else {
                    diff.binary = true;
                    return Ok(diff);
                }
            }
            _ => (Vec::new(), false),
        };
        diff.hunks = hunks;
        diff.truncated = truncated;
        Ok(diff)
    }
}

fn line_starts(bytes: &[u8]) -> Vec<usize> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut starts = vec![0];
    starts.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(i, b)| **b == b'\n' && i + 1 < bytes.len())
            .map(|(i, _)| i + 1),
    );
    starts
}

fn whole_file(bytes: &[u8], kind: LineKind) -> (Vec<Hunk>, bool) {
    let starts = line_starts(bytes);
    if starts.is_empty() {
        return (Vec::new(), false);
    }
    let truncated = starts.len() > MAX_DIFF_LINES;
    let lines = starts
        .iter()
        .take(MAX_DIFF_LINES)
        .enumerate()
        .map(|(i, &start)| {
            let rest = &bytes[start..];
            let end = rest.iter().position(|b| *b == b'\n');
            let no = Some(i as u64 + 1);
            let start = Some(start as u64);
            let (old_no, new_no, old_byte_start, new_byte_start) = match kind {
                LineKind::Del => (no, None, start, None),
                _ => (None, no, None, start),
            };
            DiffLine {
                kind,
                old_no,
                new_no,
                old_byte_start,
                new_byte_start,
                text: rest[..end.unwrap_or(rest.len())].to_vec(),
                no_newline: end.is_none(),
            }
        })
        .collect();
    let count = starts.len() as u64;
    let hunk = match kind {
        LineKind::Del => Hunk {
            old_start: 1,
            old_lines: count,
            new_start: 0,
            new_lines: 0,
            lines,
        },
        _ => Hunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: count,
            lines,
        },
    };
    (vec![hunk], truncated)
}

fn parse_range(text: &str) -> Option<(u64, u64)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// The empty hunk that the header `@@ -<old range> +<new range> @@` opens, given the header
/// without its `@@ -` prefix.
fn open_hunk(header: &[u8]) -> Option<Hunk> {
    let (ranges, _) = std::str::from_utf8(header).ok()?.split_once(" @@")?;
    let (old_range, new_range) = ranges.split_once(" +")?;
    let (old_start, old_lines) = parse_range(old_range)?;
    let (new_start, new_lines) = parse_range(new_range)?;
    Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        lines: Vec::new(),
    })
}

fn parse_patch(patch: &[u8], old: &[u8], new: &[u8]) -> Result<Option<(Vec<Hunk>, bool)>> {
    let bad = |what| GitError::Parse { op: "diff", what };
    let old_starts = line_starts(old);
    let new_starts = line_starts(new);
    let locate = |starts: &[usize], blob: &[u8], no: u64, text: &[u8]| -> Result<u64> {
        let start = *no
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| starts.get(index))
            .ok_or(bad("line number"))?;
        let rest = &blob[start..];
        let terminated = rest.get(text.len()).is_none_or(|b| *b == b'\n');
        if rest.starts_with(text) && terminated {
            Ok(start as u64)
        } else {
            Err(bad("line offset"))
        }
    };
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut total = 0usize;
    let (mut old_left, mut new_left, mut old_no, mut new_no) = (0u64, 0u64, 0u64, 0u64);
    let mut records = patch.split(|b| *b == b'\n').peekable();
    while let Some(line) = records.next() {
        if old_left == 0 && new_left == 0 {
            if let Some(header) = line.strip_prefix(b"@@ -") {
                let hunk = open_hunk(header).ok_or(bad("hunk header"))?;
                (old_left, new_left) = (hunk.old_lines, hunk.new_lines);
                old_no = hunk.old_start + u64::from(hunk.old_lines == 0);
                new_no = hunk.new_start + u64::from(hunk.new_lines == 0);
                hunks.push(hunk);
                continue;
            }
            if line.starts_with(b"\\") {
                if let Some(last) = hunks.last_mut().and_then(|h| h.lines.last_mut()) {
                    last.no_newline = true;
                }
                continue;
            }
            if hunks.is_empty() {
                if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
                    return Ok(None);
                }
                continue;
            }
            if line.is_empty() && records.peek().is_none() {
                break;
            }
            return Err(bad("hunk body"));
        }
        let Some(hunk) = hunks.last_mut() else {
            return Err(bad("hunk body"));
        };
        let (kind, text) = match line.split_first() {
            Some((b' ', text)) => (LineKind::Context, text),
            Some((b'+', text)) => (LineKind::Add, text),
            Some((b'-', text)) => (LineKind::Del, text),
            Some((b'\\', _)) => {
                if let Some(last) = hunk.lines.last_mut() {
                    last.no_newline = true;
                }
                continue;
            }
            _ => return Err(bad("hunk line")),
        };
        if total == MAX_DIFF_LINES {
            return Ok(Some((hunks, true)));
        }
        total += 1;
        let has_old = kind != LineKind::Add;
        let has_new = kind != LineKind::Del;
        if (has_old && old_left == 0) || (has_new && new_left == 0) {
            return Err(bad("hunk count"));
        }
        let mut line = DiffLine {
            kind,
            old_no: None,
            new_no: None,
            old_byte_start: None,
            new_byte_start: None,
            text: text.to_vec(),
            no_newline: false,
        };
        if has_old {
            line.old_no = Some(old_no);
            line.old_byte_start = Some(locate(&old_starts, old, old_no, text)?);
            old_no += 1;
            old_left -= 1;
        }
        if has_new {
            line.new_no = Some(new_no);
            line.new_byte_start = Some(locate(&new_starts, new, new_no, text)?);
            new_no += 1;
            new_left -= 1;
        }
        hunk.lines.push(line);
    }
    if old_left != 0 || new_left != 0 {
        return Err(bad("incomplete hunk"));
    }
    Ok(Some((hunks, false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_handle_trailing_newline_and_crlf() {
        assert_eq!(line_starts(b""), Vec::<usize>::new());
        assert_eq!(line_starts(b"a\r\nb"), vec![0, 3]);
        assert_eq!(line_starts(b"a\nb\n"), vec![0, 2]);
    }

    #[test]
    fn patch_offsets_are_byte_exact() {
        let old = "α\nb\nc\n".as_bytes();
        let new = "α\nβ\nc\nd".as_bytes();
        let patch = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,4 @@\n \xce\xb1\n-b\n+\xce\xb2\n c\n+d\n\\ No newline at end of file\n";
        let (hunks, truncated) = parse_patch(patch, old, new).unwrap().unwrap();
        assert!(!truncated);
        let lines = &hunks[0].lines;
        assert_eq!(lines[0].new_byte_start, Some(0));
        assert_eq!(lines[1].old_byte_start, Some(3));
        assert_eq!(lines[2].new_byte_start, Some(3));
        assert_eq!(lines[3].new_byte_start, Some(6));
        assert_eq!(lines[4].new_byte_start, Some(8));
        assert!(lines[4].no_newline);
    }

    #[test]
    fn binary_patch_is_reported() {
        assert!(
            parse_patch(b"Binary files a/x and b/x differ\n", b"", b"")
                .unwrap()
                .is_none()
        );
    }
}
