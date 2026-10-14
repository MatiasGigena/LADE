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

#[derive(Default)]
pub struct Runtime {
    pub control: Option<Box<dyn WorkerControl>>,
    pub launching: Option<u64>,
    pub busy: Option<&'static str>,
    pub waiting_freeze: Option<(ReqCtx, Instant)>,
    pub dispatching: bool,
    pub capture_running: bool,
    pub recapture: bool,
}
