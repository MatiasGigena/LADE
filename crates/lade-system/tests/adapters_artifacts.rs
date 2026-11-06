#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod adapters_support;

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use adapters_support::TempDir;
use lade_system::adapters::artifacts::{ArtifactDigest, ArtifactError, ArtifactIo, ArtifactStore};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn store(tmp: &TempDir) -> ArtifactStore {
    ArtifactStore::open(&tmp.path().join("artifacts"), &tmp.path().join("tmp")).unwrap()
}

#[test]
fn publishes_content_addressed_private_immutable_files() {
    let tmp = TempDir::new("artifacts");
    let store = store(&tmp);
    let digest = store.put(b"abc").unwrap();
    assert_eq!(
        digest.to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(ArtifactDigest::parse(&digest.to_hex()).unwrap(), digest);
    let path = store.path_of(&digest);
    assert_eq!(
        path,
        tmp.path().join(
            "artifacts/sha256/ba/7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
    );
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&tmp.path().join("artifacts/sha256")), 0o700);
    assert_eq!(mode(&tmp.path().join("tmp")), 0o700);
    assert_eq!(fs::read_dir(tmp.path().join("tmp")).unwrap().count(), 0);

    let inode = fs::metadata(&path).unwrap();
    assert_eq!(store.put(b"abc").unwrap(), digest, "idempotent");
    assert_eq!(
        fs::metadata(&path).unwrap().ino(),
        inode.ino(),
        "not replaced"
    );

    let big: Vec<u8> = (0..300_000u32)
        .map(|i| u8::try_from(i * 7 % 256).unwrap())
        .collect();
    let big_digest = store.put_reader(&mut &big[..]).unwrap();
    assert_eq!(big_digest, ArtifactDigest::of(&big));
    let mut assembled = Vec::new();
    let mut offset = 0;
    loop {
        let page = store.read_page(&big_digest, offset, 65_536).unwrap();
        assert_eq!(page.total_bytes, big.len() as u64);
        assembled.extend_from_slice(&page.data);
        offset += page.data.len() as u64;
        if page.eof {
            break;
        }
    }
    assert_eq!(assembled, big);
    assert_eq!(store.read_all(&big_digest, 1 << 20, true).unwrap(), big);
    assert!(matches!(
        store.read_all(&big_digest, 10, false),
        Err(ArtifactError::TooLarge { .. })
    ));
    assert!(store.verify(&big_digest).unwrap());

    let missing = ArtifactDigest::of(b"missing");
    assert!(!store.contains(&missing));
    assert!(matches!(
        store.read_page(&missing, 0, 10),
        Err(ArtifactError::NotFound)
    ));
    assert!(ArtifactDigest::parse("ABC").is_err());
    assert!(ArtifactDigest::parse(&"G".repeat(64)).is_err());
}

#[test]
fn verification_detects_corruption() {
    let tmp = TempDir::new("artifacts-corrupt");
    let store = store(&tmp);
    let digest = store.put(b"original bytes").unwrap();
    fs::write(store.path_of(&digest), b"tampered bytes").unwrap();
    assert!(!store.verify(&digest).unwrap());
    assert!(matches!(
        store.read_all(&digest, 1024, true),
        Err(ArtifactError::Corrupt)
    ));
    assert_eq!(
        store.read_all(&digest, 1024, false).unwrap(),
        b"tampered bytes"
    );
    assert!(
        matches!(store.put(b"original bytes"), Err(ArtifactError::Corrupt)),
        "put must not confirm publication of a corrupt pre-existing artifact"
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    Write,
    SyncFile,
    Publish,
}

struct FaultyIo(Fault);

impl ArtifactIo for FaultyIo {
    fn write_chunk(&self, file: &mut File, chunk: &[u8]) -> io::Result<()> {
        if self.0 == Fault::Write {
            io::Write::write_all(file, &chunk[..chunk.len() / 2])?;
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        io::Write::write_all(file, chunk)
    }

    fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.0 == Fault::SyncFile {
            return Err(io::Error::other("injected fsync failure"));
        }
        file.sync_all()
    }

    fn publish(&self, tmp: &Path, dest: &Path) -> io::Result<()> {
        if self.0 == Fault::Publish {
            return Err(io::Error::other("injected link failure"));
        }
        fs::hard_link(tmp, dest)
    }
}

#[test]
fn injected_faults_leave_prior_artifacts_and_no_partial_publication() {
    for (fault, stage) in [
        (Fault::Write, "write"),
        (Fault::SyncFile, "sync_file"),
        (Fault::Publish, "publish"),
    ] {
        let tmp = TempDir::new("artifacts-fault");
        let root = tmp.path().join("artifacts");
        let tmp_dir = tmp.path().join("tmp");
        let prior = ArtifactStore::open(&root, &tmp_dir)
            .unwrap()
            .put(b"prior artifact")
            .unwrap();
        let faulty = ArtifactStore::with_io(&root, &tmp_dir, Box::new(FaultyIo(fault))).unwrap();
        let payload = b"new artifact that must not appear";
        match faulty.put(payload) {
            Err(ArtifactError::Io { stage: got, .. }) => assert_eq!(got, stage, "{fault:?}"),
            other => panic!("{fault:?}: expected failure, got {other:?}"),
        }
        let healthy = ArtifactStore::open(&root, &tmp_dir).unwrap();
        let new = ArtifactDigest::of(payload);
        assert!(
            !healthy.contains(&new),
            "{fault:?}: partial artifact published"
        );
        assert!(!healthy.path_of(&new).exists());
        assert_eq!(
            fs::read_dir(&tmp_dir).unwrap().count(),
            0,
            "{fault:?}: temporary file left behind"
        );
        assert_eq!(
            healthy.read_all(&prior, 1024, true).unwrap(),
            b"prior artifact"
        );
        assert_eq!(
            healthy.put(payload).unwrap(),
            new,
            "{fault:?}: retry succeeds"
        );
    }
}

struct SyncDirFault {
    parent: bool,
}

impl ArtifactIo for SyncDirFault {
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        if (dir.file_name().unwrap() == "sha256") == self.parent {
            return Err(io::Error::other("injected directory fsync failure"));
        }
        File::open(dir)?.sync_all()
    }
}

#[test]
fn publication_retries_do_not_bypass_directory_sync_failures() {
    for parent in [true, false] {
        let tmp = TempDir::new("artifact-directory-sync");
        let root = tmp.path().join("artifacts");
        let tmp_dir = tmp.path().join("tmp");
        let faulty =
            ArtifactStore::with_io(&root, &tmp_dir, Box::new(SyncDirFault { parent })).unwrap();
        let payload = b"durable artifact";
        for attempt in 1..=2 {
            assert!(
                matches!(
                    faulty.put(payload),
                    Err(ArtifactError::Io {
                        stage: "sync_dir",
                        ..
                    })
                ),
                "attempt {attempt} bypassed failed fsync (parent={parent})"
            );
        }
        let healthy = ArtifactStore::open(&root, &tmp_dir).unwrap();
        assert_eq!(healthy.put(payload).unwrap(), ArtifactDigest::of(payload));
        assert_eq!(
            healthy
                .read_all(&ArtifactDigest::of(payload), 1024, true)
                .unwrap(),
            payload
        );
    }
}
