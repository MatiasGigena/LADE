//! Bounded reads of tree entries, blobs and content search.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::ffi::OsStrExt;

use super::{GitError, GitId, RepoPath, Repository, Result};

/// Largest page [`Repository::read_blob`] returns; larger requests are clamped.
pub const MAX_BLOB_PAGE: usize = 1 << 20;
/// Upper bound on grep results regardless of the requested limit.
pub const MAX_GREP_RESULTS: usize = 1000;
const MAX_GREP_PREVIEW: usize = 512;
const MAX_GREP_OUTPUT: u64 = 16 << 20;
const MAX_GREP_QUERY: usize = 1024;
/// Matched blobs larger than this get no byte offsets (line and column only).
pub const MAX_GREP_OFFSET_BLOB: u64 = 16 << 20;
/// Total matched-blob bytes read per search to compute byte offsets.
const MAX_GREP_OFFSET_TOTAL: u64 = 64 << 20;

/// Kind of a tree entry's object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Blob,
    Tree,
    /// A submodule gitlink.
    Commit,
}

/// One entry of a tree listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Octal mode as stored by Git (`100644`, `100755`, `120000`, `040000`, `160000`).
    pub mode: u32,
    pub kind: ObjectKind,
    pub id: GitId,
    /// Blob size in bytes; `None` for trees and gitlinks.
    pub size: Option<u64>,
    pub path: RepoPath,
}

/// One bounded page of blob content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobPage {
    pub blob: GitId,
    pub mode: u32,
    pub total_bytes: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub eof: bool,
}

/// A fixed-string content search.
#[derive(Clone, Debug)]
pub struct GrepQuery {
    /// Literal text (1..=1024 bytes, no newline or NUL).
    pub text: String,
    pub ignore_case: bool,
    /// Clamped to [`MAX_GREP_RESULTS`].
    pub max_results: usize,
}

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: RepoPath,
    /// 1-based line number.
    pub line: u64,
    /// 1-based byte column of the first match in the line.
    pub column: u64,
    /// Byte range `[byte_start, byte_end)` of the first match within the blob (the exact raw
    /// bytes `file.read` retains), confirmed against those bytes. Both `None` when the blob
    /// exceeds [`MAX_GREP_OFFSET_BLOB`], the per-search offset budget was spent or the match
    /// could not be confirmed.
    pub byte_start: Option<u64>,
    pub byte_end: Option<u64>,
    /// Line bytes, at most 512.
    pub preview: Vec<u8>,
    pub preview_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepResults {
    pub matches: Vec<GrepMatch>,
    /// More matches existed than were returned (result cap or output bound reached).
    pub truncated: bool,
}

impl Repository {
    /// List a tree (`tree` may be a commit). `dir` restricts to that directory's contents.
    pub fn ls_tree(
        &self,
        tree: &GitId,
        dir: Option<&RepoPath>,
        recursive: bool,
    ) -> Result<Vec<TreeEntry>> {
        let mut cmd = self
            .git("ls-tree")
            .read_only()
            .args(["ls-tree", "-z", "-l", "--full-tree"]);
        if recursive {
            cmd = cmd.arg("-r");
        }
        cmd = cmd.arg(tree.hex());
        if let Some(dir) = dir {
            let mut spec = dir.as_bytes().to_vec();
            if !recursive {
                spec.push(b'/');
            }
            cmd = cmd.arg("--").arg(OsStr::from_bytes(&spec));
        }
        let out = cmd.run()?;
        out.split(|b| *b == 0)
            .filter(|r| !r.is_empty())
            .map(|r| self.parse_tree_entry(r))
            .collect()
    }

