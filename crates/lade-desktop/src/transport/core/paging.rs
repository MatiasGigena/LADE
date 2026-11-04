//! Paged draft and input history: each reply is checked against the page that requested it,
//! later pages are requested until the cursor runs out, and the merged list is published once.

use std::collections::HashMap;
use std::hash::Hash;

use lade_protocol::client::{
    ErrorCode, Rejection, ReplyData, ReplyResult, Request, RequestFrame, SafeAction,
};
use lade_protocol::types::{Counter, Id};

use super::pages::Pages;
use super::{Core, Step};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::model::ObjectiveId;
use crate::presentation::state::store::StoreUpdate;
use crate::transport::convert::{self, Invalid};
use crate::transport::requests::{self, Context};

/// Takes the loading pages for `objective_id` if `request_id` is the page they wait for.
fn take_page<K, T, C>(
    loading: &mut HashMap<ObjectiveId, Pages<K, T, C>>,
    objective_id: &ObjectiveId,
    request_id: &Id,
) -> Option<Pages<K, T, C>>
where
    K: Clone + Eq + Hash,
{
    if loading.get(objective_id)?.request_id != *request_id {
        return None;
    }
    loading.remove(objective_id)
}

impl Core {
    fn invalid_page(&mut self, context: &Context) -> Step {
        self.settle(
            context,
            ReplyResult::Rejected {
                name: None,
                rejection: Rejection::new(
                    ErrorCode::InvalidRequest,
                    SafeAction::None,
                    "The service returned an invalid history page. Refresh to try again.",
                ),
            },
        )
    }

    /// Sends the request for the page after the one just received, or reports why it
    /// could not be built.
    fn next_page(
        &mut self,
        intent: Intent,
        request: Result<Request, Invalid>,
        context: &Context,
    ) -> Result<RequestFrame, Step> {
        match request {
            Ok(request) => Ok(self.query(Some(intent), request, context.clone())),
            Err(_) => Err(self.invalid_page(context)),
        }
    }

    pub(super) fn draft_page(
        &mut self,
        objective_id: ObjectiveId,
        request_id: &Id,
        result: ReplyResult,
    ) -> Step {
        let Some(mut pages) = take_page(&mut self.draft_pages, &objective_id, request_id) else {
            return Step::default();
        };
        let context = Context::Drafts(objective_id.clone());
        let data = match result {
            ReplyResult::Ok(data) => match *data {
                ReplyData::DraftsList(data) => data,
                _ => return self.invalid_page(&context),
            },
            rejected @ ReplyResult::Rejected { .. } => return self.settle(&context, rejected),
        };
        if data
            .drafts
            .iter()
            .any(|draft| draft.objective_id.as_str() != objective_id.as_str())
        {
            return self.invalid_page(&context);
        }
        if let Some(next) = &data.next_after {
            let advances = pages.after.as_ref().is_none_or(|after| {
                (&next.created_at, &next.draft_id) > (&after.created_at, &after.draft_id)
            });
            if !advances
                || data
                    .drafts
                    .last()
                    .is_none_or(|draft| draft.draft_id != next.draft_id)
            {
                return self.invalid_page(&context);
            }
        }
        pages.page(
            data.drafts
                .iter()
                .map(convert::draft)
                .map(|draft| (draft.draft_id.clone(), draft)),
        );
        if let Some(next) = data.next_after {
            let intent = Intent::LoadDrafts {
                objective_id: objective_id.clone(),
            };
            let request = requests::drafts_page(&objective_id, Some(next.clone()));
            let frame = match self.next_page(intent, request, &context) {
                Ok(frame) => frame,
                Err(step) => return step,
            };
            pages.request_id = frame.request_id.clone();
            pages.after = Some(next);
            self.draft_pages.insert(objective_id, pages);
            return Step {
                updates: Vec::new(),
                frames: vec![frame],
            };
        }
        let mut step = Step {
            updates: vec![StoreUpdate::Drafts {
                objective_id: objective_id.clone(),
                drafts: pages.complete(),
            }],
            frames: Vec::new(),
        };
        self.finish_drafts(&objective_id, None, &mut step);
        step
    }

    pub(super) fn input_page(
        &mut self,
        objective_id: ObjectiveId,
        request_id: &Id,
        result: ReplyResult,
    ) -> Step {
        let Some(mut pages) = take_page(&mut self.input_pages, &objective_id, request_id) else {
            return Step::default();
        };
        let context = Context::Inputs(objective_id.clone());
        let data = match result {
            ReplyResult::Ok(data) => match *data {
                ReplyData::InputsList(data) => data,
                _ => return self.invalid_page(&context),
            },
            rejected @ ReplyResult::Rejected { .. } => return self.settle(&context, rejected),
        };
        if data
            .inputs
            .iter()
            .any(|input| input.objective_id.as_str() != objective_id.as_str())
        {
            return self.invalid_page(&context);
        }
        if let Some(next) = &data.next_after
            && (data.inputs.is_empty()
                || next.value() <= pages.after.as_ref().map_or(0, Counter::value))
        {
            return self.invalid_page(&context);
        }
        pages.page(
            data.inputs
                .iter()
                .map(convert::input)
                .map(|input| (input.input_id.clone(), input)),
        );
        if let Some(next) = data.next_after {
            let intent = Intent::LoadInputs {
                objective_id: objective_id.clone(),
            };
            let request = requests::inputs_page(&objective_id, Some(next.clone()));
            let frame = match self.next_page(intent, request, &context) {
                Ok(frame) => frame,
                Err(step) => return step,
            };
            pages.request_id = frame.request_id.clone();
            pages.after = Some(next);
            self.input_pages.insert(objective_id, pages);
            return Step {
                updates: Vec::new(),
                frames: vec![frame],
            };
        }
        Step {
            updates: vec![StoreUpdate::Inputs {
                objective_id,
                inputs: pages.complete(),
            }],
            frames: Vec::new(),
        }
    }
}
