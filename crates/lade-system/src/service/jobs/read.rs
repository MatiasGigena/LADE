//! Repository and file reads: opening a workspace, resolving the target, reading file bytes
//! (whole, or through the artifact store for large blobs) and checking Reference staleness.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use lade_core::domain::ids::GitId;
use lade_core::domain::inspection::{CurrentContent, Reference, Side};
use lade_core::domain::review::ChangeCounts;
use lade_protocol::client::Rejection;
use sha2::{Digest as _, Sha256};

use crate::adapters::artifacts::{ArtifactDigest, ArtifactStore};
use crate::adapters::git::{self, ChangedFile, GitError, Repository};

use crate::service::convert::{adapter_git, core_git, git_rejection, not_found};
use crate::support::hex;

pub const MAX_WHOLE_FILE: u64 = 64 << 20;
pub const MAX_CHUNKED_FILE: u64 = 1 << 30;
const LARGE_BLOB_MEMORY: usize = 8;

pub fn open_repository(path: &Path) -> Result<Repository, GitError> {
    Repository::open(path).map(|repo| repo.with_excludes_file(git::user_excludes_file()))
}

pub fn target_commit(repo: &Repository, branch: &str) -> Result<GitId, Rejection> {
    repo.resolve_ref(&format!("refs/heads/{branch}"))
        .map_err(git_rejection)?
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
    let Some(entry) = repo.tree_entry(&tree, path).map_err(git_rejection)? else {
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
        .map_err(git_rejection)?
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
        .map_err(git_rejection)?
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
            .map_err(git_rejection)?
            .map_err(|_| crate::service::convert::durability("content could not be retained"))?;
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