    /// The entry at exactly `path` in `tree`, if any.
    pub fn tree_entry(&self, tree: &GitId, path: &RepoPath) -> Result<Option<TreeEntry>> {
        let out = self
            .git("ls-tree")
            .read_only()
            .args(["ls-tree", "-z", "-l", "--full-tree", tree.hex(), "--"])
            .arg(path.as_os_str())
            .run()?;
        for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let entry = self.parse_tree_entry(record)?;
            if entry.path == *path {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    fn parse_tree_entry(&self, record: &[u8]) -> Result<TreeEntry> {
        let parse_err = || GitError::Parse {
            op: "ls-tree",
            what: "entry",
        };
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(parse_err)?;
        let meta = std::str::from_utf8(&record[..tab]).map_err(|_| parse_err())?;
        let mut fields = meta.split_ascii_whitespace();
        let (Some(mode), Some(kind), Some(id), Some(size)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(parse_err());
        };
        let kind = match kind {
            "blob" => ObjectKind::Blob,
            "tree" => ObjectKind::Tree,
            "commit" => ObjectKind::Commit,
            _ => {
                return Err(GitError::Parse {
                    op: "ls-tree",
                    what: "kind",
                });
            }
        };
        Ok(TreeEntry {
            mode: u32::from_str_radix(mode, 8).map_err(|_| parse_err())?,
            kind,
            id: self.parse_id("ls-tree", id.as_bytes())?,
            size: if size == "-" {
                None
            } else {
                Some(size.parse().map_err(|_| parse_err())?)
            },
            path: RepoPath::from_git("ls-tree", &record[tab + 1..])?,
        })
    }

    /// Read `limit` bytes (clamped to [`MAX_BLOB_PAGE`]) at `offset` of the blob at `path` in
    /// `tree`. Content is raw object bytes: no filters, no textconv.
    pub fn read_blob(
        &self,
        tree: &GitId,
        path: &RepoPath,
        offset: u64,
        limit: usize,
    ) -> Result<BlobPage> {
        let entry = self
            .tree_entry(tree, path)?
            .ok_or(GitError::NotFound { what: "path" })?;
        let (ObjectKind::Blob, Some(total)) = (entry.kind, entry.size) else {
            return Err(GitError::NotABlob);
        };
        let limit = limit.min(MAX_BLOB_PAGE) as u64;
        let mut data = Vec::new();
        if offset < total && limit > 0 {
            let mut stream = self
                .git("cat-file")
                .read_only()
                .args(["cat-file", "blob", entry.id.hex()])
                .stream()?;
            let op = stream.op();
            let io = |e: io::Error| GitError::Io { op, kind: e.kind() };
            io::copy(&mut stream.stdout().take(offset), &mut io::sink()).map_err(io)?;
            stream
                .stdout()
                .take(limit)
                .read_to_end(&mut data)
                .map_err(io)?;
            if offset + (data.len() as u64) < total {
                stream.abort();
            } else {
                let (status, stderr) = stream.finish()?;
                if !status.success() {
                    return Err(GitError::Failed {
                        op,
                        exit_code: status.code(),
                        stderr,
                    });
                }
            }
        }
        let eof = offset.saturating_add(data.len() as u64) >= total;
        Ok(BlobPage {
            blob: entry.id,
            mode: entry.mode,
            total_bytes: total,
            offset: offset.min(total),
            data,
            eof,
        })
    }

    /// Stream the raw bytes of blob `id` (no filters) to `consume`, which may read as much as it
    /// needs; the process is reaped afterwards. A Git failure after `consume` returned is an
    /// error, so a consumer that read to the end saw the complete blob.
    pub fn with_blob_reader<R>(
        &self,
        id: &GitId,
        consume: impl FnOnce(&mut dyn Read) -> R,
    ) -> Result<R> {
        self.check_id(id)?;
        let mut stream = self
            .git("cat-file")
            .read_only()
            .args(["cat-file", "blob", id.hex()])
            .stream()?;
        let op = stream.op();
        let result = consume(stream.stdout());
        let (status, stderr) = stream.finish()?;
        if status.success() {
            Ok(result)
        } else {
            Err(GitError::Failed {
                op,
                exit_code: status.code(),
                stderr,
            })
        }
    }

    /// Whole blob by id if it is at most `cap` bytes (`Ok(None)` when larger).
    pub(crate) fn read_blob_capped(&self, id: &GitId, cap: u64) -> Result<Option<Vec<u8>>> {
        let size = self
            .git("cat-file")
            .read_only()
            .args(["cat-file", "-s", id.hex()])
            .run()?;
        let size: u64 = std::str::from_utf8(size.trim_ascii())
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(GitError::Parse {
                op: "cat-file",
                what: "size",
            })?;
        if size > cap {
            return Ok(None);
        }
        let data = self
            .git("cat-file")
            .read_only()
            .args(["cat-file", "blob", id.hex()])
            .stdout_limit(size as usize)
            .run()?;
        Ok(Some(data))
    }

    /// Bounded fixed-string search over the text blobs of `tree` (binary files skipped).
    pub fn grep(&self, tree: &GitId, query: &GrepQuery) -> Result<GrepResults> {
        let text = query.text.as_bytes();
        if text.is_empty() || text.len() > MAX_GREP_QUERY || text.contains(&b'\n') {
            return Err(GitError::InvalidArgument { what: "grep query" });
        }
        let max = query.max_results.clamp(1, MAX_GREP_RESULTS);
        let mut cmd = self.git("grep").read_only().args([
            "grep",
            "-z",
            "-n",
            "--column",
            "--no-color",
            "-I",
            "-F",
            "--no-textconv",
        ]);
        if query.ignore_case {
            cmd = cmd.arg("-i");
        }
        let mut stream = cmd
            .arg("-e")
            .arg(&query.text)
            .arg(tree.hex())
            .arg("--")
            .stream()?;
        let op = stream.op();
        let prefix = format!("{}:", tree.hex()).into_bytes();
        let mut matches = Vec::new();
        let mut truncated = false;
        let mut consumed = 0u64;
        {
            let mut reader = BufReader::new(stream.stdout());
            let io = |e: io::Error| GitError::Io { op, kind: e.kind() };
            let parse = || GitError::Parse {
                op: "grep",
                what: "record",
            };
            loop {
                let mut name = Vec::new();
                let (n, found) =
                    read_until_bounded(&mut reader, 0, 64 << 10, &mut name).map_err(io)?;
                if n == 0 {
                    break;
                }
                if !found {
                    return Err(parse());
                }
                let mut line = Vec::new();
                let mut column = Vec::new();
                let mut preview = Vec::new();
                let (a, _) = read_until_bounded(&mut reader, 0, 32, &mut line).map_err(io)?;
                let (b, _) = read_until_bounded(&mut reader, 0, 32, &mut column).map_err(io)?;
                let (c, found_nl) =
                    read_until_bounded(&mut reader, b'\n', MAX_GREP_PREVIEW, &mut preview)
                        .map_err(io)?;
                consumed += (n + a + b + c) as u64;
                let number = |bytes: &[u8]| {
                    std::str::from_utf8(bytes)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(parse)
                };
                let path = name.strip_prefix(&prefix[..]).ok_or_else(parse)?;
                let preview_truncated = c - usize::from(found_nl) > preview.len();
                matches.push(GrepMatch {
                    path: RepoPath::from_git("grep", path)?,
                    line: number(&line)?,
                    column: number(&column)?,
                    byte_start: None,
                    byte_end: None,
                    preview,
                    preview_truncated,
                });
                if matches.len() > max || consumed > MAX_GREP_OUTPUT {
                    truncated = true;
                    break;
                }
            }
        }
        if truncated {
            matches.truncate(max);
            stream.abort();
        } else {
            let (status, stderr) = stream.finish()?;
            if !matches!(status.code(), Some(0 | 1)) {
                return Err(GitError::Failed {
                    op,
                    exit_code: status.code(),
                    stderr,
                });
            }
        }
        self.grep_offsets(tree, query, &mut matches)?;
        Ok(GrepResults { matches, truncated })
    }

    /// Fill in exact byte offsets: the start of line `line` plus `column - 1`, and the match end
    /// once the matched bytes are confirmed against the query (case-insensitively when asked).
    fn grep_offsets(
        &self,
        tree: &GitId,
        query: &GrepQuery,
        matches: &mut [GrepMatch],
    ) -> Result<()> {
        let mut budget = MAX_GREP_OFFSET_TOTAL;
        let mut index = 0;
        while index < matches.len() {
            let path = matches[index].path.clone();
            let same = matches[index..]
                .iter()
                .take_while(|m| m.path == path)
                .count();
            let group = &mut matches[index..index + same];
            index += same;
            let Some(entry) = self.tree_entry(tree, &path)? else {
                continue;
            };
            let Some(size) = entry
                .size
                .filter(|s| *s <= MAX_GREP_OFFSET_BLOB && *s <= budget)
            else {
                continue;
            };
            let Some(blob) = self.read_blob_capped(&entry.id, size)? else {
                continue;
            };
            budget -= size;
            let starts = line_starts(&blob);
            for found in group {
                let line_start = usize::try_from(found.line)
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|n| starts.get(n).copied());
                let column = usize::try_from(found.column)
                    .ok()
                    .and_then(|c| c.checked_sub(1));
                let range = line_start
                    .zip(column)
                    .map(|(line, col)| line + col)
                    .filter(|s| *s <= blob.len())
                    .and_then(|s| {
                        match_end(&blob[s..], &query.text, query.ignore_case).map(|n| (s, s + n))
                    });
                found.byte_start = range.map(|(s, _)| s as u64);
                found.byte_end = range.map(|(_, e)| e as u64);
            }
        }
        Ok(())
    }
}

/// Byte start of every line of `bytes` (line `n` starts at `starts[n - 1]`).
fn line_starts(bytes: &[u8]) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        )
        .collect()
}

