//! Drafts, accepted input and feedback: draft save/delete and listing, messages to the Agent
//! Session, and feedback submission after a fresh working capture.

use std::collections::HashMap;

use lade_core::Revision;
use lade_core::domain::feedback::{
    Acceptance, BatchEntry, DraftState, SaveDraft, accept_feedback, accept_message, save_draft,
};
use lade_core::domain::ids::{DraftId, InputId, ObjectiveId, OperationId, SessionId};
use lade_core::domain::inspection::{
    ArtifactDigest as CoreDigest, CurrentContent, InspectedVersion, Reference,
};
use lade_protocol::client::{
    ClientBody, ClientEnvelope, Draft as WireDraft, DraftCursor, DraftDeleteCommand, DraftList,
    DraftSaveCommand, DraftState as WireDraftState, DraftsListQuery, Empty, ErrorCode, Event,
    EventFrame, FeedbackSubmitCommand, InputList, InputsListQuery, MessageSendCommand,
    OperationState, OperationStatus, Outcome, Rejection, Reply, ReplyData, SafeAction,
};
use lade_protocol::types::{Counter, Id};
use lade_protocol::{Framing, MAX_FRAME, encode, json_len};

use crate::adapters::artifacts::ArtifactDigest;
use crate::storage;

use super::capture::CapturePurpose;
use super::convert::{
    artifact_rejection, conflict, core_mode, core_reference, domain_rejection, durability_failure,
    invalid, not_found, parse_uuid, wire_draft, wire_input,
};
use super::handlers::sql;
use super::jobs::{self, CaptureOut};
use super::state::ObjState;
use super::{Completed, Flow, ReqCtx, Service};
use crate::fault::{self, Fault};
use crate::support::{new_uuid, now_ms, ts_now, wire_ts};

fn reply_json_len(request_id: &Id, data: ReplyData) -> usize {
    json_len(&ClientEnvelope::new(ClientBody::Reply(Reply::ok(
        request_id.clone(),
        data,
    ))))
}

fn history_too_large() -> Rejection {
    Rejection::new(
        ErrorCode::LimitReached,
        SafeAction::None,
        "This saved record exceeds the reply limit and cannot be shown.",
    )
}

impl Service {
    pub(super) fn drafts_list(
        &self,
        request_id: &Id,
        query: &DraftsListQuery,
    ) -> Result<ReplyData, Rejection> {
        let st = self.obj(self.objective_id(&query.objective_id)?)?;
        let after = query
            .after
            .as_ref()
            .map(|cursor| {
                parse_uuid(&cursor.draft_id)
                    .map(|id| (cursor.created_at.unix_ms(), DraftId::new(id)))
            })
            .transpose()?;
        let mut eligible: Vec<_> = st
            .drafts
            .iter()
            .filter(|draft| {
                after.is_none_or(|key| (draft.record().created_at.unix_ms(), draft.id()) > key)
            })
            .collect();
        eligible.sort_by_key(|draft| (draft.record().created_at.unix_ms(), draft.id()));
        let mut drafts = Vec::new();
        let mut content_bytes: usize = 0;
        let mut last = None;
        for (index, draft) in eligible
            .iter()
            .enumerate()
            .take(query.limit.value() as usize)
        {
            let wire = wire_draft(st, draft);
            let cursor = DraftCursor {
                created_at: wire_ts(draft.record().created_at.unix_ms()),
                draft_id: wire.draft_id.clone(),
            };
            let more = index + 1 < eligible.len();
            let overhead = reply_json_len(
                request_id,
                ReplyData::DraftsList(DraftList {
                    drafts: Vec::new(),
                    next_after: more.then_some(cursor.clone()),
                }),
            );
            let next_content = content_bytes
                .saturating_add(usize::from(!drafts.is_empty()))
                .saturating_add(json_len(&wire));
            if overhead.saturating_add(next_content) > MAX_FRAME {
                break;
            }
            drafts.push(wire);
            content_bytes = next_content;
            last = Some(cursor);
        }
        if drafts.is_empty() && !eligible.is_empty() {
            return Err(history_too_large());
        }
        let next_after = (drafts.len() < eligible.len()).then_some(last).flatten();
        Ok(ReplyData::DraftsList(DraftList { drafts, next_after }))
    }

