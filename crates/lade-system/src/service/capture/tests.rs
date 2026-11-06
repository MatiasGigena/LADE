use std::fs;

use lade_protocol::client::ReplyData;

use super::*;
use crate::service::convert::parse_uuid;
use crate::service::test_harness::Harness;

#[test]
fn reused_capture_registration_keeps_its_original_identity_after_a_newer_capture() {
    let mut harness = Harness::new("capture-completion-order");
    let (_, oid) = harness.create_objective("Capture completion order");
    let repo = harness.repo.clone();
    let service = &mut harness.service;
    let input = service
        .capture_input(service.obj(oid).unwrap(), repo.clone(), Vec::new())
        .unwrap();
    let original = jobs::capture(&input).unwrap();
    service.register_capture(oid, &original).unwrap();

    // Two independent capture requests can run on the general pool. Hold the
    // reused result while another request captures and registers changed bytes.
    let reused_input = service
        .capture_input(service.obj(oid).unwrap(), repo.clone(), Vec::new())
        .unwrap();
    let reused = jobs::capture(&reused_input).unwrap();
    assert!(reused.reused);
    assert_eq!(reused.capture_id, original.capture_id);
    fs::write(
        repo.join("README"),
        "changed while the first result waits\n",
    )
    .unwrap();
    let changed_input = service
        .capture_input(service.obj(oid).unwrap(), repo, Vec::new())
        .unwrap();
    let changed = jobs::capture(&changed_input).unwrap();
    assert!(!changed.reused);
    let changed_row = service.register_capture(oid, &changed).unwrap();
    assert_ne!(changed_row.tree, reused.tree);

    let reused_row = service.register_capture(oid, &reused).unwrap();
    let ReplyData::ChangesList(list) = service
        .working_change_list(oid, &reused, &reused_row)
        .unwrap()
    else {
        panic!("expected changes")
    };
    let returned_id = CaptureId::new(parse_uuid(list.capture_id.as_ref().unwrap()).unwrap());
    let retained = storage::load_capture(service.store.conn(), returned_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.tree, reused.tree,
        "the capture ID in the reply must resolve to the tree used for its files"
    );
    assert_eq!(retained.target_commit, reused.target);
    assert_eq!(retained.id, original.capture_id);
    assert_eq!(retained.counts, jobs::counts(&reused.files));
}
