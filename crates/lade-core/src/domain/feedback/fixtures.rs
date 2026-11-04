//! Shared fixtures for the feedback tests: a file with multi-byte and CRLF content, drafts anchored
//! into it, and an acceptance addressed to an Objective's current session.

use super::{Acceptance, Draft, InputMode, SaveDraft, save_draft};
use crate::domain::ids::Revision;
use crate::domain::inspection::{ArtifactDigest, CurrentContent, Reference};
use crate::domain::objectives::Objective;
use crate::testing::{draft_id, input_id, operation_id, reference_at, ts};

pub(super) const FILE: &str = "fn main() {\r\n\tlet é = 1;\r\n\tprintln!(\"{é}\");\r\n}\r\n";

pub(super) fn reference_for(selection: &str) -> Reference {
    let start = FILE.find(selection).expect("selection in file");
    reference_at(1, start, start + selection.len(), selection).expect("reference")
}

pub(super) fn save_request(
    objective: &Objective,
    id: u128,
    expected: u64,
    body: &str,
) -> SaveDraft {
    SaveDraft {
        draft_id: draft_id(id),
        objective_id: objective.id(),
        expected_revision: Revision::new(expected),
        reference: reference_for("\tlet é"),
        body: body.to_string(),
    }
}

pub(super) fn save_new(objective: &Objective, id: u128, selection: &str, body: &str) -> Draft {
    let request = SaveDraft {
        reference: reference_for(selection),
        ..save_request(objective, id, 0, body)
    };
    save_draft(None, objective, request, ts(3)).expect("create draft")
}

pub(super) fn acceptance(objective: &Objective) -> Acceptance {
    let session = objective.execution().session().expect("session");
    Acceptance {
        input_id: input_id(1),
        operation_id: operation_id(1),
        session_id: session.session_id,
        association_revision: session.association_revision,
        mode: InputMode::Steer,
    }
}

pub(super) fn present<'a>(digest: &'a ArtifactDigest, bytes: &'a [u8]) -> CurrentContent<'a> {
    CurrentContent::Present { digest, bytes }
}