/// Length in bytes of `needle` matched at the start of `hay`. Git runs with `LC_ALL=C`, so a
/// case-insensitive match folds ASCII only and always has the needle's byte length.
fn match_end(hay: &[u8], needle: &str, ignore_case: bool) -> Option<usize> {
    let exact = needle.as_bytes();
    let head = hay.get(..exact.len())?;
    (head == exact || (ignore_case && head.eq_ignore_ascii_case(exact))).then_some(exact.len())
}

/// Read through `delim` (consumed, not stored), keeping at most `keep` bytes in `out`.
/// Returns total bytes consumed and whether the delimiter was found.
fn read_until_bounded(
    reader: &mut impl BufRead,
    delim: u8,
    keep: usize,
    out: &mut Vec<u8>,
) -> io::Result<(usize, bool)> {
    let mut consumed = 0;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok((consumed, false));
        }
        let (chunk, found) = match buf.iter().position(|b| *b == delim) {
            Some(i) => (i, true),
            None => (buf.len(), false),
        };
        let room = keep.saturating_sub(out.len());
        out.extend_from_slice(&buf[..chunk.min(room)]);
        let used = chunk + usize::from(found);
        reader.consume(used);
        consumed += used;
        if found {
            return Ok((consumed, true));
        }
    }
}