    pub(super) fn inputs_list(
        &self,
        request_id: &Id,
        query: &InputsListQuery,
    ) -> Result<ReplyData, Rejection> {
        let st = self.obj(self.objective_id(&query.objective_id)?)?;
        let after = query.after.as_ref().map_or(0, Counter::value);
        let mut eligible: Vec<_> = st.inputs.iter().filter(|(seq, _)| *seq > after).collect();
        eligible.sort_by_key(|(seq, _)| *seq);
        let mut inputs = Vec::new();
        let mut content_bytes: usize = 0;
        let mut last = None;
        for (index, (seq, input)) in eligible
            .iter()
            .enumerate()
            .take(query.limit.value() as usize)
        {
            let wire = wire_input(input);
            let cursor = Counter::new(*seq);
            let more = index + 1 < eligible.len();
            let overhead = reply_json_len(
                request_id,
                ReplyData::InputsList(InputList {
                    inputs: Vec::new(),
                    next_after: more.then_some(cursor.clone()),
                }),
            );
            let next_content = content_bytes
                .saturating_add(usize::from(!inputs.is_empty()))
                .saturating_add(json_len(&wire));
            if overhead.saturating_add(next_content) > MAX_FRAME {
                break;
            }
            inputs.push(wire);
            content_bytes = next_content;
            last = Some(cursor);
        }
        if inputs.is_empty() && !eligible.is_empty() {
            return Err(history_too_large());
        }
        let next_after = (inputs.len() < eligible.len()).then_some(last).flatten();
        Ok(ReplyData::InputsList(InputList { inputs, next_after }))
    }

    fn check_draft_representation(
        &self,
        request_id: &Id,
        mut draft: WireDraft,
        cursor: DraftCursor,
    ) -> Result<(), Rejection> {
        draft.revision = Counter::new(u64::MAX);
        draft.state = WireDraftState::Submitted;
        draft.input_id = Some(request_id.clone());
        draft.stale = false;
        let fits_reply = |data| {
            encode(
                &ClientEnvelope::new(ClientBody::Reply(Reply::ok(request_id.clone(), data))),
                Framing::Client,
            )
            .is_ok()
        };
        let fits = fits_reply(ReplyData::DraftSave(draft.clone()))
            && fits_reply(ReplyData::DraftsList(DraftList {
                drafts: vec![draft.clone()],
                next_after: Some(cursor),
            }))
            && fits_reply(ReplyData::OperationGet(OperationStatus {
                state: OperationState::Completed,
                outcome: Some(Outcome::Ok(Box::new(ReplyData::DraftSave(draft.clone())))),
            }))
            && encode(
                &ClientEnvelope::new(ClientBody::Event(EventFrame {
                    service_epoch: self.epoch.clone(),
                    sequence: Counter::new(u64::MAX),
                    event: Event::DraftChanged(draft),
                })),
                Framing::Client,
            )
            .is_ok();
        if !fits {
            return Err(Rejection::new(
                ErrorCode::LimitReached,
                SafeAction::None,
                "This draft is too large to save. Shorten the comment or selection and try again.",
            ));
        }
        Ok(())
    }

