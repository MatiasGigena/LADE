//! Request mapping for the transport: which wire request an [`Intent`] sends, the [`Context`]
//! kept to interpret its reply, and the [`Effects`] of that reply or of its rejection.

use lade_protocol::client::{self as wire, DraftCursor, Request};
use lade_protocol::types::{Counter, GitId, PageLimit, ReadLimit};

use super::convert::{Invalid, id};
use crate::presentation::controller::intents::{Intent, StartPoint};
use crate::presentation::state::model::{
    self as view, ActionSurface, Basis, DraftId, ObjectiveId, RepoPath, SearchScope, Side,
};
use crate::presentation::state::store::StoreUpdate;

mod effects;
mod mapping;
mod rejection;

pub use effects::{recovered_effects, reply_effects};
pub use mapping::request;
pub use rejection::{rejection_effects, rejection_message};

pub const ACTIVITY_PAGE: u32 = 100;
pub const SEARCH_LIMIT: u32 = 100;
pub const READ_WINDOW: u32 = 262_144;
/// Drafts or inputs requested per history page.
const HISTORY_PAGE: u32 = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    Snapshot,
    OpenWorkspace,
    Objective(ObjectiveId),
    Activity(ObjectiveId),
    Changes(ObjectiveId, Basis),
    Diff(ObjectiveId, Basis, RepoPath),
    File(ObjectiveId, Basis, RepoPath, Side),
    Drafts(ObjectiveId),
    Inputs(ObjectiveId),
    Search {
        scope: SearchScope,
        query: String,
        objective_id: Option<ObjectiveId>,
        basis: Option<Basis>,
    },
    Models(Option<String>),
    Tree(ObjectiveId),
    Resources,
    Terminal {
        label: &'static str,
        objective_id: ObjectiveId,
    },
    Stop,
    /// Deleting this draft. A delete names no objective; the draft's own state carries it.
    DraftDelete {
        label: &'static str,
        draft_id: DraftId,
    },
    Command {
        label: &'static str,
        objective_id: Option<ObjectiveId>,
        /// The draft this command saves, when it is a draft save.
        saved_draft: Option<DraftId>,
        /// The drafts this command submits as feedback.
        batch: Vec<DraftId>,
        /// Where the command was issued from, so its refusal is shown there.
        surface: Option<ActionSurface>,
    },
}

impl Context {
    fn command(
        label: &'static str,
        objective_id: Option<&ObjectiveId>,
        surface: Option<ActionSurface>,
    ) -> Self {
        Self::Command {
            label,
            objective_id: objective_id.cloned(),
            saved_draft: None,
            batch: Vec::new(),
            surface,
        }
    }

    pub fn objective_id(&self) -> Option<&ObjectiveId> {
        match self {
            Self::Snapshot
            | Self::OpenWorkspace
            | Self::Search { .. }
            | Self::Models(_)
            | Self::Resources
            | Self::DraftDelete { .. }
            | Self::Stop => None,
            Self::Tree(o)
            | Self::Terminal {
                objective_id: o, ..
            }
            | Self::Objective(o)
            | Self::Activity(o)
            | Self::Changes(o, _)
            | Self::Diff(o, ..)
            | Self::File(o, ..)
            | Self::Drafts(o)
            | Self::Inputs(o) => Some(o),
            Self::Command { objective_id, .. } => objective_id.as_ref(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Snapshot => "Load service state",
            Self::OpenWorkspace => "Open workspace",
            Self::Objective(_) => "Load Objective",
            Self::Activity(_) => "Load activity",
            Self::Changes(..) => "Load Change Set",
            Self::Diff(..) => "Load diff",
            Self::File(..) => "Open file",
            Self::Drafts(_) => "Load drafts",
            Self::Inputs(_) => "Load inputs",
            Self::Search { .. } => "Search",
            Self::Models(_) => "Load models",
            Self::Tree(_) => "Load session tree",
            Self::Resources => "Load profile resources",
            Self::Command { label, .. }
            | Self::Terminal { label, .. }
            | Self::DraftDelete { label, .. } => label,
            Self::Stop => "Exit and stop the service",
        }
    }

    /// The label as a verb phrase, for "Couldn't …" sentences.
    pub fn action(&self) -> String {
        let mut chars = self.label().chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_lowercase().chain(chars).collect()
        })
    }
}

fn selector(objective_id: &ObjectiveId) -> Result<wire::ObjectiveSelector, Invalid> {
    Ok(wire::ObjectiveSelector {
        objective_id: id(objective_id.as_str(), "objective id")?,
    })
}

fn candidate(
    objective_id: &ObjectiveId,
    candidate_id: &view::CandidateId,
) -> Result<wire::CandidateCommand, Invalid> {
    Ok(wire::CandidateCommand {
        objective_id: id(objective_id.as_str(), "objective id")?,
        candidate_id: id(candidate_id.as_str(), "candidate id")?,
    })
}

/// The page limits are constants inside the protocol's range; a failure here is reported like
/// any other request the window could not build.
fn limit(value: u32) -> Result<PageLimit, Invalid> {
    PageLimit::parse(value.to_string())
        .map_err(|_| Invalid(format!("page limit {value} is out of range")))
}

fn read_limit() -> Result<ReadLimit, Invalid> {
    ReadLimit::parse(READ_WINDOW.to_string())
        .map_err(|_| Invalid(format!("read limit {READ_WINDOW} is out of range")))
}

/// One page of an objective's drafts, starting after `after` (or at the start).
pub fn drafts_page(
    objective_id: &ObjectiveId,
    after: Option<DraftCursor>,
) -> Result<Request, Invalid> {
    Ok(Request::DraftsList(wire::DraftsListQuery {
        objective_id: id(objective_id.as_str(), "objective id")?,
        limit: limit(HISTORY_PAGE)?,
        after,
    }))
}

/// One page of an objective's accepted inputs, starting after `after` (or at the start).
pub fn inputs_page(objective_id: &ObjectiveId, after: Option<Counter>) -> Result<Request, Invalid> {
    Ok(Request::InputsList(wire::InputsListQuery {
        objective_id: id(objective_id.as_str(), "objective id")?,
        limit: limit(HISTORY_PAGE)?,
        after,
    }))
}

pub fn start_point(start: &StartPoint) -> Result<wire::Start, Invalid> {
    match start {
        StartPoint::Target => Ok(wire::Start::Target),
        StartPoint::Commit(hex) => {
            let hex = hex.trim().to_ascii_lowercase();
            if !matches!(hex.len(), 40 | 64) {
                return Err(Invalid(
                    "start commit must be 40 or 64 hex characters".into(),
                ));
            }
            GitId::from_hex(&hex)
                .map(|commit| wire::Start::Commit { commit })
                .map_err(|_| Invalid("start commit is not hexadecimal".into()))
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub updates: Vec<StoreUpdate>,
    pub follow_ups: Vec<Intent>,
}

fn refresh_objective(objective_id: &ObjectiveId) -> Intent {
    Intent::LoadObjective {
        objective_id: objective_id.clone(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
