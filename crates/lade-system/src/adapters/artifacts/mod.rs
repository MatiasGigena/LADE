//! Immutable SHA-256 content-addressed artifact store.
//!
//! Layout: `<artifacts>/sha256/<first 2 hex>/<remaining 62 hex>`, files 0600, directories 0700.
//! Publication protocol: write to a new temporary file in `tmp` (same filesystem), fsync it, then
//! `link(2)` it into place — which never replaces an existing file — and fsync the shard
//! directory. A crash or failure before the link leaves no published file; an existing digest is
//! never rewritten (publication is idempotent).

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

/// Largest page returned by [`ArtifactStore::read_page`]; larger requests are clamped.
pub const MAX_ARTIFACT_PAGE: usize = 1 << 20;
const CHUNK: usize = 64 << 10;

/// A SHA-256 content digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactDigest([u8; 32]);

impl ArtifactDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self::from_hasher(Sha256::new_with_prefix(bytes))
    }

    fn from_hasher(hasher: Sha256) -> Self {
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&hasher.finalize());
        Self(digest)
    }

    /// Parse 64 lowercase hexadecimal characters.
    pub fn parse(hex: &str) -> Result<Self, ArtifactError> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return Err(ArtifactError::InvalidDigest);
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(ArtifactError::InvalidDigest),
        };
        let mut digest = [0u8; 32];
        for (i, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
            digest[i] = (nibble(*high)? << 4) | nibble(*low)?;
        }
        Ok(Self(digest))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.to_hex())
    }
}

/// Typed store failure; `stage` names the publication step for diagnostics.
#[derive(Debug)]
pub enum ArtifactError {
    Io {
        stage: &'static str,
        kind: io::ErrorKind,
    },
    NotFound,
    InvalidDigest,
    /// Stored bytes no longer hash to their digest.
    Corrupt,
    TooLarge {
        size: u64,
        limit: u64,
    },
}

impl ArtifactError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "artifact_io",
            Self::NotFound => "artifact_not_found",
            Self::InvalidDigest => "invalid_digest",
            Self::Corrupt => "artifact_corrupt",
            Self::TooLarge { .. } => "artifact_too_large",
        }
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { stage, .. } => write!(f, "{} ({stage})", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for ArtifactError {}

fn io_err(stage: &'static str) -> impl Fn(io::Error) -> ArtifactError {
    move |e| ArtifactError::Io {
        stage,
        kind: e.kind(),
    }
}

/// Filesystem steps of publication, replaceable for fault-injection tests.
pub trait ArtifactIo: Send + Sync {
    fn write_chunk(&self, file: &mut File, chunk: &[u8]) -> io::Result<()> {
        file.write_all(chunk)
    }

    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    /// Publish without replacement: must fail with `AlreadyExists` if `dest` exists.
    fn publish(&self, tmp: &Path, dest: &Path) -> io::Result<()> {
        fs::hard_link(tmp, dest)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }
}

/// The real filesystem.
pub struct OsArtifactIo;

impl ArtifactIo for OsArtifactIo {}

/// One page of an artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactPage {
    pub total_bytes: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub eof: bool,
}

pub struct ArtifactStore {
    objects: PathBuf,
    tmp: PathBuf,
    io: Box<dyn ArtifactIo>,
}

impl ArtifactStore {
    /// Open (creating as needed, 0700) the store rooted at `artifacts_dir` with temporary files
    /// in `tmp_dir`, which must be on the same filesystem.
    pub fn open(artifacts_dir: &Path, tmp_dir: &Path) -> Result<Self, ArtifactError> {
        Self::with_io(artifacts_dir, tmp_dir, Box::new(OsArtifactIo))
    }

    /// As [`Self::open`] with substituted filesystem steps (fault injection).
    pub fn with_io(
        artifacts_dir: &Path,
        tmp_dir: &Path,
        io: Box<dyn ArtifactIo>,
    ) -> Result<Self, ArtifactError> {
        let objects = artifacts_dir.join("sha256");
        for dir in [&objects, tmp_dir] {
            private_dir(dir).map_err(io_err("create_dir"))?;
        }
        Ok(Self {
            objects,
            tmp: tmp_dir.to_path_buf(),
            io,
        })
    }

