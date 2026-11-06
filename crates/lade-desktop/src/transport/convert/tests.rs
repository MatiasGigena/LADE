use super::*;
use crate::transport::requests::tests::{C, O, oid, reference, timestamp, wire_detail};

#[test]
fn repository_paths_keep_their_exact_bytes_across_the_wire() {
    let bytes = b"src/\xff.rs".to_vec();
    let wire = to_path(&view::RepoPath {
        bytes: bytes.clone(),
        display: String::new(),
    })
    .unwrap();
    let back = path(&wire);
    assert_eq!(back.bytes, bytes);
    assert!(back.display.contains('\u{fffd}'), "{}", back.display);
    assert_eq!(
        to_path(&view::RepoPath {
            bytes: Vec::new(),
            display: String::new(),
        }),
        Err(Invalid("invalid path".into()))
    );
}

#[test]
fn a_basis_with_a_malformed_candidate_id_is_not_sent() {
    assert_eq!(to_basis(&view::Basis::Working), Ok(wire::Basis::Working));
    assert_eq!(
        to_basis(&view::Basis::Candidate {
            candidate_id: view::CandidateId::new("not-a-uuid"),
        }),
        Err(Invalid("invalid candidate id".into()))
    );
}

#[test]
fn commit_ids_round_trip_and_reject_the_wrong_length() {
    let sha1 = view::GitId {
        algorithm: view::GitAlgorithm::Sha1,
        hex: "a".repeat(40),
    };
    assert_eq!(git_id(&to_git_id(&sha1).unwrap()), sha1);
    let short = view::GitId {
        algorithm: view::GitAlgorithm::Sha256,
        hex: "a".repeat(40),
    };
    assert_eq!(to_git_id(&short), Err(Invalid("invalid commit id".into())));
}

#[test]
fn delivery_modes_and_sides_round_trip() {
    for mode in [
        view::DeliveryMode::Prompt,
        view::DeliveryMode::Steer,
        view::DeliveryMode::FollowUp,
    ] {
        assert_eq!(delivery_mode(to_delivery_mode(mode)), mode);
    }
    for s in [view::Side::Old, view::Side::New] {
        assert_eq!(side(to_side(s)), s);
    }
}

#[test]
fn objective_detail_converts_field_by_field() {
    let detail = detail(&wire_detail(O));
    assert_eq!(detail.summary.objective_id, oid());
    assert_eq!(detail.summary.lifecycle, view::Lifecycle::Active);
    assert_eq!(
        detail.summary.execution.state,
        view::ExecutionState::Running
    );
    assert_eq!(detail.summary.badges, vec![view::Badge::UnsentDrafts]);
    assert_eq!(
        detail.summary.updated_at,
        view::Timestamp(timestamp().unix_ms())
    );
    assert_eq!(detail.integration_target.label(), "main@aaaaaaa");
    assert_eq!(detail.current_candidate().unwrap().ordinal, 1);
    assert_eq!(
        detail.availability(view::ActionName::ReviewIntegrate),
        view::ActionAvailability::Disabled {
            reason: "no valid approval".into()
        }
    );
    // `terminal.open` is kept apart from the Objective actions.
    assert_eq!(detail.available_actions.len(), 2);
    assert_eq!(
        detail.terminal_availability(),
        view::ActionAvailability::Disabled {
            reason: "Start the agent once to create this Objective's worktree.".into()
        }
    );
    assert_eq!(
        detail.session.as_ref().unwrap().model,
        view::ModelRef::new("lade-fixture", "scripted")
    );
}

#[test]
fn references_and_paths_round_trip_losslessly() {
    let r = reference();
    assert_eq!(review::reference(&to_reference(&r).unwrap()), r);
    let raw = view::RepoPath::from_bytes(b"a/\xfe\xff".to_vec());
    let wire_path = to_path(&raw).unwrap();
    assert_eq!(wire_path.as_bytes(), raw.bytes.as_slice());
    assert_eq!(path(&wire_path), raw);
    for basis in [
        view::Basis::Working,
        view::Basis::Candidate {
            candidate_id: view::CandidateId::new(C),
        },
    ] {
        assert!(to_basis(&basis).is_ok());
    }
}
