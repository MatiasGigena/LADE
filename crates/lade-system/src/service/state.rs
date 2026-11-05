use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use lade_core::domain::feedback::{AcceptedInput, Draft};
use lade_core::domain::ids::{DraftId, InputId, VerificationId, WorkspaceId};
use lade_core::domain::objectives::Objective;
use lade_core::domain::review::Review;

use crate::adapters::git::Repository;
use crate::storage::{CaptureRow, ObjectiveRow};
use crate::supervision::WorkerControl;

use super::ReqCtx;

#[derive(Clone, Debug)]
pub struct WsState {
    pub id: WorkspaceId,
    pub repo: Repository,
    pub path: PathBuf,
    pub name: String,
    pub default_branch: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ObjState {
    pub row: ObjectiveRow,
    pub review: Review,
    pub drafts: Vec<Draft>,
    pub draft_stale: HashMap<DraftId, bool>,
    pub inputs: Vec<(u64, AcceptedInput)>,
    pub capture: Option<CaptureRow>,
    pub tails: HashMap<VerificationId, String>,
}

impl ObjState {
    pub fn objective(&self) -> &Objective {
        &self.row.objective
    }

    pub fn objective_mut(&mut self) -> &mut Objective {
        &mut self.row.objective
    }

    pub fn input_mut(&mut self, id: InputId) -> Option<&mut AcceptedInput> {
        self.inputs.iter_mut().map(|(_, i)| i).find(|i| i.id == id)
    }

    pub fn draft(&self, id: DraftId) -> Option<&Draft> {
        self.drafts.iter().find(|d| d.id() == id)
    }

    pub fn unsent_drafts(&self) -> usize {
        self.drafts.iter().filter(|d| d.is_unsent()).count()
    }
}

/// The deferred operation an Objective is running; while one runs, other changes to it are
/// refused as busy. `Freeze` also lets a read-only terminal stay open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Freeze,
    Integrate,
    Refresh,
    Complete,
    Cleanup,
    DeleteHistory,
    SessionReload,
}

#[derive(Default)]
pub struct Runtime {
    pub control: Option<Box<dyn WorkerControl>>,
    pub launching: Option<u64>,
    pub busy: Option<Busy>,
    pub waiting_freeze: Option<(ReqCtx, Instant)>,
    pub dispatching: bool,
    pub capture_running: bool,
    pub recapture: bool,
}

#[cfg(test)]
mod tests {
    use lade_core::Revision;
    use lade_core::domain::feedback::{DeliveryState, DraftRecord, DraftState, InputMode};
    use lade_core::domain::ids::{
        CaptureId, InputId, ObjectiveId, OperationId, SessionId, Timestamp, Uuid,
    };
    use lade_core::domain::inspection::{
        ArtifactDigest, ByteRange, InspectedVersion, Reference, RepoPath, Side,
    };

    use super::*;
    use crate::service::test_harness::Harness;

    fn id(n: u8) -> Uuid {
        Uuid::from_random_bytes([n; 16])
    }

    fn draft(oid: ObjectiveId, n: u8, state: DraftState) -> Draft {
        let reference = Reference::new(
            ArtifactDigest::parse(&"0".repeat(64)).expect("digest"),
            RepoPath::new(b"src/lib.rs".to_vec()).expect("path"),
            InspectedVersion::Working {
                capture_id: CaptureId::new(id(90)),
            },
            Side::New,
            ByteRange::new(0, 2).expect("range"),
            "fn".into(),
            1,
        )
        .expect("reference");
        Draft::restore(DraftRecord {
            id: DraftId::new(id(n)),
            objective_id: oid,
            destination_session_id: SessionId::new(id(80)),
            reference,
            body: "comment".into(),
            revision: Revision::INITIAL,
            state,
            created_at: Timestamp::from_unix_ms(0),
            updated_at: Timestamp::from_unix_ms(0),
        })
        .expect("draft")
    }

    fn input(oid: ObjectiveId, n: u8) -> AcceptedInput {
        AcceptedInput {
            id: InputId::new(id(n)),
            operation_id: OperationId::new(id(n + 100)),
            objective_id: oid,
            session_id: SessionId::new(id(80)),
            association_revision: Revision::INITIAL,
            mode: InputMode::Prompt,
            items: Vec::new(),
            note: None,
            rendered_text: format!("input {n}"),
            accepted_at: Timestamp::from_unix_ms(0),
            delivery: DeliveryState::Queued,
        }
    }

    #[test]
    fn objective_state_finds_its_drafts_and_inputs_and_counts_unsent_drafts() {
        let mut harness = Harness::new("state-lookups");
        let (_, oid) = harness.create_objective("State");
        let mut st = harness.service.objectives[&oid].clone();
        let submitted = DraftState::Submitted {
            input_id: InputId::new(id(1)),
        };
        st.drafts = vec![
            draft(oid, 10, DraftState::Unsent),
            draft(oid, 11, submitted),
            draft(oid, 12, DraftState::Unsent),
        ];
        st.inputs = vec![(1, input(oid, 1)), (2, input(oid, 2))];
        assert_eq!(st.unsent_drafts(), 2);
        assert_eq!(
            st.draft(DraftId::new(id(11))).map(Draft::id),
            Some(DraftId::new(id(11)))
        );
        assert!(st.draft(DraftId::new(id(99))).is_none());
        st.input_mut(InputId::new(id(2)))
            .expect("input")
            .rendered_text = "edited".into();
        assert_eq!(st.inputs[1].1.rendered_text, "edited");
        assert!(st.input_mut(InputId::new(id(3))).is_none());
        assert_eq!(st.objective().id(), oid);
        assert!(st.objective_mut().execution().session().is_none());
    }
}
