use std::fs;

use super::super::fixtures::{id, path};
use super::*;
use crate::adapters_support::{TempDir, commit_all, init_repo};

#[test]
fn blob_pages_and_tree_listing() {
    let tmp = TempDir::new("blob");
    let repo_dir = init_repo(tmp.path(), "sha256");
    let content: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    fs::create_dir(repo_dir.join("dir")).unwrap();
    fs::write(repo_dir.join("dir/data.bin"), &content).unwrap();
    fs::write(repo_dir.join("dir/*"), "literal star\n").unwrap();
    let commit_hex = commit_all(&repo_dir, "data");
    let repo = Repository::open(&repo_dir).unwrap();
    let commit = id(&repo, &commit_hex);
    let mut assembled = Vec::new();
    let mut offset = 0;
    loop {
        let page = repo
            .read_blob(&commit, &path(b"dir/data.bin"), offset, 1000)
            .unwrap();
        assert_eq!(page.total_bytes, 3000);
        assembled.extend_from_slice(&page.data);
        offset += page.data.len() as u64;
        if page.eof {
            break;
        }
    }
    assert_eq!(assembled, content);
    let past = repo
        .read_blob(&commit, &path(b"dir/data.bin"), 5000, 10)
        .unwrap();
    assert!(past.data.is_empty() && past.eof);
    assert!(matches!(
        repo.read_blob(&commit, &path(b"dir"), 0, 10),
        Err(GitError::NotABlob)
    ));
    let star = repo.read_blob(&commit, &path(b"dir/*"), 0, 100).unwrap();
    assert_eq!(star.data, b"literal star\n");

    let listing = repo.ls_tree(&commit, Some(&path(b"dir")), false).unwrap();
    let names: Vec<_> = listing.iter().map(|e| e.path.display()).collect();
    assert_eq!(names, ["dir/*", "dir/data.bin"]);
    assert_eq!(listing[1].size, Some(3000));
    let all = repo.ls_tree(&commit, None, true).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn grep_is_bounded_and_skips_binary() {
    let tmp = TempDir::new("grep");
    let repo_dir = init_repo(tmp.path(), "sha1");
    for i in 0..30 {
        fs::write(
            repo_dir.join(format!("f{i:02}.txt")),
            format!("first\nxx Needle {i}\n"),
        )
        .unwrap();
    }
    fs::write(repo_dir.join("bin.dat"), b"needle\0binary").unwrap();
    let commit_hex = commit_all(&repo_dir, "grep");
    let repo = Repository::open(&repo_dir).unwrap();
    let commit = id(&repo, &commit_hex);
    let query = |text: &str, ignore_case, max_results| GrepQuery {
        text: text.into(),
        ignore_case,
        max_results,
    };
    let bounded = repo.grep(&commit, &query("Needle", false, 10)).unwrap();
    assert_eq!(bounded.matches.len(), 10);
    assert!(bounded.truncated);
    let first = &bounded.matches[0];
    assert_eq!(first.path.as_bytes(), b"f00.txt");
    assert_eq!((first.line, first.column), (2, 4));
    assert_eq!(first.preview, b"xx Needle 0");

    let all = repo.grep(&commit, &query("needle", true, 200)).unwrap();
    assert_eq!(all.matches.len(), 30, "binary file must be skipped");
    assert!(!all.truncated);
    let none = repo.grep(&commit, &query("needle", false, 10)).unwrap();
    assert!(none.matches.is_empty() && !none.truncated);
    let dash = repo.grep(&commit, &query("-e", false, 10)).unwrap();
    assert!(dash.matches.is_empty());
    assert!(matches!(
        repo.grep(&commit, &query("a\nb", false, 10)),
        Err(GitError::InvalidArgument { .. })
    ));
}

#[test]
fn grep_reports_exact_byte_ranges() {
    let tmp = TempDir::new("grep-offsets");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let text = "αβγ\r\nfirst line\r\nprefix ÉCOLE Needle needle\nlast";
    fs::write(repo_dir.join("u.txt"), text).unwrap();
    let big = "x".repeat(17 << 20) + "\nNeedle\n";
    fs::write(repo_dir.join("big.txt"), &big).unwrap();
    let commit = commit_all(&repo_dir, "offsets");
    let repo = Repository::open(&repo_dir).unwrap();
    let tree = id(&repo, &commit);
    let found = repo
        .grep(
            &tree,
            &GrepQuery {
                text: "needle".into(),
                ignore_case: true,
                max_results: 10,
            },
        )
        .unwrap();
    let hit = found
        .matches
        .iter()
        .find(|m| m.path.as_bytes() == b"u.txt")
        .unwrap();
    let start = text.find("Needle").unwrap();
    assert_eq!(hit.line, 3);
    assert_eq!(hit.byte_start, Some(start as u64));
    assert_eq!(hit.byte_end, Some((start + 6) as u64));
    assert_eq!(&text.as_bytes()[start..start + 6], b"Needle");
    let large = found
        .matches
        .iter()
        .find(|m| m.path.as_bytes() == b"big.txt")
        .unwrap();
    assert_eq!(
        (large.line, large.byte_start, large.byte_end),
        (2, None, None)
    );

    let accented = repo
        .grep(
            &tree,
            &GrepQuery {
                text: "ÉCOLE".into(),
                ignore_case: true,
                max_results: 10,
            },
        )
        .unwrap();
    let start = text.find("ÉCOLE").unwrap();
    assert_eq!(accented.matches.len(), 1);
    assert_eq!(accented.matches[0].byte_start, Some(start as u64));
    assert_eq!(
        accented.matches[0].byte_end,
        Some((start + "ÉCOLE".len()) as u64)
    );
}
