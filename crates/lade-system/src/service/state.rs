//! In-memory authoritative state, mirrored write-through into the product store.

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

/// An opened Workspace and its validated repository handle.
#[derive(Clone, Debug)]
pub struct WsState {
    pub id: WorkspaceId,
    pub repo: Repository,
    pub path: PathBuf,
    pub name: String,
    pub default_branch: Option<String>,
}

/// Durable per-Objective state (cloned, modified, committed, then swapped in).
#[derive(Clone, Debug)]
pub struct ObjState {
    pub row: ObjectiveRow,
    pub review: Review,
    /// Non-deleted drafts in creation order.
    pub drafts: Vec<Draft>,
    /// Last computed staleness of unsent drafts against the current basis.
    pub draft_stale: HashMap<DraftId, bool>,
    /// Accepted inputs in acceptance order with their global sequence.
    pub inputs: Vec<(u64, AcceptedInput)>,
    /// Latest working capture.
    pub capture: Option<CaptureRow>,
    /// Retained verification output tails (summary text only).
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

/// Non-durable runtime facts about an Objective.
#[derive(Default)]
pub struct Runtime {
    pub control: Option<Box<dyn WorkerControl>>,
    /// A launch job is in flight for this generation.
    pub launching: Option<u64>,
    /// A long mutating operation holds the Objective (per-Objective sequencing).
    pub busy: Option<&'static str>,
    /// A freeze waiting for the worker to exit.
    pub waiting_freeze: Option<(ReqCtx, Instant)>,
    /// An input dispatch check is in flight; later inputs wait (strict order).
    pub dispatching: bool,
    pub capture_running: bool,
    pub recapture: bool,
}
