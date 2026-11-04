//! Conversions between the service protocol and the view model, and the checks that keep a
//! view value the protocol would refuse from being sent.

use lade_protocol::client as wire;
use lade_protocol::types::{self, Counter, Id, WirePath};

use crate::presentation::state::model as view;

mod notices;
mod objective;
mod review;
#[cfg(test)]
mod tests;

pub use notices::{advice, error_code, fallback_reason, notice};
pub use objective::{detail, settings, summary, verification, workspace};
pub use review::{
    activity_item, change_list, draft, file_diff, file_page, input, search_result, terminal_end,
    to_reference,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid(pub String);

impl Invalid {
    pub(super) fn new(what: &str) -> Self {
        Self(format!("invalid {what}"))
    }
}

pub fn timestamp(t: &types::Timestamp) -> view::Timestamp {
    view::Timestamp(t.unix_ms())
}

pub fn counter(c: &Counter) -> u64 {
    c.value()
}

pub fn id(value: &str, what: &str) -> Result<Id, Invalid> {
    Id::parse(value).map_err(|_| Invalid::new(what))
}

pub fn git_id(g: &types::GitId) -> view::GitId {
    view::GitId {
        algorithm: match g.algorithm() {
            types::GitAlgorithm::Sha1 => view::GitAlgorithm::Sha1,
            types::GitAlgorithm::Sha256 => view::GitAlgorithm::Sha256,
        },
        hex: g.hex().to_owned(),
    }
}

pub fn to_git_id(g: &view::GitId) -> Result<types::GitId, Invalid> {
    let algorithm = match g.algorithm {
        view::GitAlgorithm::Sha1 => types::GitAlgorithm::Sha1,
        view::GitAlgorithm::Sha256 => types::GitAlgorithm::Sha256,
    };
    types::GitId::new(algorithm, g.hex.clone()).map_err(|_| Invalid::new("commit id"))
}

pub fn path(p: &WirePath) -> view::RepoPath {
    view::RepoPath {
        bytes: p.as_bytes().to_vec(),
        display: p.display().to_owned(),
    }
}

pub fn to_path(p: &view::RepoPath) -> Result<WirePath, Invalid> {
    WirePath::from_bytes(p.bytes.clone()).map_err(|_| Invalid::new("path"))
}

pub fn model(m: &types::Model) -> view::ModelRef {
    view::ModelRef::new(m.provider.clone(), m.id.clone())
}

pub fn to_model(m: &view::ModelRef) -> types::Model {
    types::Model {
        provider: m.provider.clone(),
        id: m.id.clone(),
    }
}

pub fn delivery_mode(m: types::DeliveryMode) -> view::DeliveryMode {
    match m {
        types::DeliveryMode::Prompt => view::DeliveryMode::Prompt,
        types::DeliveryMode::Steer => view::DeliveryMode::Steer,
        types::DeliveryMode::FollowUp => view::DeliveryMode::FollowUp,
    }
}

pub fn to_delivery_mode(m: view::DeliveryMode) -> types::DeliveryMode {
    match m {
        view::DeliveryMode::Prompt => types::DeliveryMode::Prompt,
        view::DeliveryMode::Steer => types::DeliveryMode::Steer,
        view::DeliveryMode::FollowUp => types::DeliveryMode::FollowUp,
    }
}

pub fn side(s: wire::Side) -> view::Side {
    match s {
        wire::Side::Old => view::Side::Old,
        wire::Side::New => view::Side::New,
    }
}

pub fn to_side(s: view::Side) -> wire::Side {
    match s {
        view::Side::Old => wire::Side::Old,
        view::Side::New => wire::Side::New,
    }
}

pub fn to_scope(s: view::SearchScope) -> wire::SearchScope {
    match s {
        view::SearchScope::Objectives => wire::SearchScope::Objectives,
        view::SearchScope::Files => wire::SearchScope::Files,
        view::SearchScope::Content => wire::SearchScope::Content,
    }
}

pub fn to_basis(b: &view::Basis) -> Result<wire::Basis, Invalid> {
    Ok(match b {
        view::Basis::Working => wire::Basis::Working,
        view::Basis::Candidate { candidate_id } => wire::Basis::Candidate {
            candidate_id: id(candidate_id.as_str(), "candidate id")?,
        },
        view::Basis::Since {
            from_candidate_id,
            to_candidate_id,
        } => wire::Basis::Since {
            from_candidate_id: id(from_candidate_id.as_str(), "candidate id")?,
            to_candidate_id: id(to_candidate_id.as_str(), "candidate id")?,
        },
    })
}