    /// Path of a (possibly absent) artifact.
    pub fn path_of(&self, digest: &ArtifactDigest) -> PathBuf {
        let hex = digest.to_hex();
        self.objects.join(&hex[..2]).join(&hex[2..])
    }

    pub fn contains(&self, digest: &ArtifactDigest) -> bool {
        fs::symlink_metadata(self.path_of(digest)).is_ok_and(|m| m.is_file())
    }

    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactDigest, ArtifactError> {
        self.put_reader(&mut &bytes[..])
    }

    /// Stream `reader` into the store and return its digest.
    pub fn put_reader(&self, reader: &mut dyn Read) -> Result<ArtifactDigest, ArtifactError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp_path = self.tmp.join(format!(
            "artifact-{}-{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)
            .map_err(io_err("create_tmp"))?;
        let _cleanup = RemoveOnDrop(&tmp_path);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err("read_source")(e)),
            };
            hasher.update(&buf[..n]);
            self.io
                .write_chunk(&mut file, &buf[..n])
                .map_err(io_err("write"))?;
        }
        self.io.sync_file(&file).map_err(io_err("sync_file"))?;
        drop(file);
        let digest = ArtifactDigest::from_hasher(hasher);
        let dest = self.path_of(&digest);
        if self.contains(&digest) {
            return Ok(digest);
        }
        let shard = dest.parent().unwrap_or(&self.objects);
        match DirBuilder::new().mode(0o700).create(shard) {
            Ok(()) => self
                .io
                .sync_dir(&self.objects)
                .map_err(io_err("sync_dir"))?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err("create_dir")(e)),
        }
        match self.io.publish(&tmp_path, &dest) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(digest),
            Err(e) => return Err(io_err("publish")(e)),
        }
        self.io.sync_dir(shard).map_err(io_err("sync_dir"))?;
        Ok(digest)
    }

    fn open_artifact(&self, digest: &ArtifactDigest) -> Result<(File, u64), ArtifactError> {
        let file = File::open(self.path_of(digest)).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ArtifactError::NotFound,
            kind => ArtifactError::Io {
                stage: "open",
                kind,
            },
        })?;
        let len = file.metadata().map_err(io_err("stat"))?.len();
        Ok((file, len))
    }

    /// Read up to `limit` bytes (clamped to [`MAX_ARTIFACT_PAGE`]) at `offset`.
    pub fn read_page(
        &self,
        digest: &ArtifactDigest,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, ArtifactError> {
        let (mut file, total) = self.open_artifact(digest)?;
        let offset = offset.min(total);
        file.seek(SeekFrom::Start(offset)).map_err(io_err("seek"))?;
        let mut data = Vec::new();
        file.take(limit.min(MAX_ARTIFACT_PAGE) as u64)
            .read_to_end(&mut data)
            .map_err(io_err("read"))?;
        Ok(ArtifactPage {
            total_bytes: total,
            offset,
            eof: offset + data.len() as u64 >= total,
            data,
        })
    }

    /// Read a whole artifact of at most `max_bytes`, optionally verifying its digest.
    pub fn read_all(
        &self,
        digest: &ArtifactDigest,
        max_bytes: u64,
        verify: bool,
    ) -> Result<Vec<u8>, ArtifactError> {
        let (file, total) = self.open_artifact(digest)?;
        if total > max_bytes {
            return Err(ArtifactError::TooLarge {
                size: total,
                limit: max_bytes,
            });
        }
        let mut data = Vec::new();
        file.take(max_bytes)
            .read_to_end(&mut data)
            .map_err(io_err("read"))?;
        if verify && ArtifactDigest::of(&data) != *digest {
            return Err(ArtifactError::Corrupt);
        }
        Ok(data)
    }

    /// Re-hash a stored artifact; `Ok(false)` if its bytes no longer match.
    pub fn verify(&self, digest: &ArtifactDigest) -> Result<bool, ArtifactError> {
        let (mut file, _) = self.open_artifact(digest)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io_err("read")(e)),
            }
        }
        Ok(ArtifactDigest::from_hasher(hasher) == *digest)
    }
}

fn private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().mode(0o700).recursive(true).create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}
