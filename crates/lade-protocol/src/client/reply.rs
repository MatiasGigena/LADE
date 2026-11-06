//! Replies, rejections and the recorded outcome of a mutating request.

use super::{ErrorCode, ReplyData, RequestName, SafeAction};
use crate::{
    WireError,
    types::{Id, truncate_chars},
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

pub const MAX_DETAIL_CHARS: usize = 512;
pub const MAX_STALE_DRAFTS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rejection {
    pub code: ErrorCode,
    pub action: SafeAction,
    /// Human-readable reason; never a code or an identifier list.
    pub detail: String,
    /// For `stale_reference` from a feedback batch: the drafts whose anchors no longer match.
    pub stale_drafts: Vec<Id>,
}
impl Rejection {
    #[must_use]
    pub fn new(code: ErrorCode, action: SafeAction, detail: &str) -> Self {
        Self {
            code,
            action,
            detail: truncate_chars(detail, MAX_DETAIL_CHARS).to_owned(),
            stale_drafts: Vec::new(),
        }
    }
    #[must_use]
    pub fn with_stale_drafts(mut self, mut drafts: Vec<Id>) -> Self {
        drafts.truncate(MAX_STALE_DRAFTS);
        self.stale_drafts = drafts;
        self
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum RawResult {
    Ok {
        data: Value,
    },
    Rejected {
        code: ErrorCode,
        action: SafeAction,
        detail: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stale_drafts: Vec<Id>,
    },
}
fn raw_result(data: Result<&ReplyData, &Rejection>) -> Result<RawResult, WireError> {
    Ok(match data {
        Ok(data) => RawResult::Ok {
            data: data.data_value()?,
        },
        Err(r) => RawResult::Rejected {
            code: r.code,
            action: r.action,
            detail: r.detail.clone(),
            stale_drafts: r.stale_drafts.clone(),
        },
    })
}
fn typed_result(
    name: RequestName,
    raw: RawResult,
) -> Result<Result<ReplyData, Rejection>, WireError> {
    Ok(match raw {
        RawResult::Ok { data } => Ok(ReplyData::from_parts(name, data)?),
        RawResult::Rejected {
            code,
            action,
            detail,
            stale_drafts,
        } => Err(Rejection {
            code,
            action,
            detail,
            stale_drafts,
        }),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReplyResult {
    Ok(Box<ReplyData>),
    Rejected {
        name: Option<RequestName>,
        rejection: Rejection,
    },
}
impl ReplyResult {
    #[must_use]
    pub fn name(&self) -> Option<RequestName> {
        match self {
            Self::Ok(data) => Some(data.name()),
            Self::Rejected { name, .. } => *name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawReply")]
pub struct Reply {
    pub request_id: Id,
    pub result: ReplyResult,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReply {
    request_id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<RequestName>,
    result: RawResult,
}
impl Reply {
    #[must_use]
    pub fn ok(request_id: Id, data: ReplyData) -> Self {
        Self {
            request_id,
            result: ReplyResult::Ok(Box::new(data)),
        }
    }
    #[must_use]
    pub fn rejected(request_id: Id, name: Option<RequestName>, rejection: Rejection) -> Self {
        Self {
            request_id,
            result: ReplyResult::Rejected { name, rejection },
        }
    }
    #[must_use]
    pub fn outcome(&self) -> Option<Outcome> {
        match &self.result {
            ReplyResult::Ok(data) if data.name().is_mutating() => Some(Outcome::Ok(data.clone())),
            ReplyResult::Rejected {
                name: Some(name),
                rejection,
            } if name.is_mutating() => Some(Outcome::Rejected {
                name: *name,
                rejection: rejection.clone(),
            }),
            _ => None,
        }
    }
}
impl TryFrom<RawReply> for Reply {
    type Error = WireError;
    fn try_from(raw: RawReply) -> Result<Self, WireError> {
        let result = match (raw.name, raw.result) {
            (Some(name), result) => match typed_result(name, result)? {
                Ok(data) => ReplyResult::Ok(Box::new(data)),
                Err(rejection) => ReplyResult::Rejected {
                    name: Some(name),
                    rejection,
                },
            },
            (
                None,
                RawResult::Rejected {
                    code,
                    action,
                    detail,
                    stale_drafts,
                },
            ) => ReplyResult::Rejected {
                name: None,
                rejection: Rejection {
                    code,
                    action,
                    detail,
                    stale_drafts,
                },
            },
            (None, RawResult::Ok { .. }) => return Err(WireError::InvalidSchema),
        };
        Ok(Self {
            request_id: raw.request_id,
            result,
        })
    }
}
impl TryFrom<&Reply> for RawReply {
    type Error = WireError;
    fn try_from(reply: &Reply) -> Result<Self, WireError> {
        let (name, result) = match &reply.result {
            ReplyResult::Ok(data) => (Some(data.name()), raw_result(Ok(data))?),
            ReplyResult::Rejected { name, rejection } => (*name, raw_result(Err(rejection))?),
        };
        Ok(Self {
            request_id: reply.request_id.clone(),
            name,
            result,
        })
    }
}
impl Serialize for Reply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RawReply::try_from(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawOutcome")]
pub enum Outcome {
    Ok(Box<ReplyData>),
    Rejected {
        name: RequestName,
        rejection: Rejection,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutcome {
    name: RequestName,
    result: RawResult,
}
impl TryFrom<RawOutcome> for Outcome {
    type Error = WireError;
    fn try_from(raw: RawOutcome) -> Result<Self, WireError> {
        if !raw.name.is_mutating() {
            return Err(WireError::InvalidSchema);
        }
        Ok(match typed_result(raw.name, raw.result)? {
            Ok(data) => Self::Ok(Box::new(data)),
            Err(rejection) => Self::Rejected {
                name: raw.name,
                rejection,
            },
        })
    }
}
impl TryFrom<&Outcome> for RawOutcome {
    type Error = WireError;
    fn try_from(outcome: &Outcome) -> Result<Self, WireError> {
        Ok(match outcome {
            Outcome::Ok(data) => Self {
                name: data.name(),
                result: raw_result(Ok(data))?,
            },
            Outcome::Rejected { name, rejection } => Self {
                name: *name,
                result: raw_result(Err(rejection))?,
            },
        })
    }
}
impl Serialize for Outcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RawOutcome::try_from(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fixtures::{id, round_trip};
    use crate::client::{ClientBody, Empty, OperationState, OperationStatus};
    use serde_json::json;

    fn rejection() -> Rejection {
        Rejection::new(
            ErrorCode::Conflict,
            SafeAction::Refresh,
            "The draft changed.",
        )
    }

    fn reply_json(reply: &Reply) -> Value {
        round_trip(&ClientBody::Reply(reply.clone()), "Reply")
    }

    #[test]
    fn rejection_details_and_stale_drafts_are_bounded() {
        let long = Rejection::new(ErrorCode::NotFound, SafeAction::None, &"é".repeat(600));
        assert_eq!(long.detail.chars().count(), MAX_DETAIL_CHARS);
        assert_eq!(
            Rejection::new(ErrorCode::NotFound, SafeAction::None, "short").detail,
            "short"
        );
        let drafts: Vec<Id> = (0..=255).chain(0..10).map(id).collect();
        let stale = rejection().with_stale_drafts(drafts.clone());
        assert_eq!(stale.stale_drafts, drafts[..MAX_STALE_DRAFTS]);
    }

    #[test]
    fn an_ok_reply_names_its_request_and_types_its_data() {
        let reply = Reply::ok(id(1), ReplyData::DraftDelete(Empty {}));
        assert_eq!(reply.result.name(), Some(RequestName::DraftDelete));
        let json = reply_json(&reply);
        assert_eq!(json["name"], json!("draft.delete"));
        assert_eq!(json["result"], json!({"status": "ok", "data": {}}));
        let mut wrong = json.clone();
        wrong["result"]["data"] = json!({"unexpected": true});
        assert!(serde_json::from_value::<ClientBody>(wrong).is_err());
    }

    #[test]
    fn a_rejection_may_omit_the_name_but_data_may_not() {
        let reply = Reply::rejected(id(1), None, rejection());
        let json = reply_json(&reply);
        assert!(json.get("name").is_none());
        assert!(json["result"].get("stale_drafts").is_none());
        assert_eq!(reply.result.name(), None);
        let unnamed_ok = json!({
            "request_id": id(1).as_str(),
            "result": {"status": "ok", "data": {}}
        });
        assert!(serde_json::from_value::<Reply>(unnamed_ok).is_err());
        let stale = Reply::rejected(
            id(1),
            Some(RequestName::FeedbackSubmit),
            rejection().with_stale_drafts(vec![id(2)]),
        );
        assert_eq!(
            reply_json(&stale)["result"]["stale_drafts"],
            json!([id(2).as_str()])
        );
    }

    #[test]
    fn only_mutating_requests_record_an_outcome() {
        let read = Reply::ok(
            id(1),
            ReplyData::OperationGet(OperationStatus {
                state: OperationState::Unknown,
                outcome: None,
            }),
        );
        assert_eq!(read.outcome(), None);
        assert_eq!(
            Reply::rejected(id(1), Some(RequestName::Snapshot), rejection()).outcome(),
            None
        );
        assert_eq!(Reply::rejected(id(1), None, rejection()).outcome(), None);
        let deleted = Reply::ok(id(1), ReplyData::DraftDelete(Empty {}));
        assert_eq!(
            deleted.outcome(),
            Some(Outcome::Ok(Box::new(ReplyData::DraftDelete(Empty {}))))
        );
        let refused = Reply::rejected(id(1), Some(RequestName::DraftDelete), rejection());
        let outcome = refused.outcome().expect("mutating");
        assert_eq!(
            outcome,
            Outcome::Rejected {
                name: RequestName::DraftDelete,
                rejection: rejection()
            }
        );
        round_trip(&outcome, "Outcome");
    }

    #[test]
    fn an_outcome_of_a_read_is_refused() {
        let read = json!({"name": "snapshot", "result": {
            "status": "rejected", "code": "conflict", "action": "refresh", "detail": "x"
        }});
        assert!(serde_json::from_value::<Outcome>(read).is_err());
        let ok = round_trip(
            &Outcome::Ok(Box::new(ReplyData::DraftDelete(Empty {}))),
            "Outcome",
        );
        assert_eq!(
            ok,
            json!({"name": "draft.delete", "result": {"status": "ok", "data": {}}})
        );
    }
}