    /// Checks that `reference` still points at something the client can comment on: its
    /// capture or candidate belongs to the Objective, its file version is stored, and the
    /// selected bytes still match.
    fn check_reference_current(
        &self,
        oid: ObjectiveId,
        st: &ObjState,
        reference: &Reference,
    ) -> Result<(), Rejection> {
        match reference.version() {
            InspectedVersion::Working { capture_id } => {
                let row = sql(storage::load_capture(self.store.conn(), capture_id))?;
                if row.is_none_or(|r| r.objective_id != oid) {
                    return Err(not_found("working capture"));
                }
            }
            InspectedVersion::Candidate { candidate_id } => {
                if st.review.candidate(candidate_id).is_none() {
                    return Err(not_found("candidate"));
                }
            }
        }
        let digest = ArtifactDigest::parse(reference.artifact_digest().hex())
            .map_err(|_| invalid("invalid artifact digest"))?;
        if !self.artifacts.contains(&digest) {
            return Err(Rejection::new(
                ErrorCode::NotFound,
                SafeAction::None,
                "The commented file version is no longer available. Open the file again.",
            ));
        }
        let range = reference.byte_range();
        let page = self
            .artifacts
            .read_page(
                &digest,
                range.start(),
                usize::try_from(range.byte_len()).unwrap_or(usize::MAX),
            )
            .map_err(artifact_rejection(
                "The commented file version couldn't be read. Open the file again.",
            ))?;
        if page.offset != range.start() || page.data != reference.selected_text().as_bytes() {
            return Err(invalid(
                "The selected text no longer matches the file. Select it again.",
            ));
        }
        Ok(())
    }

