use super::*;
use crate::adapters::git;
use crate::adapters_support;
use lade_core::domain::ids::GitAlgorithm;
use lade_protocol::client::{ClientBody, ClientEnvelope, Reply};
use lade_protocol::types::{Id, PageLimit};

#[test]
fn change_lists_with_long_renamed_paths_fit_the_wire_frame() {
    let target = GitId::new(GitAlgorithm::Sha1, &"a".repeat(40)).unwrap();
    let resolved = Resolved {
        old: target.clone(),
        new: target.clone(),
        label: "candidate 1".into(),
        target: TargetRef {
            branch: "main".into(),
            commit: wire_git(&target),
        },
        capture_id: None,
        candidate_id: None,
    };
    for component in [
        b"long-directory".to_vec(),
        [b'"', b'\\', b'\n', 0xff].repeat(3),
    ] {
        let mut prefix = component;
        prefix.push(b'/');
        let prefix = prefix.repeat(200);
        let path = |name: String| {
            let mut bytes = prefix.clone();
            bytes.extend_from_slice(name.as_bytes());
            git::RepoPath::new(bytes).unwrap()
        };
        let files: Vec<git::ChangedFile> = (0..MAX_FILES)
            .map(|index| git::ChangedFile {
                path: path(format!("new-{index}")),
                old_path: Some(path(format!("old-{index}"))),
                status: git::ChangeStatus::Renamed { similarity: 100 },
                old_mode: 0o100_644,
                new_mode: 0o100_644,
                old_id: None,
                new_id: None,
                added: 2,
                removed: 1,
                binary: false,
            })
            .collect();
        let notices = content_notices(
            &files
                .iter()
                .take(64)
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
            &[],
        );
        for notices in [Vec::new(), notices] {
            let list = change_list(&resolved, &files, &[], notices);
            let reply = ClientEnvelope::new(ClientBody::Reply(Reply::ok(
                Id::from_random_bytes([1; 16]),
                ReplyData::ChangesList(list.clone()),
            )));
            lade_protocol::encode(&reply, lade_protocol::Framing::Client)
                .expect("long paths must yield a bounded change list");
            assert!(!list.files.is_empty());
            assert!(list.truncated);
            assert_eq!(list.total_added.value(), (MAX_FILES * 2) as u64);
            assert_eq!(list.total_removed.value(), MAX_FILES as u64);
        }
    }
}

#[test]
fn file_and_content_searches_with_long_paths_fit_the_wire_frame() {
    let tmp = adapters_support::TempDir::new("search-long-paths");
    let dir = adapters_support::init_repo(tmp.path(), "sha1");
    let prefix = "long-directory/".repeat(200);
    std::fs::create_dir_all(dir.join(&prefix)).unwrap();
    for index in 0..200 {
        std::fs::write(dir.join(format!("{prefix}file-{index:03}")), "needle\n").unwrap();
    }
    let commit = adapters_support::commit_all(&dir, "long paths");
    let repo = Repository::open(&dir).unwrap();
    let tree = GitId::new(GitAlgorithm::Sha1, &commit).unwrap();
    for (scope, query) in [
        (SearchScope::Files, "file"),
        (SearchScope::Content, "needle"),
    ] {
        let query = SearchQuery {
            workspace_id: Id::from_random_bytes([1; 16]),
            scope,
            query: query.into(),
            limit: PageLimit::parse("200").unwrap(),
            objective_id: None,
            basis: None,
        };
        let data = search_tree(&repo, &tree, &query, "start commit").unwrap();
        let reply = ClientEnvelope::new(ClientBody::Reply(Reply::ok(
            Id::from_random_bytes([2; 16]),
            data.clone(),
        )));
        lade_protocol::encode(&reply, lade_protocol::Framing::Client)
            .expect("long paths must yield bounded search results");
        let ReplyData::Search(found) = data else {
            panic!("expected search results")
        };
        assert!(!found.results.is_empty());
        assert!(found.truncated);
        assert_eq!(
            found.results[0].path.as_ref().unwrap().as_bytes(),
            format!("{prefix}file-000").as_bytes()
        );
    }
}

#[test]
fn file_search_preserves_non_ascii_query_bytes() {
    let tmp = adapters_support::TempDir::new("search-non-ascii");
    let dir = adapters_support::init_repo(tmp.path(), "sha1");
    std::fs::write(dir.join("ÉCOLE.rs"), "text\n").unwrap();
    let commit = adapters_support::commit_all(&dir, "unicode file name");
    let repo = Repository::open(&dir).unwrap();
    let tree = GitId::new(GitAlgorithm::Sha1, &commit).unwrap();
    let query = SearchQuery {
        workspace_id: Id::from_random_bytes([1; 16]),
        scope: SearchScope::Files,
        query: "École.RS".into(),
        limit: PageLimit::parse("10").unwrap(),
        objective_id: None,
        basis: None,
    };
    let ReplyData::Search(found) = search_tree(&repo, &tree, &query, "start commit").unwrap()
    else {
        panic!("expected search results")
    };
    assert_eq!(found.results.len(), 1);
    assert_eq!(
        found.results[0].path.as_ref().unwrap().as_bytes(),
        "ÉCOLE.rs".as_bytes()
    );
    assert!(!found.truncated);
}
