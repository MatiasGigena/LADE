use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

use crate::support::fs::ensure_private_dir;

pub const MAX_ARTIFACT_PAGE: usize = 1 << 20;
const CHUNK: usize = 64 << 10;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactDigest([u8; 32]);

impl ArtifactDigest {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self::from_hasher(Sha256::new_with_prefix(bytes))
    }

    fn from_hasher(hasher: Sha256) -> Self {
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&hasher.finalize());
        Self(digest)
    }

    /// Parses a lowercase hex SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::InvalidDigest`] when `hex` is not 64 lowercase hex digits.
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

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
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

#[derive(Debug)]
pub enum ArtifactError {
    Io {
        stage: &'static str,
        kind: io::ErrorKind,
    },
    NotFound,
    InvalidDigest,
    Corrupt,
    TooLarge {
        size: u64,
        limit: u64,
    },
}

impl ArtifactError {
    #[must_use]
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

pub trait ArtifactIo: Send + Sync {
    /// Appends `chunk` to a staged artifact file.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the write fails.
    fn write_chunk(&self, file: &mut File, chunk: &[u8]) -> io::Result<()> {
        file.write_all(chunk)
    }

    /// Flushes a staged artifact file to stable storage.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the sync fails.
    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    /// Makes the staged file `tmp` visible at `dest` without replacing an existing artifact.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error, `AlreadyExists` when `dest` is already present.
    fn publish(&self, tmp: &Path, dest: &Path) -> io::Result<()> {
        fs::hard_link(tmp, dest)
    }

    /// Flushes the directory entries of `dir` to stable storage.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when `dir` cannot be opened or synced.
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }
}

pub struct OsArtifactIo;

impl ArtifactIo for OsArtifactIo {}

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
    /// Opens the content-addressed store under `artifacts_dir`, staging writes in `tmp_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::Io`] when either directory cannot be created as a private directory.
    pub fn open(artifacts_dir: &Path, tmp_dir: &Path) -> Result<Self, ArtifactError> {
        Self::with_io(artifacts_dir, tmp_dir, Box::new(OsArtifactIo))
    }

    /// Opens the store as [`ArtifactStore::open`] does, with `io` performing the file
    /// operations whose failure the tests inject.
    ///
    /// # Errors
    ///
    /// Fails as [`ArtifactStore::open`] does.
    pub fn with_io(
        artifacts_dir: &Path,
        tmp_dir: &Path,
        io: Box<dyn ArtifactIo>,
    ) -> Result<Self, ArtifactError> {
        let objects = artifacts_dir.join("sha256");
        for dir in [&objects, tmp_dir] {
            ensure_private_dir(dir).map_err(io_err("create_dir"))?;
        }
        Ok(Self {
            objects,
            tmp: tmp_dir.to_path_buf(),
            io,
        })
    }

    #[must_use]
    pub fn path_of(&self, digest: &ArtifactDigest) -> PathBuf {
        let hex = digest.to_hex();
        self.objects.join(&hex[..2]).join(&hex[2..])
    }

    #[must_use]
    pub fn contains(&self, digest: &ArtifactDigest) -> bool {
        fs::symlink_metadata(self.path_of(digest)).is_ok_and(|m| m.is_file())
    }

    /// Stores `bytes` durably and returns their digest.
    ///
    /// # Errors
    ///
    /// Fails as [`ArtifactStore::put_reader`] does.
    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactDigest, ArtifactError> {
        self.put_reader(&mut &bytes[..])
    }

    /// Streams `reader` into the store, durably, and returns the digest of what it read. An
    /// artifact that is already stored is verified rather than rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::Io`] naming the stage (staging, reading the source, writing, syncing,
    /// publishing) that failed, or [`ArtifactError::Corrupt`] when the stored copy of the same digest
    /// no longer matches it.
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
        let shard = dest.parent().unwrap_or(&self.objects);
        match DirBuilder::new().mode(0o700).create(shard) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err("create_dir")(e)),
        }
        // A previous attempt may have created this shard before its fsync failed.
        self.io
            .sync_dir(&self.objects)
            .map_err(io_err("sync_dir"))?;
        match self.io.publish(&tmp_path, &dest) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !self.verify(&digest)? {
                    return Err(ArtifactError::Corrupt);
                }
            }
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

    /// Up to `limit` bytes (at most one page) of an artifact, starting at `offset`.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::NotFound`] when the artifact is not stored, or [`ArtifactError::Io`]
    /// when it cannot be read.
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

    /// The whole artifact, checked against its digest when `verify` is set.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::TooLarge`] when it is bigger than `max_bytes`,
    /// [`ArtifactError::Corrupt`] when verification fails, [`ArtifactError::NotFound`] when it is not
    /// stored, or [`ArtifactError::Io`] when it cannot be read.
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

    /// Whether the stored artifact still hashes to `digest`.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::NotFound`] when it is not stored, or [`ArtifactError::Io`] when it
    /// cannot be read.
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

struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}