    pub(super) fn draft_save(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &DraftSaveCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        let draft_id = DraftId::new(parse_uuid(&cmd.draft_id)?);
        let reference = core_reference(&cmd.reference)?;
        let st = self.obj(oid)?;
        self.check_reference_current(oid, st, &reference)?;
        let existing = st.draft(draft_id).cloned();
        if existing.is_none()
            && self
                .objectives
                .values()
                .any(|other| other.draft(draft_id).is_some())
        {
            return Err(conflict("draft id belongs to another objective"));
        }
        let created = existing.is_none();
        let draft = save_draft(
            existing,
            st.objective(),
            SaveDraft {
                draft_id,
                objective_id: oid,
                expected_revision: Revision::new(cmd.expected_revision.value()),
                reference,
                body: cmd.body.clone(),
            },
            ts_now(),
        )
        .map_err(domain_rejection)?;
        let mut st = st.clone();
        match st.drafts.iter_mut().find(|d| d.id() == draft_id) {
            Some(slot) => *slot = draft.clone(),
            None => st.drafts.push(draft.clone()),
        }
        st.draft_stale.insert(draft_id, false);
        let wire = wire_draft(&st, &draft);
        self.check_draft_representation(
            &ctx.request_id,
            wire.clone(),
            DraftCursor {
                created_at: wire_ts(draft.record().created_at.unix_ms()),
                draft_id: wire.draft_id.clone(),
            },
        )?;
        let data = ReplyData::DraftSave(wire.clone());
        match fault::operation("draft.save") {
            Some(Fault::Refuse) => {
                return Err(durability_failure(
                    "The draft was not written (injected test fault).",
                ));
            }
            Some(Fault::Delay(delay)) => std::thread::sleep(delay),
            None => {}
        }
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_draft(tx, &draft, true)
        })?;
        self.objectives.insert(oid, st);
        self.emit(Event::DraftChanged(wire));
        if created {
            self.emit_objective(oid);
        }
        Ok(data)
    }

    pub(super) fn draft_delete(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &DraftDeleteCommand,
    ) -> Result<ReplyData, Rejection> {
        let draft_id = DraftId::new(parse_uuid(&cmd.draft_id)?);
        let (oid, draft) = self
            .objectives
            .iter()
            .find_map(|(oid, st)| st.draft(draft_id).map(|d| (*oid, d.clone())))
            .ok_or_else(|| not_found("draft"))?;
        draft
            .check_delete(Revision::new(cmd.expected_revision.value()))
            .map_err(domain_rejection)?;
        let data = ReplyData::DraftDelete(Empty {});
        let now = now_ms();
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::delete_draft(tx, draft_id, now)
        })?;
        if let Some(st) = self.objectives.get_mut(&oid) {
            st.drafts.retain(|d| d.id() != draft_id);
            st.draft_stale.remove(&draft_id);
        }
        self.emit_objective(oid);
        Ok(data)
    }

    fn acceptance(
        st: &ObjState,
        ctx: &ReqCtx,
        mode: lade_protocol::types::DeliveryMode,
        session: Option<(SessionId, Revision)>,
    ) -> Result<Acceptance, Rejection> {
        let operation = ctx
            .op
            .as_ref()
            .ok_or_else(|| invalid("operation id required"))?;
        let (session_id, association_revision) = if let Some(pair) = session {
            pair
        } else {
            let current =
                st.objective().execution().session().ok_or_else(|| {
                    domain_rejection(lade_core::DomainError::ExecutionUnavailable)
                })?;
            (current.session_id, current.association_revision)
        };
        Ok(Acceptance {
            input_id: InputId::new(new_uuid()),
            operation_id: OperationId::new(parse_uuid(&operation.id)?),
            session_id,
            association_revision,
            mode: core_mode(mode),
        })
    }

    fn accept_and_dispatch(
        &mut self,
        mut ctx: ReqCtx,
        mut st: ObjState,
        input: lade_core::domain::feedback::AcceptedInput,
        submitted: &[lade_core::domain::feedback::Draft],
        checked: bool,
    ) {
        let oid = st.objective().id();
        let seq = self.input_seq + 1;
        let summary = wire_input(&input);
        let data = input_reply(ctx.name, summary.clone());
        let committed = self.commit(Some(&mut ctx), Some(&data), |tx| {
            storage::insert_input(tx, &input, seq)?;
            for draft in submitted {
                storage::save_draft(tx, draft, false)?;
            }
            Ok(())
        });
        if let Err(rejection) = committed {
            self.finish(ctx, Err(rejection));
            return;
        }
        fault::point("input.accepted");
        self.input_seq = seq;
        for draft in submitted {
            if let Some(slot) = st.drafts.iter_mut().find(|d| d.id() == draft.id()) {
                *slot = draft.clone();
            }
        }
        let input_id = input.id;
        st.inputs.push((seq, input));
        let drafts: Vec<_> = submitted.iter().map(|d| wire_draft(&st, d)).collect();
        self.objectives.insert(oid, st);
        self.finish(ctx, Ok(data));
        self.emit(Event::InputChanged(summary));
        for draft in drafts {
            self.emit(Event::DraftChanged(draft));
        }
        self.dispatch_queued(oid, checked.then_some(input_id));
        self.emit_objective(oid);
    }

    pub(super) fn message_send(&mut self, ctx: ReqCtx, cmd: &MessageSendCommand) -> Flow {
        let result = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let st = self.obj(oid)?;
            let acceptance = Self::acceptance(st, &ctx, cmd.mode, None)?;
            let input = accept_message(st.objective(), acceptance, cmd.text.clone(), ts_now())
                .map_err(domain_rejection)?;
            Ok((st.clone(), input))
        })();
        match result {
            Ok((st, input)) => self.accept_and_dispatch(ctx, st, input, &[], true),
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
        Flow::Done
    }

    pub(super) fn feedback_submit(&mut self, ctx: ReqCtx, cmd: FeedbackSubmitCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let st = self.obj(oid)?;
            let session = SessionId::new(parse_uuid(&cmd.destination_session_id)?);
            st.objective()
                .execution()
                .check_association(session, Revision::new(cmd.association_revision.value()))
                .map_err(domain_rejection)?;
            if st.objective().lifecycle() != lade_core::domain::objectives::Lifecycle::Active {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    lade_core::domain::feedback::input_refusal(st.objective().lifecycle()),
                )));
            }
            let mut drafts = Vec::new();
            for entry in &cmd.drafts {
                let id = DraftId::new(parse_uuid(&entry.draft_id)?);
                let draft = st.draft(id).ok_or_else(|| not_found("draft"))?;
                if draft.revision().value() != entry.revision.value() {
                    return Err(conflict(
                        "A draft changed after you reviewed it. Refresh and try again.",
                    ));
                }
                if !draft.is_unsent() {
                    return Err(conflict("A draft was already submitted."));
                }
                drafts.push((id, draft.record().reference.clone()));
            }
            let worktree =
                st.row.worktree.clone().ok_or_else(|| {
                    domain_rejection(lade_core::DomainError::ExecutionUnavailable)
                })?;
            Ok((oid, self.capture_input(st, worktree, drafts)?))
        })();
        match prepared {
            Ok((oid, input)) => {
                self.spawn(move || Completed::Capture {
                    oid,
                    purpose: CapturePurpose::Submit(ctx, Box::new(cmd)),
                    result: jobs::capture(&input),
                });
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(super) fn submit_after_capture(
        &mut self,
        ctx: ReqCtx,
        cmd: &FeedbackSubmitCommand,
        out: &CaptureOut,
    ) {
        let result = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let st = self.obj(oid)?;
            let session = SessionId::new(parse_uuid(&cmd.destination_session_id)?);
            let acceptance = Self::acceptance(
                st,
                &ctx,
                cmd.mode,
                Some((session, Revision::new(cmd.association_revision.value()))),
            )?;
            let stale: HashMap<DraftId, bool> = out.stale.iter().copied().collect();
            let mut drafts = Vec::new();
            let mut revisions = Vec::new();
            for entry in &cmd.drafts {
                let id = DraftId::new(parse_uuid(&entry.draft_id)?);
                drafts.push(st.draft(id).ok_or_else(|| not_found("draft"))?.clone());
                revisions.push(Revision::new(entry.revision.value()));
            }
            let digests: Vec<CoreDigest> = drafts
                .iter()
                .map(|d| d.record().reference.artifact_digest().clone())
                .collect();
            let mut batch: Vec<BatchEntry<'_>> = drafts
                .iter_mut()
                .zip(revisions)
                .zip(&digests)
                .map(|((draft, revision), digest)| {
                    let current = if stale.get(&draft.id()).copied().unwrap_or(true) {
                        CurrentContent::Missing
                    } else {
                        CurrentContent::Present { digest, bytes: &[] }
                    };
                    BatchEntry {
                        draft,
                        revision,
                        current,
                    }
                })
                .collect();
            let input = accept_feedback(
                st.objective(),
                acceptance,
                &mut batch,
                cmd.note.clone(),
                ts_now(),
            )
            .map_err(domain_rejection)?;
            drop(batch);
            Ok((st.clone(), input, drafts))
        })();
        match result {
            Ok((st, input, drafts)) => {
                debug_assert!(
                    drafts
                        .iter()
                        .all(|d| matches!(d.record().state, DraftState::Submitted { .. }))
                );
                self.accept_and_dispatch(ctx, st, input, &drafts, true);
            }
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
    }
}

fn input_reply(
    name: lade_protocol::client::RequestName,
    summary: lade_protocol::client::InputSummary,
) -> ReplyData {
    match name {
        lade_protocol::client::RequestName::FeedbackSubmit => ReplyData::FeedbackSubmit(summary),
        _ => ReplyData::MessageSend(summary),
    }
}

#[cfg(test)]
mod tests {
    use lade_protocol::client::{
        DraftDeleteCommand, ExecutionStartCommand, InputsListQuery, Request,
    };
    use lade_protocol::types::{Counter, DeliveryMode, Model, PageLimit};

    use super::*;
    use crate::service::convert::wire_id;
    use crate::service::test_harness::Harness;
    use crate::supervision::WorkerEvent;

    fn start(harness: &mut Harness, id: &Id) {
        harness
            .call(Request::ExecutionStart(ExecutionStartCommand {
                objective_id: id.clone(),
                model: Model {
                    provider: "anthropic".into(),
                    id: "m".into(),
                },
                thinking: None,
            }))
            .expect("start");
    }

    fn send(harness: &mut Harness, id: &Id, text: &str) -> Result<ReplyData, Rejection> {
        harness.call(Request::MessageSend(MessageSendCommand {
            objective_id: id.clone(),
            text: text.into(),
            mode: DeliveryMode::Prompt,
        }))
    }

    fn delivered(log: &std::sync::Mutex<crate::service::test_harness::WorkerLog>) -> Vec<String> {
        log.lock()
            .expect("log")
            .sent
            .iter()
            .filter_map(|body| match body {
                lade_protocol::worker::WorkerBody::Deliver { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn messages_need_a_started_agent_and_wait_until_it_is_ready() {
        let mut harness = Harness::new("feedback-message");
        let (id, oid) = harness.create_objective("Message");
        let refused = send(&mut harness, &id, "too early").expect_err("not started");
        assert_eq!(
            refused.code,
            ErrorCode::ExecutionUnavailable,
            "no session to address"
        );

        start(&mut harness, &id);
        let Ok(ReplyData::MessageSend(summary)) = send(&mut harness, &id, "first") else {
            panic!("accepted");
        };
        assert_eq!(summary.text_preview, "first");
        let (_, log) = harness.last_worker();
        assert!(delivered(&log).is_empty(), "the worker is not ready yet");
        harness.worker_event(
            oid,
            WorkerEvent::Body(lade_protocol::worker::WorkerBody::Ready {
                pi_session_id: "pi".into(),
                session_file: "/s.jsonl".into(),
            }),
        );
        assert_eq!(delivered(&log), ["first"]);
        let input_id = harness.service.objectives[&oid].inputs[0].1.id;
        harness.worker_event(
            oid,
            WorkerEvent::Body(lade_protocol::worker::WorkerBody::Received {
                input_id: wire_id(input_id.uuid()),
            }),
        );
        assert!(matches!(
            harness.service.objectives[&oid].inputs[0].1.delivery,
            lade_core::domain::feedback::DeliveryState::Received { .. }
        ));
        send(&mut harness, &id, "second").expect("accepted while idle");
        assert_eq!(delivered(&log), ["first", "second"]);
        assert_eq!(harness.service.input_seq, 2);
        let stored = storage::load_inputs(harness.service.store.conn()).expect("inputs");
        assert_eq!(stored.len(), 2);
    }

    #[test]
    fn inputs_page_forward_from_a_sequence_cursor() {
        let mut harness = Harness::new("feedback-inputs");
        let (id, _) = harness.create_objective("Inputs");
        start(&mut harness, &id);
        for n in 0..3 {
            send(&mut harness, &id, &format!("message {n}")).expect("accepted");
        }
        let page = |harness: &mut Harness, after: Option<u64>| {
            let Ok(ReplyData::InputsList(list)) =
                harness.call(Request::InputsList(InputsListQuery {
                    objective_id: id.clone(),
                    limit: PageLimit::parse("2").expect("limit"),
                    after: after.map(Counter::new),
                }))
            else {
                panic!("inputs list");
            };
            list
        };
        let first = page(&mut harness, None);
        let texts = |list: &lade_protocol::client::InputList| {
            list.inputs
                .iter()
                .map(|i| i.text_preview.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&first), ["message 0", "message 1"]);
        let cursor = first.next_after.clone().expect("more inputs");
        let rest = page(&mut harness, Some(cursor.value()));
        assert_eq!(texts(&rest), ["message 2"]);
        assert!(rest.next_after.is_none());
    }

    #[test]
    fn deleting_an_unknown_draft_is_not_found() {
        let mut harness = Harness::new("feedback-drafts");
        harness.create_objective("Drafts");
        let missing = harness
            .call(Request::DraftDelete(DraftDeleteCommand {
                draft_id: wire_id(new_uuid()),
                expected_revision: Counter::new(1),
            }))
            .expect_err("unknown draft");
        assert_eq!(missing.code, ErrorCode::NotFound);
    }
}
